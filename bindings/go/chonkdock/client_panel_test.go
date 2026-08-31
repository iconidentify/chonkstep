package chonkdock

// Instrument-panel lifecycle tests, headless: a fake shell over a
// *real* SOCK_SEQPACKET socket (the chonk-switch test pattern,
// extended to the panel half of the protocol), against a real Dockapp
// running its real event loop in a goroutine. The shell's half of the
// wire is written out independently here with encoding/binary rather
// than borrowed from the SDK's encoders, so a codec bug cannot vouch
// for itself.

import (
	"bytes"
	"encoding/binary"
	"errors"
	"math"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

const (
	testTile  = 56
	panelFill = "\x10\x20\x30\xff"
)

var testToken = func() []byte {
	tok := make([]byte, 16)
	for i := range tok {
		tok[i] = byte(i)
	}
	return tok
}()

// -- the shell's half of the wire, written independently --------------

func shellWelcome(tilePx uint32, scale float32, proto uint16) []byte {
	// The u16 that was reserved in protocol 1 carries the shell's
	// protocol version; a pre-panel shell sends 0 there.
	id := "nextstep-classic"
	out := []byte{0x81, 0, 0, 0}
	out = binary.LittleEndian.AppendUint32(out, tilePx)
	out = binary.LittleEndian.AppendUint32(out, math.Float32bits(scale))
	out = binary.LittleEndian.AppendUint16(out, uint16(len(id)))
	out = binary.LittleEndian.AppendUint16(out, proto)
	out = binary.LittleEndian.AppendUint32(out, 0)
	return append(out, id...)
}

func shellInput(kind byte, x, y int32, button byte, delta int32) []byte {
	out := []byte{0x83, 0, 0, 0, kind, button, 0, 0}
	out = binary.LittleEndian.AppendUint32(out, uint32(x))
	out = binary.LittleEndian.AppendUint32(out, uint32(y))
	return binary.LittleEndian.AppendUint32(out, uint32(delta))
}

func shellPanelInput(kind byte, x, y int32, button byte, delta int32) []byte {
	msg := shellInput(kind, x, y, button, delta)
	msg[0] = 0x89
	return msg
}

func shellPanelOpened(w, h uint32) []byte {
	out := []byte{0x87, 0, 0, 0}
	out = binary.LittleEndian.AppendUint32(out, w)
	return binary.LittleEndian.AppendUint32(out, h)
}

func shellPanelClosed(reason byte) []byte {
	// reason u8 + 3 reserved zeros, the Goodbye/Visibility convention.
	return []byte{0x88, 0, 0, 0, reason, 0, 0, 0}
}

func shellGoodbye(reason byte) []byte {
	return []byte{0x86, 0, 0, 0, reason, 0, 0, 0}
}

type clientMsg struct {
	kind     string // "hello" | "frame" | "pong" | "log" | "open_panel" | "panel_frame" | "close_panel"
	w, h     uint32
	gen      uint32 // panel_frame: the shared repaint generation
	y        uint32 // panel_frame: first row of the band
	bandRows uint32 // panel_frame: rows in the band
	pixels   []byte
	openW    uint32
	openH    uint32
	helloID  string
}

func decClient(t *testing.T, buf []byte) clientMsg {
	t.Helper()
	if len(buf) < 4 || buf[1] != 0 || buf[2] != 0 || buf[3] != 0 {
		t.Fatalf("bad client header % x", buf[:min(len(buf), 4)])
	}
	body := buf[4:]
	switch buf[0] {
	case 0x01:
		idLen := int(body[6])
		if len(body) != 24+idLen {
			t.Fatalf("Hello is %d body bytes for id_len %d", len(body), idLen)
		}
		if !bytes.Equal(body[8:24], testToken) {
			t.Fatalf("Hello token = % x", body[8:24])
		}
		return clientMsg{kind: "hello", helloID: string(body[24:])}
	case 0x02:
		w := binary.LittleEndian.Uint32(body[4:8])
		h := binary.LittleEndian.Uint32(body[8:12])
		if len(body) != 12+int(w)*int(h)*4 {
			t.Fatalf("Frame length disagrees with %dx%d", w, h)
		}
		return clientMsg{kind: "frame", w: w, h: h, pixels: body[12:]}
	case 0x03:
		return clientMsg{kind: "pong"}
	case 0x04:
		return clientMsg{kind: "log"}
	case 0x05:
		if len(body) != 8 {
			t.Fatalf("OpenPanel body must be exactly 8 bytes, got %d", len(body))
		}
		return clientMsg{kind: "open_panel",
			openW: binary.LittleEndian.Uint32(body[0:4]),
			openH: binary.LittleEndian.Uint32(body[4:8])}
	case 0x06: // one BAND: generation, y, band_height, width, pixels
		gen := binary.LittleEndian.Uint32(body[0:4])
		y := binary.LittleEndian.Uint32(body[4:8])
		bh := binary.LittleEndian.Uint32(body[8:12])
		w := binary.LittleEndian.Uint32(body[12:16])
		if len(body) != 16+int(w)*int(bh)*4 {
			t.Fatalf("PanelFrame band length disagrees with %dx%d", w, bh)
		}
		if int(w)*int(bh)*4 > MaxFrameBytes {
			t.Fatalf("a %dx%d band does not fit MaxFrameBytes", w, bh)
		}
		return clientMsg{kind: "panel_frame", gen: gen, y: y, bandRows: bh, w: w, pixels: body[16:]}
	case 0x07:
		if len(body) != 0 {
			t.Fatalf("ClosePanel carries no body, got %d bytes", len(body))
		}
		return clientMsg{kind: "close_panel"}
	}
	t.Fatalf("unexpected client message kind %#x", buf[0])
	return clientMsg{}
}

// -- the harness ------------------------------------------------------

type appEvent struct {
	kind   string // "opened" | "closed" | "panel_input" | "error" | "returned"
	w, h   uint32
	reason PanelCloseReason
	input  InputEvent
	err    error
}

type harness struct {
	t      *testing.T
	fd     int // the shell's side of the accepted connection
	app    *Dockapp
	events chan appEvent
	done   chan error
	// scripted behavior for the next tile press, run in the app's loop
	onPress func(ctx *Ctx)
}

func newHarness(t *testing.T) *harness { return newHarnessProto(t, 2) }

func newHarnessProto(t *testing.T, shellProto uint16) *harness {
	t.Helper()
	dir := t.TempDir()
	sockPath := filepath.Join(dir, "dock.sock")
	lfd, err := syscall.Socket(syscall.AF_UNIX, syscall.SOCK_SEQPACKET|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		t.Fatal(err)
	}
	if err := syscall.Bind(lfd, &syscall.SockaddrUnix{Name: sockPath}); err != nil {
		t.Fatal(err)
	}
	if err := syscall.Listen(lfd, 1); err != nil {
		t.Fatal(err)
	}
	t.Setenv(EnvSocket, sockPath)
	t.Setenv(EnvToken, "000102030405060708090a0b0c0d0e0f")

	h := &harness{t: t, events: make(chan appEvent, 64), done: make(chan error, 1)}
	h.app = &Dockapp{
		ID:             "panel-probe",
		RedrawInterval: 50 * time.Millisecond,
		// The tile never changes after its mandatory first frame, so
		// the tile half of the wire goes quiet and panel traffic is
		// easy to see.
		Draw: func(ctx *Ctx, buf []byte) bool { return false },
		OnInput: func(ctx *Ctx, ev InputEvent) bool {
			if ev.Kind == InputPress && h.onPress != nil {
				h.onPress(ctx)
			}
			return false
		},
		DrawPanel: func(ctx *Ctx, p *Panel, buf []byte) bool {
			for i := 0; i < len(buf); i += 4 {
				copy(buf[i:], panelFill)
			}
			return false // unchanged; mustPresent drives the sends
		},
		OnPanelOpened: func(ctx *Ctx, p *Panel) {
			h.events <- appEvent{kind: "opened", w: p.Width, h: p.Height}
		},
		OnPanelInput: func(ctx *Ctx, p *Panel, ev InputEvent) bool {
			h.events <- appEvent{kind: "panel_input", input: ev}
			return ev.Kind == InputPress // a press requests a repaint
		},
		OnPanelClosed: func(ctx *Ctx, p *Panel, reason PanelCloseReason) {
			h.events <- appEvent{kind: "closed", reason: reason}
		},
	}
	go func() { h.done <- h.app.Run() }()

	// Accept the probe's connection and run the handshake.
	_ = setRcvTimeout(lfd, 5*time.Second)
	fd, _, err := syscall.Accept(lfd)
	syscall.Close(lfd)
	if err != nil {
		t.Fatal(err)
	}
	h.fd = fd
	t.Cleanup(func() { syscall.Close(fd) })
	if msg := h.next(5 * time.Second); msg.kind != "hello" || msg.helloID != "panel-probe" {
		t.Fatalf("expected Hello, got %+v", msg)
	}
	h.send(shellWelcome(testTile, 1.0, shellProto))
	if msg := h.expect("frame", 5*time.Second); msg.w != testTile || msg.h != testTile {
		t.Fatalf("first tile frame was %dx%d", msg.w, msg.h)
	}
	return h
}

func setRcvTimeout(fd int, d time.Duration) error {
	tv := syscall.NsecToTimeval(d.Nanoseconds())
	return syscall.SetsockoptTimeval(fd, syscall.SOL_SOCKET, syscall.SO_RCVTIMEO, &tv)
}

func (h *harness) send(msg []byte) {
	h.t.Helper()
	if _, err := syscall.SendmsgN(h.fd, msg, nil, nil, syscall.MSG_NOSIGNAL); err != nil {
		h.t.Fatalf("shell send: %v", err)
	}
}

// next reads one client message, failing the test after timeout.
func (h *harness) next(timeout time.Duration) clientMsg {
	h.t.Helper()
	buf := make([]byte, MaxMessageBytes)
	deadline := time.Now().Add(timeout)
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			h.t.Fatal("no client message arrived in time")
		}
		_ = setRcvTimeout(h.fd, remaining)
		n, err := syscall.Read(h.fd, buf)
		if err == syscall.EAGAIN || err == syscall.EWOULDBLOCK || err == syscall.EINTR {
			continue
		}
		if err != nil {
			h.t.Fatalf("shell recv: %v", err)
		}
		return decClient(h.t, buf[:n])
	}
}

