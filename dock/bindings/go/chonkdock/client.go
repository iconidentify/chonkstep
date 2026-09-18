package chonkdock

// The dockapp client loop: connect, draw, retheme, reconnect. The Go
// mirror of crates/chonk-ui/src/dockapp.rs, with the same timings,
// because the timings are a contract with the shell rather than tuning
// knobs: Welcome within 2 s of Hello; on EOF, reconnect to the same
// stable socket path for 10 s (100 ms doubling to a 1 s cap) — the
// shell restarts without saying goodbye and readopts the survivor —
// then exit and let the shell's registry relaunch us.
//
// The socket is raw syscalls rather than net.Conn: Go's net package
// does not speak SOCK_SEQPACKET, and SEQPACKET is not negotiable — it
// is what deletes message framing from the protocol.

import (
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"syscall"
	"time"
)

// Environment the shell sets when it launches a dockapp.
const (
	EnvSocket = "CHONKSTEP_DOCK_SOCKET"
	EnvToken  = "CHONKSTEP_DOCK_TOKEN"
)

// The contract's timings; see the package comment.
const (
	HandshakeTimeout      = 2 * time.Second
	ReconnectWindow       = 10 * time.Second
	ReconnectFirstDelay   = 100 * time.Millisecond
	ReconnectMaxDelay     = time.Second
	DefaultRedrawInterval = time.Second
)

// ErrPanelsUnsupported is returned by Ctx.OpenPanel on a protocol-1
// shell: one that predates instrument panels, and would treat an
// OpenPanel as a protocol error and close the whole connection — the
// tile would die with the panel. The SDK refuses locally instead.
var ErrPanelsUnsupported = errors.New("chonkdock: this shell predates instrument panels (protocol 1); OpenPanel needs a protocol-2 shell")

// ErrRefused wraps the Goodbye reason when the shell declines or ends
// a connection deliberately.
type ErrRefused struct{ Reason GoodbyeReason }

func (e *ErrRefused) Error() string {
	return fmt.Sprintf("chonkdock: the shell closed this dockapp's connection: %s", e.Reason)
}

// Ctx is everything a dockapp is told about its surroundings. It is
// rebuilt whenever the shell sends a ThemeChanged — which is how a
// theme switch or scale change reaches a dockapp without restarting it.
type Ctx struct {
	// TilePx is the device pixels along one tile edge (the frame's
	// width); Height is TilePx * TileUnits (the frame's height).
	TilePx    uint32
	TileUnits uint8
	Height    uint32
	// Scale is the session's scale factor, for sizing hand-drawn
	// geometry.
	Scale float32
	// ThemeID names the active theme; ThemeTOML is the serialized
	// palette (possibly empty). Use either, or neither — wrong colors
	// beat a blank tile.
	ThemeID   string
	ThemeTOML string
	// ShellProto is the shell's protocol version (from
	// Welcome/ThemeChanged). 1 has tiles only; instrument panels need
	// 2. Feature-gate any panel affordance on this rather than
	// finding out the hard way.
	ShellProto uint16
	// Visible is false while the dock is hidden or this tile is
	// scrolled out of view; stop sampling as well as drawing.
	Visible bool

	fd  int
	app *Dockapp
}

// Panel is one instrument panel: a larger popup surface the shell
// places near this dockapp's tile. Obtained from Ctx.OpenPanel; one
// per dockapp. The lifecycle is asynchronous on purpose — the shell
// answers an open request with a grant (possibly clamped) or a
// refusal, and can dismiss the panel at any time — and the SDK
// enforces the ordering the wire demands: no frame leaves before the
// grant, and every frame is exactly the granted size.
type Panel struct {
	// Width and Height are the *granted* size in device pixels, zero
	// until the shell's PanelOpened arrives. Draw at this size, never
	// at the size you asked for.
	Width, Height uint32
	// Reason says why the panel went away, once Closed() is true.
	Reason PanelCloseReason

	reqW, reqH  uint32
	opened      bool
	closing     bool
	closed      bool
	mustPresent bool
	buf         []byte
	generation  uint32
	fd          int
}

