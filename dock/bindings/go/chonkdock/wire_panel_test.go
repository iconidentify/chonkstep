package chonkdock

// The instrument-panel half of the codec, held to the documented byte
// layouts: OpenPanel, the *banded* PanelFrame and ClosePanel written
// out byte-for-byte, and strict decodes of PanelOpened, the padded
// PanelClosed and PanelInput — including the malformed vectors a
// hostile or broken shell could send. Also the version advertisement:
// Welcome's once-reserved u16 now carries the shell's protocol
// version, and zero (what pre-panel shells always sent) decodes as 1.

import (
	"bytes"
	"encoding/binary"
	"math"
	"testing"
)

func TestPanelEncodingsHaveTheDocumentedByteLayout(t *testing.T) {
	// OpenPanel: header + width u32 + height u32, little-endian.
	msg, err := EncodeOpenPanel(320, 240)
	if err != nil {
		t.Fatal(err)
	}
	want := []byte{0x05, 0, 0, 0}
	want = binary.LittleEndian.AppendUint32(want, 320)
	want = binary.LittleEndian.AppendUint32(want, 240)
	if !bytes.Equal(msg, want) {
		t.Errorf("OpenPanel = % x, want % x", msg, want)
	}

	// PanelFrame is one BAND: header + generation + y + band_height +
	// width + pixels.
	pixels := bytes.Repeat([]byte{0xAA}, 2*3*4)
	msg, err = EncodePanelFrame(7, 5, 3, 2, pixels)
	if err != nil {
		t.Fatal(err)
	}
	want = []byte{0x06, 0, 0, 0}
	want = binary.LittleEndian.AppendUint32(want, 7) // generation
	want = binary.LittleEndian.AppendUint32(want, 5) // y
	want = binary.LittleEndian.AppendUint32(want, 3) // band_height
	want = binary.LittleEndian.AppendUint32(want, 2) // width
	want = append(want, pixels...)
	if !bytes.Equal(msg, want) {
		t.Errorf("PanelFrame = % x, want % x", msg, want)
	}

	// ClosePanel is the bare header.
	if got := EncodeClosePanel(); !bytes.Equal(got, []byte{0x07, 0, 0, 0}) {
		t.Errorf("ClosePanel = % x", got)
	}
}

func TestPanelEncodersRejectOutOfRangeGeometry(t *testing.T) {
	if _, err := EncodeOpenPanel(0, 64); err == nil {
		t.Error("accepted a zero-width panel")
	}
	if _, err := EncodeOpenPanel(64, MaxPanelPx+1); err == nil {
		t.Error("accepted a panel over the per-edge cap")
	}
	// An OpenPanel request may ask up to the protocol bound.
	if _, err := EncodeOpenPanel(MaxPanelPx, MaxPanelPx); err != nil {
		t.Errorf("refused a maximal request: %v", err)
	}
	if _, err := EncodePanelFrame(1, 0, 2, 2, make([]byte, 15)); err == nil {
		t.Error("accepted a band one byte short")
	}
	if _, err := EncodePanelFrame(1, 0, 1, 0, nil); err == nil {
		t.Error("accepted a zero-width band")
	}
	if _, err := EncodePanelFrame(1, 0, 0, 2, nil); err == nil {
		t.Error("accepted a zero-height band")
	}
	if _, err := EncodePanelFrame(1, 0, 1, MaxPanelPx+1, make([]byte, (MaxPanelPx+1)*4)); err == nil {
		t.Error("accepted a band over the per-edge cap")
	}
	if _, err := EncodePanelFrame(1, 1000, 25, 8, make([]byte, 8*25*4)); err == nil {
		t.Error("accepted a band running past the panel edge")
	}
	// The oversized band: 1024 * 64 * 4 = 262144 > 262080.
	if _, err := EncodePanelFrame(1, 0, 64, 1024, make([]byte, 1024*64*4)); err == nil {
		t.Error("accepted a band a datagram cannot carry")
	}
	// One row shy fits.
	if _, err := EncodePanelFrame(1, 0, 63, 1024, make([]byte, 1024*63*4)); err != nil {
		t.Errorf("refused a maximal legal band: %v", err)
	}
}

func TestPanelFitsAgreesWithTheCaps(t *testing.T) {
	for _, c := range []struct {
		w, h uint32
		want bool
	}{
		{1, 1, true}, {1024, 1024, true}, // 1024^2*4 is exactly 4 MiB
		{1025, 1, false}, {1, 1025, false}, {0, 64, false}, {64, 0, false},
	} {
		if got := PanelFits(c.w, c.h); got != c.want {
			t.Errorf("PanelFits(%d, %d) = %v, want %v", c.w, c.h, got, c.want)
		}
	}
	if MaxPanelFrameBytes != 1024*1024*4 {
		t.Errorf("the byte cap and the edge cap disagree")
	}
}

func TestPanelBandRowsFillsButNeverOverflowsADatagram(t *testing.T) {
	if got := PanelBandRows(1024); got != 63 { // 262080 / 4096
		t.Errorf("PanelBandRows(1024) = %d, want 63", got)
	}
	if got := PanelBandRows(320); got != 204 { // 262080 / 1280
		t.Errorf("PanelBandRows(320) = %d, want 204", got)
	}
	if got := PanelBandRows(1); got != 1024 { // edge-capped
		t.Errorf("PanelBandRows(1) = %d, want 1024", got)
	}
	for _, width := range []uint32{1, 17, 56, 320, 640, 1024} {
		rows := PanelBandRows(width)
		if rows == 0 || rows > MaxPanelPx || int(width)*int(rows)*4 > MaxFrameBytes {
			t.Errorf("PanelBandRows(%d) = %d violates a bound", width, rows)
		}
	}
}