// expect returns the next message of the wanted kind, skipping tile
// frames and logs.
func (h *harness) expect(kind string, timeout time.Duration) clientMsg {
	h.t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		msg := h.next(time.Until(deadline))
		if msg.kind == kind {
			return msg
		}
		if msg.kind != "frame" && msg.kind != "log" && msg.kind != "pong" {
			h.t.Fatalf("unexpected %s while waiting for %s", msg.kind, kind)
		}
	}
	h.t.Fatalf("no %s arrived in time", kind)
	return clientMsg{}
}

// expectSilence asserts nothing of the forbidden kind crosses the
// wire for the window.
func (h *harness) expectSilence(forbidden string, window time.Duration) {
	h.t.Helper()
	buf := make([]byte, MaxMessageBytes)
	deadline := time.Now().Add(window)
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return
		}
		_ = setRcvTimeout(h.fd, remaining)
		n, err := syscall.Read(h.fd, buf)
		if err == syscall.EAGAIN || err == syscall.EWOULDBLOCK || err == syscall.EINTR {
			continue
		}
		if err != nil {
			h.t.Fatalf("shell recv: %v", err)
		}
		if msg := decClient(h.t, buf[:n]); msg.kind == forbidden {
			h.t.Fatalf("%s crossed the wire too early", forbidden)
		}
	}
}