// Opened reports whether the shell's grant has arrived (and the panel
// is not mid-renegotiation or closed). Until then no frame may cross
// the wire, and Draw returns an error rather than letting you
// protocol-error.
func (p *Panel) Opened() bool { return p.opened }

// Closed reports whether the panel is gone, for any reason.
func (p *Panel) Closed() bool { return p.closed }

// Requested returns the size last asked for, which the grant may have
// clamped.
func (p *Panel) Requested() (width, height uint32) { return p.reqW, p.reqH }

// Draw pushes one full repaint, for panels not driven by the
// DrawPanel callback: premultiplied RGBA8, top row first, exactly
// Width*Height*4 bytes. The SDK slices it into maximal legal bands
// and streams them under one generation — callers never think in
// bands. It fails before the grant has arrived and after the panel
// closed. Bands are sent with a bounded wait (see
// PanelBandSendTimeout) rather than the tile's drop-on-EAGAIN,
// because bands do not supersede each other.
func (p *Panel) Draw(pixels []byte) error {
	if err := p.checkStreamable(); err != nil {
		return err
	}
	if expected := int(p.Width) * int(p.Height) * 4; len(pixels) != expected {
		return fmt.Errorf("chonkdock: panel frame needs %d bytes for the granted %dx%d, got %d", expected, p.Width, p.Height, len(pixels))
	}
	return p.streamBands(0, pixels)
}

// DrawRows pushes a partial update — rows y.. of the panel, for
// hover-highlight economy. pixels is premultiplied RGBA8, a whole
// number of Width-wide rows, and y plus that row count must stay
// within the granted height. Same grant and lifecycle rules as Draw.
func (p *Panel) DrawRows(y uint32, pixels []byte) error {
	if err := p.checkStreamable(); err != nil {
		return err
	}
	stride := int(p.Width) * 4
	if len(pixels) == 0 || len(pixels)%stride != 0 {
		return fmt.Errorf("chonkdock: partial update must be whole %dpx rows (%d bytes each), got %d bytes", p.Width, stride, len(pixels))
	}
	rows := uint32(len(pixels) / stride)
	if uint64(y)+uint64(rows) > uint64(p.Height) {
		return fmt.Errorf("chonkdock: rows %d..%d fall outside the granted height %d", y, y+rows, p.Height)
	}
	return p.streamBands(y, pixels)
}

func (p *Panel) checkStreamable() error {
	if p.closed {
		return errors.New("chonkdock: this panel is closed")
	}
	if !p.opened {
		return errors.New("chonkdock: the shell has not granted this panel yet; frames before PanelOpened are a protocol error")
	}
	return nil
}

// errBandStalled means the shell stopped taking panel bands within
// the bounded wait; the update should be retried whole, later.
var errBandStalled = errors.New("chonkdock: the shell stopped taking panel bands (send timed out)")

// PanelBandSendTimeout is how long one panel band send may wait for
// socket space. Unlike tile frames, bands do not supersede each other
// — a dropped band is a stale stripe, not a skipped frame — so they
// are sent with a bounded wait instead of drop-on-EAGAIN. A healthy
// shell drains its socket far faster than this.
const PanelBandSendTimeout = time.Second

// sendBand is the bounded-wait send bands need: blocking, with
// SO_SNDTIMEO, so a momentarily full buffer waits for the shell to
// drain rather than holing the repaint.
func sendBand(fd int, msg []byte) error {
	tv := syscall.NsecToTimeval(PanelBandSendTimeout.Nanoseconds())
	_ = syscall.SetsockoptTimeval(fd, syscall.SOL_SOCKET, syscall.SO_SNDTIMEO, &tv)
	for {
		_, err := syscall.SendmsgN(fd, msg, nil, nil, syscall.MSG_NOSIGNAL)
		switch err {
		case nil:
			return nil
		case syscall.EINTR:
			continue
		case syscall.EAGAIN: // == EWOULDBLOCK on Linux
			return errBandStalled // SO_SNDTIMEO elapsed
		default:
			return fmt.Errorf("chonkdock: send panel band: %w", err)
		}
	}
}

