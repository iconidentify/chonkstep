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
	// Visible is false while the dock is hidden or this tile is
	// scrolled out of view; stop sampling as well as drawing.
	Visible bool

	fd int
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
		TilePx:    state.TilePx,
		TileUnits: d.TileUnits,
		Height:    state.TilePx * uint32(d.TileUnits),
		Scale:     state.Scale,
		ThemeID:   state.ThemeID,
		ThemeTOML: state.ThemeTOML,
		Visible:   visible,
		fd:        fd,
	}
	frame := make([]byte, int(ctx.TilePx)*int(ctx.Height)*4)
	if d.OnTheme != nil {
		d.OnTheme(ctx)
	}

	recv := make([]byte, MaxMessageBytes)
	var generation uint32
	mustPresent := true // the shell has nothing to show until frame one
	nextDraw := time.Now()
	for {
		now := time.Now()
		if ctx.Visible && (mustPresent || !now.Before(nextDraw)) {
			changed := d.Draw(ctx, frame)
			if changed || mustPresent {
				generation++
				msg, err := EncodeFrame(generation, ctx.TilePx, ctx.Height, frame)
				if err != nil {
					return "", state, 0, err
				}
				if err := send(fd, msg); err != nil {
					return "disconnected", state, 0, nil
				}
			}
			mustPresent = false
			nextDraw = now.Add(d.RedrawInterval)
		}

		deadline := nextDraw
		if !ctx.Visible {
			// While hidden, wake only for messages.
			deadline = time.Now().Add(d.RedrawInterval)
		}
		data, ok, err := recvDeadline(fd, recv, deadline)
		if err != nil {
			return "", state, 0, err
		}
		if !ok {
			continue
		}
		if len(data) == 0 {
			return "disconnected", state, 0, nil
		}
		msg, err := DecodeServer(data)
		if err != nil {
			// The two ends genuinely disagree about the protocol;
			// continuing would be guessing.
			return "disconnected", state, 0, nil
		}
		switch msg.Kind {
		case "welcome", "theme_changed":
			return "retheme", msg.Theme, 0, nil
		case "input":
			if d.OnInput != nil && d.OnInput(ctx, msg.Input) {
				mustPresent = true
			}
		case "visibility":
			becameVisible := msg.Visible && !ctx.Visible
			ctx.Visible = msg.Visible
			mustPresent = mustPresent || becameVisible
		case "ping":
			if err := send(fd, EncodePong(msg.Seq)); err != nil {
				return "disconnected", state, 0, nil
			}
		case "goodbye":
			if msg.Reason == GoodbyeShutdown {
				return "shutdown", state, 0, nil
			}
			return "refused", state, msg.Reason, nil
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