// collectRepaint gathers one full repaint: a top-to-bottom,
// contiguous band sequence covering the granted height, all bands the
// granted width, sharing one generation. Returns the reassembled
// pixels.
func (h *harness) collectRepaint(wantW, wantH uint32) []byte {
	h.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	var gen uint32
	haveGen := false
	nextY := uint32(0)
	var out []byte
	for nextY < wantH {
		msg := h.expect("panel_frame", time.Until(deadline))
		if msg.w != wantW {
			h.t.Fatalf("band width %d, want the granted %d", msg.w, wantW)
		}
		if msg.y != nextY {
			h.t.Fatalf("band at y=%d, want contiguous y=%d", msg.y, nextY)
		}
		if !haveGen {
			gen, haveGen = msg.gen, true
		} else if msg.gen != gen {
			h.t.Fatalf("band generation %d, want the repaint's %d", msg.gen, gen)
		}
		out = append(out, msg.pixels...)
		nextY += msg.bandRows
	}
	if nextY != wantH {
		h.t.Fatalf("bands cover %d rows, want exactly %d", nextY, wantH)
	}
	return out
}

func (h *harness) pressTile() {
	h.send(shellInput(InputPress, testTile/2, testTile/2, ButtonLeft, 0))
}

func (h *harness) event(timeout time.Duration) appEvent {
	h.t.Helper()
	select {
	case ev := <-h.events:
		return ev
	case <-time.After(timeout):
		h.t.Fatal("the dockapp reported nothing in time")
		return appEvent{}
	}
}