// streamBands sends one update top-to-bottom as bands that each fit a
// datagram, sharing one generation. Returns errBandStalled when the
// shell stopped taking them; callers retry the update rather than
// treating that as a dead connection.
func (p *Panel) streamBands(y uint32, pixels []byte) error {
	stride := int(p.Width) * 4
	totalRows := uint32(len(pixels) / stride)
	perBand := PanelBandRows(p.Width)
	p.generation++
	for row := uint32(0); row < totalRows; {
		rows := perBand
		if totalRows-row < rows {
			rows = totalRows - row
		}
		band := pixels[int(row)*stride : int(row+rows)*stride]
		msg, err := EncodePanelFrame(p.generation, y+row, rows, p.Width, band)
		if err != nil {
			return err
		}
		if err := sendBand(p.fd, msg); err != nil {
			return err
		}
		row += rows
	}
	return nil
}

// Close asks the shell to take the panel down; OnPanelClosed fires
// with PanelClosedByClient when the shell confirms. Safe to call
// twice.
func (p *Panel) Close() {
	if p.closed || p.closing {
		return
	}
	p.closing = true
	p.opened = false
	_, _ = syscall.SendmsgN(p.fd, EncodeClosePanel(), nil, nil, syscall.MSG_NOSIGNAL)
}

// Panel returns the current instrument panel (requested or open), or
// nil.
func (c *Ctx) Panel() *Panel { return c.app.panel }

// OpenPanel requests an instrument panel of width x height device
// pixels (at most MaxPanelPx per edge). It returns the handle
// immediately; the shell's answer arrives later — either a grant
// (Opened becomes true at the possibly-clamped Width x Height, and
// OnPanelOpened fires) or a refusal (OnPanelClosed with PanelRefused).
// Called again while the panel is open, it renegotiates the size on
// the same handle; frames pause until the new grant.
//
// A shell predating panels treats the request as a protocol error and
// closes the whole connection.
func (c *Ctx) OpenPanel(width, height uint32) (*Panel, error) {
	d := c.app
	if d.shellProto < 2 {
		return nil, ErrPanelsUnsupported
	}
	if !PanelFits(width, height) {
		return nil, fmt.Errorf("chonkdock: panel geometry %dx%d is out of range (at most %d per edge)", width, height, MaxPanelPx)
	}
	p := d.panel
	if p != nil && p.closed {
		p = nil
	}
	if p != nil && p.closing {
		// The old panel's PanelClosed is still in flight; SEQPACKET
		// ordering guarantees it arrives before the new grant, so park
		// it and attribute the next PanelClosed to it.
		d.retired = append(d.retired, p)
		p = nil
	}
	if p == nil {
		p = &Panel{reqW: width, reqH: height, fd: c.fd}
		d.panel = p
	} else {
		// Renegotiation: same handle, frames blocked until the fresh
		// grant — a frame at the old size could otherwise race the
		// shell's re-grant and be rejected as mismatched.
		p.reqW, p.reqH = width, height
		p.opened = false
	}
	msg, err := EncodeOpenPanel(width, height)
	if err != nil {
		return nil, err
	}
	if _, err := syscall.SendmsgN(c.fd, msg, nil, nil, syscall.MSG_NOSIGNAL); err != nil {
		return nil, fmt.Errorf("chonkdock: send OpenPanel: %w", err)
	}
	return p, nil
}

// Log says something in the shell's journal (a dockapp's stdout and
// stderr are /dev/null). Best-effort and non-blocking: a diagnostic
// that could block a redraw would be worse than no diagnostic.
func (c *Ctx) Log(level uint8, text string) {
	if msg, err := EncodeLog(level, text); err == nil {
		_, _ = syscall.SendmsgN(c.fd, msg, nil, nil, syscall.MSG_DONTWAIT|syscall.MSG_NOSIGNAL)
	}
}