func TestWelcomeAdvertisesTheShellProtocolVersion(t *testing.T) {
	welcome := func(proto uint16) []byte {
		out := []byte{0x81, 0, 0, 0}
		out = binary.LittleEndian.AppendUint32(out, 56)
		out = binary.LittleEndian.AppendUint32(out, math.Float32bits(1.0))
		out = binary.LittleEndian.AppendUint16(out, 0) // theme_id_len
		out = binary.LittleEndian.AppendUint16(out, proto)
		return binary.LittleEndian.AppendUint32(out, 0) // theme_toml_len
	}
	msg, err := DecodeServer(welcome(2))
	if err != nil {
		t.Fatal(err)
	}
	if msg.Theme.Proto != 2 {
		t.Errorf("Proto = %d, want 2", msg.Theme.Proto)
	}
	// A pre-panel shell zeroes the field; zero decodes as 1.
	msg, err = DecodeServer(welcome(0))
	if err != nil {
		t.Fatal(err)
	}
	if msg.Theme.Proto != 1 {
		t.Errorf("Proto for a zeroed field = %d, want 1", msg.Theme.Proto)
	}
}

func TestPanelServerMessagesDecodeStrictly(t *testing.T) {
	// PanelOpened: a (possibly clamped) grant. Banding made the whole
	// protocol range streamable, so big grants are legal.
	opened := func(w, h uint32) []byte {
		out := []byte{0x87, 0, 0, 0}
		out = binary.LittleEndian.AppendUint32(out, w)
		return binary.LittleEndian.AppendUint32(out, h)
	}
	msg, err := DecodeServer(opened(280, 180))
	if err != nil {
		t.Fatal(err)
	}
	if msg.Kind != "panel_opened" || msg.PanelW != 280 || msg.PanelH != 180 {
		t.Errorf("panel_opened = %+v", msg)
	}
	if _, err := DecodeServer(opened(1024, 1024)); err != nil {
		t.Errorf("refused a maximal grant: %v", err)
	}

	// PanelClosed: reason u8 + 3 reserved zeros, every reason named.
	for reason, name := range map[uint8]string{
		0: "closed", 1: "dismissed", 2: "shutdown", 3: "refused",
	} {
		msg, err := DecodeServer([]byte{0x88, 0, 0, 0, reason, 0, 0, 0})
		if err != nil {
			t.Fatal(err)
		}
		if msg.Kind != "panel_closed" || msg.PanelReason.String() != name {
			t.Errorf("reason %d = %+v", reason, msg)
		}
	}

	// PanelInput: Input-shaped, panel-local coordinates.
	in := []byte{0x89, 0, 0, 0, InputScroll, 0, 0, 0}
	in = binary.LittleEndian.AppendUint32(in, 7)
	in = binary.LittleEndian.AppendUint32(in, 9)
	var minusTwo int32 = -2
	in = binary.LittleEndian.AppendUint32(in, uint32(minusTwo))
	msg, err = DecodeServer(in)
	if err != nil {
		t.Fatal(err)
	}
	if msg.Kind != "panel_input" || msg.Input.Kind != InputScroll ||
		msg.Input.X != 7 || msg.Input.Y != 9 || msg.Input.Delta != -2 {
		t.Errorf("panel_input = %+v", msg.Input)
	}

	// Motion (kind 6) exists only inside PanelInput: byte-exact hover
	// tracking, button 0, panel device pixels.
	motion := []byte{0x89, 0, 0, 0, InputMotion, 0, 0, 0}
	motion = binary.LittleEndian.AppendUint32(motion, 12)
	motion = binary.LittleEndian.AppendUint32(motion, 34)
	motion = binary.LittleEndian.AppendUint32(motion, 0)
	msg, err = DecodeServer(motion)
	if err != nil {
		t.Fatal(err)
	}
	if msg.Kind != "panel_input" || msg.Input.Kind != InputMotion ||
		msg.Input.Button != ButtonNone || msg.Input.X != 12 || msg.Input.Y != 34 {
		t.Errorf("motion = %+v", msg.Input)
	}
	// The same 16 bytes as a tile Input (0x83) are undefined.
	tileMotion := append([]byte{}, motion...)
	tileMotion[0] = 0x83

	// Malformed vectors: rejected, never clamped.
	badInput := append([]byte{}, in...)
	badInput[6] = 1 // reserved field
	for _, bad := range [][]byte{
		append(opened(64, 64), 0x00),            // trailing byte
		opened(MaxPanelPx+1, 64),                // grant over the per-edge cap
		opened(0, 64),                           // zero-width grant
		{0x88, 0, 0, 0, 4, 0, 0, 0},             // undefined close reason
		{0x88, 0, 0, 0, 0},                      // the old unpadded 1-byte body
		{0x88, 0, 0, 0, 0, 1, 0, 0},             // reserved padding set
		{0x88, 0, 0, 0},                         // reason missing entirely
		{0x87, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0}, // reserved header byte set
		badInput,                                // PanelInput reserved field set
		tileMotion,                              // Motion is a PanelInput kind, never a tile Input kind
		{0x89, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}, // undefined input kind
	} {
		if _, err := DecodeServer(bad); err == nil {
			t.Errorf("accepted % x", bad)
		}
	}
}