// shutdown ends the app cleanly and waits for Run to return.
func (h *harness) shutdown() {
	h.t.Helper()
	h.send(shellGoodbye(1))
	select {
	case err := <-h.done:
		if err != nil {
			h.t.Fatalf("Run returned %v on a clean Shutdown", err)
		}
	case <-time.After(5 * time.Second):
		h.t.Fatal("the dockapp never exited")
	}
}

// -- the tests --------------------------------------------------------

func TestPanelGrantFlowWithAClampedGrant(t *testing.T) {
	h := newHarness(t)
	var panel *Panel
	h.onPress = func(ctx *Ctx) { panel, _ = ctx.OpenPanel(300, 200) }
	h.pressTile()
	if msg := h.expect("open_panel", 5*time.Second); msg.openW != 300 || msg.openH != 200 {
		t.Fatalf("OpenPanel asked for %dx%d", msg.openW, msg.openH)
	}
	// No PanelFrame may cross before the grant.
	h.expectSilence("panel_frame", 300*time.Millisecond)
	if panel.Opened() {
		t.Fatal("Opened() before any grant arrived")
	}
	// Grant it — clamped smaller than asked.
	h.send(shellPanelOpened(280, 180))
	if ev := h.event(5 * time.Second); ev.kind != "opened" || ev.w != 280 || ev.h != 180 {
		t.Fatalf("OnPanelOpened = %+v", ev)
	}
	pixels := h.collectRepaint(280, 180)
	if want := bytes.Repeat([]byte(panelFill), 280*180); !bytes.Equal(pixels, want) {
		t.Fatal("the first repaint, reassembled, is not byte-exact")
	}
	if w, hh := panel.Requested(); w != 300 || hh != 200 {
		t.Fatalf("Requested() = %dx%d", w, hh)
	}
	h.shutdown()
	if ev := h.event(time.Second); ev.kind != "closed" || ev.reason != PanelShutdown {
		t.Fatalf("expected the shutdown close, got %+v", ev)
	}
}