// Dockapp runs one dock tile. Fill in Draw (required) and optionally
// OnInput and OnTheme, then call Run.
type Dockapp struct {
	// ID must match the id in the .dockapp registration that declared
	// this program, or the shell has no slot to give it.
	ID string
	// TileUnits is how many stacked square tiles to ask for (1-4).
	// Zero means one.
	TileUnits uint8
	// RedrawInterval is how often Draw is called — a ceiling on
	// effort, not a frame rate: a Draw that returns false sends
	// nothing. Zero means one second.
	RedrawInterval time.Duration
	// Wants selects which pointer events to receive (Want* bits).
	// Zero means WantAll.
	Wants uint8

	// Draw paints the tile into buf — premultiplied RGBA8, top row
	// first, TilePx*Height*4 bytes — and returns whether anything
	// changed.
	Draw func(ctx *Ctx, buf []byte) bool
	// OnInput receives one pointer event in tile-local coordinates and
	// returns whether it wants an immediate repaint. The dock reserves
	// middle and right click for itself, so only Left and Scroll ever
	// arrive. Optional.
	OnInput func(ctx *Ctx, ev InputEvent) bool
	// OnTheme is called after each successful handshake and theme
	// change, before the next Draw. Optional.
	OnTheme func(ctx *Ctx)

	// DrawPanel is Draw's sibling for the instrument panel: called on
	// the redraw cadence once the panel is open, with buf a
	// premultiplied-RGBA8 buffer of the granted Width*Height*4 bytes;
	// return whether anything changed. Optional (push frames with
	// Panel.Draw instead).
	DrawPanel func(ctx *Ctx, p *Panel, buf []byte) bool
	// OnPanelOpened fires when the shell's grant arrives — useful for
	// push-style drawing. Optional.
	OnPanelOpened func(ctx *Ctx, p *Panel)
	// OnPanelInput receives one pointer event in panel-local device
	// pixels and returns whether it wants an immediate panel repaint.
	// Optional.
	OnPanelInput func(ctx *Ctx, p *Panel, ev InputEvent) bool
	// OnPanelClosed fires when the panel is gone, for any reason:
	// PanelClosedByClient (you asked), PanelDismissed (the user
	// clicked away), PanelShutdown (the shell is going away, or the
	// connection dropped), PanelRefused (the open request was declined
	// and the panel never existed). The handle is dead afterwards;
	// open a fresh one. Optional.
	OnPanelClosed func(ctx *Ctx, p *Panel, reason PanelCloseReason)

	panel      *Panel
	retired    []*Panel // closed by us, PanelClosed confirmation pending
	shellProto uint16   // what the last Welcome/ThemeChanged said
}

func (d *Dockapp) grantPanel(ctx *Ctx, w, h uint32) {
	p := d.panel
	if p == nil || p.closed || p.closing {
		return // a grant that crossed our ClosePanel; already gone
	}
	p.Width, p.Height = w, h
	p.opened = true
	p.buf = make([]byte, int(w)*int(h)*4)
	p.mustPresent = true // the shell has nothing to show yet
	if d.OnPanelOpened != nil {
		d.OnPanelOpened(ctx, p)
	}
}

func (d *Dockapp) finishPanel(ctx *Ctx, reason PanelCloseReason) {
	var p *Panel
	if len(d.retired) > 0 {
		p, d.retired = d.retired[0], d.retired[1:]
	} else {
		p, d.panel = d.panel, nil
	}
	if p == nil || p.closed {
		return
	}
	p.closed = true
	p.opened = false
	p.Reason = reason
	if d.OnPanelClosed != nil {
		d.OnPanelClosed(ctx, p, reason)
	}
}