func TestDrawBeforeTheGrantIsBlockedBySDK(t *testing.T) {
	h := newHarness(t)
	drawErr := make(chan error, 1)
	h.onPress = func(ctx *Ctx) {
		p, err := ctx.OpenPanel(100, 100)
		if err != nil {
			t.Error(err)
			return
		}
		drawErr <- p.Draw(make([]byte, 100*100*4))
	}
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	select {
	case err := <-drawErr:
		if err == nil {
			t.Fatal("a frame before PanelOpened must be refused locally")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Draw never returned")
	}
	h.expectSilence("panel_frame", 300*time.Millisecond)
	h.shutdown()
}

func TestARefusedPanelReportsRefusedAndNeverOpens(t *testing.T) {
	h := newHarness(t)
	h.onPress = func(ctx *Ctx) { _, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelClosed(3)) // refused
	if ev := h.event(5 * time.Second); ev.kind != "closed" || ev.reason != PanelRefused {
		t.Fatalf("expected the refusal, got %+v", ev)
	}
	h.expectSilence("panel_frame", 300*time.Millisecond)
	h.shutdown()
}

func TestDismissedBehindYourBackDeadensTheHandle(t *testing.T) {
	h := newHarness(t)
	var panel *Panel
	h.onPress = func(ctx *Ctx) { panel, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	h.expect("panel_frame", 5*time.Second)
	if ev := h.event(time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	h.send(shellPanelClosed(1)) // the user clicked away
	if ev := h.event(5 * time.Second); ev.kind != "closed" || ev.reason != PanelDismissed {
		t.Fatalf("expected the dismissal, got %+v", ev)
	}
	if !panel.Closed() || panel.Reason != PanelDismissed {
		t.Fatalf("handle state: closed=%v reason=%v", panel.Closed(), panel.Reason)
	}
	if err := panel.Draw(make([]byte, 64*64*4)); err == nil {
		t.Fatal("Draw on a dismissed panel must fail")
	}
	h.expectSilence("panel_frame", 300*time.Millisecond)
	h.shutdown()
}

func TestCloseIsARoundTripEndingInClosed(t *testing.T) {
	h := newHarness(t)
	h.onPress = func(ctx *Ctx) { _, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	h.expect("panel_frame", 5*time.Second)
	if ev := h.event(time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	h.onPress = func(ctx *Ctx) { ctx.Panel().Close() }
	h.pressTile()
	h.expect("close_panel", 5*time.Second)
	h.send(shellPanelClosed(0)) // the shell confirms
	if ev := h.event(5 * time.Second); ev.kind != "closed" || ev.reason != PanelClosedByClient {
		t.Fatalf("expected the confirmation, got %+v", ev)
	}
	h.shutdown()
}

func TestRenegotiationBlocksFramesUntilTheNewGrant(t *testing.T) {
	h := newHarness(t)
	var first, second *Panel
	h.onPress = func(ctx *Ctx) { first, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	h.expect("panel_frame", 5*time.Second)
	if ev := h.event(time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	// Ask again, bigger: same handle, a fresh OpenPanel on the wire.
	h.onPress = func(ctx *Ctx) { second, _ = ctx.OpenPanel(128, 96) }
	h.pressTile()
	if msg := h.expect("open_panel", 5*time.Second); msg.openW != 128 || msg.openH != 96 {
		t.Fatalf("renegotiation asked for %dx%d", msg.openW, msg.openH)
	}
	if first != second {
		t.Fatal("renegotiation must keep the handle")
	}
	if second.Opened() {
		t.Fatal("frames must pause until the new grant")
	}
	h.expectSilence("panel_frame", 300*time.Millisecond)
	h.send(shellPanelOpened(128, 96))
	if ev := h.event(5 * time.Second); ev.kind != "opened" || ev.w != 128 || ev.h != 96 {
		t.Fatalf("expected the re-grant, got %+v", ev)
	}
	if pixels := h.collectRepaint(128, 96); !bytes.Equal(pixels, bytes.Repeat([]byte(panelFill), 128*96)) {
		t.Fatal("the post-renegotiation repaint is not byte-exact")
	}
	h.shutdown()
	h.event(time.Second) // the synthesized shutdown close
}

func TestPanelInputIsDispatchedInPanelCoordinates(t *testing.T) {
	h := newHarness(t)
	h.onPress = func(ctx *Ctx) { _, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	h.expect("panel_frame", 5*time.Second)
	if ev := h.event(time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	h.send(shellPanelInput(InputScroll, 7, 9, ButtonNone, -2))
	ev := h.event(5 * time.Second)
	if ev.kind != "panel_input" || ev.input.Kind != InputScroll ||
		ev.input.X != 7 || ev.input.Y != 9 || ev.input.Delta != -2 {
		t.Fatalf("panel input = %+v", ev)
	}
	// Motion (kind 6) is hover tracking: panel-only, button 0,
	// dispatched to OnPanelInput like any other panel event.
	h.send(shellPanelInput(InputMotion, 15, 16, ButtonNone, 0))
	if ev := h.event(5 * time.Second); ev.kind != "panel_input" ||
		ev.input.Kind != InputMotion || ev.input.Button != ButtonNone ||
		ev.input.X != 15 || ev.input.Y != 16 {
		t.Fatalf("motion = %+v", ev)
	}
	// A press asks for a repaint (the probe returns true for it).
	h.send(shellPanelInput(InputPress, 3, 4, ButtonLeft, 0))
	if ev := h.event(5 * time.Second); ev.kind != "panel_input" || ev.input.X != 3 {
		t.Fatalf("panel input = %+v", ev)
	}
	h.expect("panel_frame", 5*time.Second)
	h.shutdown()
	h.event(time.Second) // the synthesized shutdown close
}

func TestAFullRepaintStreamsAsContiguousBands(t *testing.T) {
	// 640 wide: a band carries at most 262080 / 2560 = 102 rows, so a
	// 300-row repaint is exactly 102 + 102 + 96.
	h := newHarness(t)
	h.onPress = func(ctx *Ctx) { _, _ = ctx.OpenPanel(640, 300) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(640, 300))
	if ev := h.event(5 * time.Second); ev.kind != "opened" || ev.w != 640 || ev.h != 300 {
		t.Fatalf("expected the grant, got %+v", ev)
	}
	deadline := time.Now().Add(5 * time.Second)
	var lays [][2]uint32
	var gens []uint32
	var pixels []byte
	rows := uint32(0)
	for rows < 300 {
		msg := h.expect("panel_frame", time.Until(deadline))
		if msg.w != 640 {
			t.Fatalf("band width %d", msg.w)
		}
		lays = append(lays, [2]uint32{msg.y, msg.bandRows})
		gens = append(gens, msg.gen)
		pixels = append(pixels, msg.pixels...)
		rows += msg.bandRows
	}
	want := [][2]uint32{{0, 102}, {102, 102}, {204, 96}}
	for i, l := range lays {
		if i >= len(want) || l != want[i] {
			t.Fatalf("bands = %v, want %v (maximal, top to bottom)", lays, want)
		}
	}
	for _, g := range gens {
		if g != gens[0] {
			t.Fatalf("generations = %v: one repaint shares one", gens)
		}
	}
	if !bytes.Equal(pixels, bytes.Repeat([]byte(panelFill), 640*300)) {
		t.Fatal("the reassembled repaint is not byte-exact")
	}
	h.shutdown()
	h.event(time.Second) // the synthesized shutdown close
}

func TestDrawRowsSendsOneNarrowBand(t *testing.T) {
	h := newHarness(t)
	h.app.DrawPanel = nil // push-style panel
	var panel *Panel
	h.onPress = func(ctx *Ctx) { panel, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	if ev := h.event(5 * time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	row := bytes.Repeat([]byte{0xEE, 0xDD, 0xCC, 0xFF}, 64)
	rowErr := make(chan error, 1)
	h.onPress = func(ctx *Ctx) { rowErr <- panel.DrawRows(10, append(append([]byte{}, row...), row...)) }
	h.pressTile()
	msg := h.expect("panel_frame", 5*time.Second)
	if msg.y != 10 || msg.bandRows != 2 || msg.w != 64 {
		t.Fatalf("partial update band = y%d h%d w%d, want y10 h2 w64", msg.y, msg.bandRows, msg.w)
	}
	if err := <-rowErr; err != nil {
		t.Fatal(err)
	}
	// Out-of-grant partial updates are refused locally.
	h.onPress = func(ctx *Ctx) { rowErr <- panel.DrawRows(63, append(append([]byte{}, row...), row...)) }
	h.pressTile()
	if err := <-rowErr; err == nil {
		t.Fatal("DrawRows past the granted height must fail")
	}
	h.shutdown()
	h.event(time.Second) // the synthesized shutdown close
}

func TestOpenPanelIsGatedOffOnAProtocol1Shell(t *testing.T) {
	// The same probe against a shell that predates panels (it zeroes
	// the version field in Welcome): the tile must work, and OpenPanel
	// must fail locally without ever putting 0x05 on the wire — a
	// pre-panel shell would answer it with Goodbye{ProtocolError} and
	// take the tile down too.
	h := newHarnessProto(t, 0)
	gateErr := make(chan error, 1)
	h.onPress = func(ctx *Ctx) {
		_, err := ctx.OpenPanel(64, 64)
		gateErr <- err
	}
	h.pressTile()
	select {
	case err := <-gateErr:
		if !errors.Is(err, ErrPanelsUnsupported) {
			t.Fatalf("want ErrPanelsUnsupported, got %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("OpenPanel never returned")
	}
	// Nothing panel-shaped may reach a shell that cannot read it.
	h.expectSilence("open_panel", 300*time.Millisecond)
	h.shutdown()
}

func TestShellShutdownReachesThePanelAsShutdown(t *testing.T) {
	h := newHarness(t)
	h.onPress = func(ctx *Ctx) { _, _ = ctx.OpenPanel(64, 64) }
	h.pressTile()
	h.expect("open_panel", 5*time.Second)
	h.send(shellPanelOpened(64, 64))
	h.expect("panel_frame", 5*time.Second)
	if ev := h.event(time.Second); ev.kind != "opened" {
		t.Fatalf("expected opened, got %+v", ev)
	}
	h.shutdown()
	if ev := h.event(time.Second); ev.kind != "closed" || ev.reason != PanelShutdown {
		t.Fatalf("a dying connection must close the panel locally, got %+v", ev)
	}
}