// dropPanel: the connection is gone; whatever panel it carried is
// too. The shell could not tell us, so synthesize the close locally —
// a dockapp should not have to special-case an EOF to learn its panel
// died.
func (d *Dockapp) dropPanel(ctx *Ctx) {
	for len(d.retired) > 0 {
		// These were closed by us; only the confirmation was lost.
		d.finishPanel(ctx, PanelClosedByClient)
	}
	if p := d.panel; p != nil {
		reason := PanelShutdown
		if p.closing {
			reason = PanelClosedByClient
		}
		d.finishPanel(ctx, reason)
	}
}

// Run connects to the dock and serves until the shell says Shutdown
// (returns nil) or refuses us (returns an error, *ErrRefused for a
// deliberate Goodbye).
func (d *Dockapp) Run() error {
	if d.Draw == nil {
		return errors.New("chonkdock: Dockapp.Draw is required")
	}
	if !IsValidID(d.ID) {
		return fmt.Errorf("chonkdock: invalid dockapp id %q", d.ID)
	}
	if d.TileUnits == 0 {
		d.TileUnits = 1
	}
	if d.RedrawInterval == 0 {
		d.RedrawInterval = DefaultRedrawInterval
	}
	if d.Wants == 0 {
		d.Wants = WantAll
	}

	path, token, err := connectionDetails()
	if err != nil {
		return err
	}
	fd, err := connect(path)
	if err != nil {
		return err
	}
	defer func() { syscall.Close(fd) }()

	state, err := d.handshake(fd, token)
	if err != nil {
		return err
	}
	visible := true
	for {
		outcome, next, reason, err := d.serve(fd, state, visible)
		if err != nil {
			return err
		}
		switch outcome {
		case "shutdown":
			return nil
		case "refused":
			return &ErrRefused{Reason: reason}
		case "retheme":
			// The same socket, a new palette: a theme switch is
			// invisible to this dockapp's own state.
			state = next
		case "disconnected":
			syscall.Close(fd)
			fd = reconnect(path)
			if fd < 0 {
				return nil // the registry relaunches us when the shell is back
			}
			if state, err = d.handshake(fd, token); err != nil {
				return err
			}
			visible = true // a fresh shell welcomes a tile it intends to show
		}
	}
}

func connectionDetails() (string, []byte, error) {
	path := os.Getenv(EnvSocket)
	if path == "" {
		return "", nil, fmt.Errorf("chonkdock: %s is not set: a dockapp is launched by the dock, not run from a shell", EnvSocket)
	}
	token, err := hex.DecodeString(os.Getenv(EnvToken))
	if err != nil || len(token) != TokenBytes {
		return "", nil, fmt.Errorf("chonkdock: %s is not 32 hex digits", EnvToken)
	}
	return path, token, nil
}

func connect(path string) (int, error) {
	fd, err := syscall.Socket(syscall.AF_UNIX, syscall.SOCK_SEQPACKET|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		return -1, fmt.Errorf("chonkdock: socket: %w", err)
	}
	// Widen the buffers so a whole tile fits in one datagram with room
	// to queue a few. Best-effort, exactly like the reference: the
	// message cap sits below even the un-widened kernel floor.
	_ = syscall.SetsockoptInt(fd, syscall.SOL_SOCKET, syscall.SO_SNDBUF, 2*MaxMessageBytes)
	_ = syscall.SetsockoptInt(fd, syscall.SOL_SOCKET, syscall.SO_RCVBUF, 2*MaxMessageBytes)
	if err := syscall.Connect(fd, &syscall.SockaddrUnix{Name: path}); err != nil {
		syscall.Close(fd)
		return -1, fmt.Errorf("chonkdock: connect %s: %w", path, err)
	}
	return fd, nil
}

// recvDeadline reads one whole message, waiting at most until
// deadline. Returns (nil, false) on timeout; an empty non-nil slice
// means EOF.
func recvDeadline(fd int, buf []byte, deadline time.Time) ([]byte, bool, error) {
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return nil, false, nil
		}
		tv := syscall.NsecToTimeval(remaining.Nanoseconds())
		_ = syscall.SetsockoptTimeval(fd, syscall.SOL_SOCKET, syscall.SO_RCVTIMEO, &tv)
		n, err := syscall.Read(fd, buf)
		if err != nil {
			if err == syscall.EAGAIN || err == syscall.EWOULDBLOCK || err == syscall.EINTR {
				continue
			}
			if err == syscall.ECONNRESET {
				return buf[:0], true, nil
			}
			return nil, false, fmt.Errorf("chonkdock: recv: %w", err)
		}
		return buf[:n], true, nil
	}
}

// send is the drop-rather-than-block send: MSG_DONTWAIT so a shell
// that is momentarily behind costs us a frame, never a stall, and
// MSG_NOSIGNAL so a dying shell is an EPIPE rather than a SIGPIPE.
func send(fd int, msg []byte) error {
	_, err := syscall.SendmsgN(fd, msg, nil, nil, syscall.MSG_DONTWAIT|syscall.MSG_NOSIGNAL)
	if err == syscall.EAGAIN || err == syscall.EWOULDBLOCK {
		return nil // the next frame supersedes this one anyway
	}
	return err
}

func (d *Dockapp) handshake(fd int, token []byte) (ThemeState, error) {
	var state ThemeState
	hello, err := EncodeHello(d.ID, d.TileUnits, token, d.Wants)
	if err != nil {
		return state, err
	}
	if _, err := syscall.SendmsgN(fd, hello, nil, nil, syscall.MSG_NOSIGNAL); err != nil {
		return state, fmt.Errorf("chonkdock: send Hello: %w", err)
	}
	buf := make([]byte, MaxMessageBytes)
	data, ok, err := recvDeadline(fd, buf, time.Now().Add(HandshakeTimeout))
	if err != nil {
		return state, err
	}
	if !ok {
		return state, errors.New("chonkdock: no Welcome from the shell")
	}
	if len(data) == 0 {
		return state, errors.New("chonkdock: the shell closed the connection during the handshake")
	}
	msg, err := DecodeServer(data)
	if err != nil {
		return state, err
	}
	switch msg.Kind {
	case "welcome":
		if !FrameFits(msg.Theme.TilePx, d.TileUnits) {
			return state, fmt.Errorf("chonkdock: a %dpx x %d-unit tile cannot cross the socket", msg.Theme.TilePx, d.TileUnits)
		}
		return msg.Theme, nil
	case "goodbye":
		return state, &ErrRefused{Reason: msg.Reason}
	default:
		return state, fmt.Errorf("chonkdock: expected Welcome, got %s", msg.Kind)
	}
}

// serve runs one connection's worth of event loop, returning on a
// theme change so the Ctx and the buffer are rebuilt in one place.
func (d *Dockapp) serve(fd int, state ThemeState, visible bool) (string, ThemeState, GoodbyeReason, error) {
	if !FrameFits(state.TilePx, d.TileUnits) {
		return "", state, 0, fmt.Errorf("chonkdock: a %dpx x %d-unit tile cannot cross the socket", state.TilePx, d.TileUnits)
	}
	ctx := &Ctx{
		TilePx:     state.TilePx,
		TileUnits:  d.TileUnits,
		Height:     state.TilePx * uint32(d.TileUnits),
		Scale:      state.Scale,
		ThemeID:    state.ThemeID,
		ThemeTOML:  state.ThemeTOML,
		ShellProto: state.Proto,
		Visible:    visible,
		fd:         fd,
		app:        d,
	}
	d.shellProto = state.Proto
	frame := make([]byte, int(ctx.TilePx)*int(ctx.Height)*4)
	if d.OnTheme != nil {
		d.OnTheme(ctx)
	}

	// Every outcome except a retheme ends this connection, and a
	// panel does not outlive its connection.
	done := func(outcome string, reason GoodbyeReason, err error) (string, ThemeState, GoodbyeReason, error) {
		d.dropPanel(ctx)
		return outcome, state, reason, err
	}

	recv := make([]byte, MaxMessageBytes)
	var generation uint32
	mustPresent := true // the shell has nothing to show until frame one
	nextDraw := time.Now()
	for {
		now := time.Now()
		due := !now.Before(nextDraw)
		if ctx.Visible && (mustPresent || due) {
			changed := d.Draw(ctx, frame)
			if changed || mustPresent {
				generation++
				msg, err := EncodeFrame(generation, ctx.TilePx, ctx.Height, frame)
				if err != nil {
					return done("", 0, err)
				}
				if err := send(fd, msg); err != nil {
					return done("disconnected", 0, nil)
				}
			}
			mustPresent = false
			nextDraw = now.Add(d.RedrawInterval)
		}
		// The panel paints on the same cadence as the tile — a
		// sibling, not a second clock. It is not gated on tile
		// visibility: an open panel is on screen by definition.
		if p := d.panel; p != nil && p.opened && (p.mustPresent || due) {
			stalled := false
			if d.DrawPanel != nil {
				changed := d.DrawPanel(ctx, p, p.buf)
				if changed || p.mustPresent {
					if err := p.streamBands(0, p.buf); err != nil {
						if !errors.Is(err, errBandStalled) {
							return done("disconnected", 0, nil)
						}
						// A stalled shell is behind, not gone: retry
						// the whole repaint next tick.
						stalled = true
					}
				}
			}
			p.mustPresent = stalled
			if due && !ctx.Visible {
				nextDraw = now.Add(d.RedrawInterval)
			}
		}

		deadline := nextDraw
		if p := d.panel; !ctx.Visible && (p == nil || !p.opened) {
			// While hidden with no panel, wake only for messages.
			deadline = time.Now().Add(d.RedrawInterval)
		}
		data, ok, err := recvDeadline(fd, recv, deadline)
		if err != nil {
			return done("", 0, err)
		}
		if !ok {
			continue
		}
		if len(data) == 0 {
			return done("disconnected", 0, nil)
		}
		msg, err := DecodeServer(data)
		if err != nil {
			// The two ends genuinely disagree about the protocol;
			// continuing would be guessing.
			return done("disconnected", 0, nil)
		}
		switch msg.Kind {
		case "welcome", "theme_changed":
			// Same connection, new palette: the panel (if any) stays
			// open across a retheme.
			return "retheme", msg.Theme, 0, nil
		case "input":
			if d.OnInput != nil && d.OnInput(ctx, msg.Input) {
				mustPresent = true
			}
		case "panel_opened":
			d.grantPanel(ctx, msg.PanelW, msg.PanelH)
		case "panel_closed":
			d.finishPanel(ctx, msg.PanelReason)
		case "panel_input":
			if p := d.panel; p != nil && p.opened && d.OnPanelInput != nil {
				if d.OnPanelInput(ctx, p, msg.Input) {
					p.mustPresent = true
				}
			}
		case "visibility":
			becameVisible := msg.Visible && !ctx.Visible
			ctx.Visible = msg.Visible
			mustPresent = mustPresent || becameVisible
		case "ping":
			if err := send(fd, EncodePong(msg.Seq)); err != nil {
				return done("disconnected", 0, nil)
			}
		case "goodbye":
			if msg.Reason == GoodbyeShutdown {
				return done("shutdown", 0, nil)
			}
			return done("refused", msg.Reason, nil)
		}
	}
}

// reconnect retries connect() against the stable socket path for the
// shell-restart window; a negative fd means the window elapsed.
func reconnect(path string) int {
	deadline := time.Now().Add(ReconnectWindow)
	delay := ReconnectFirstDelay
	for time.Now().Before(deadline) {
		time.Sleep(delay)
		if fd, err := connect(path); err == nil {
			return fd
		}
		delay *= 2
		if delay > ReconnectMaxDelay {
			delay = ReconnectMaxDelay
		}
	}
	return -1
}
