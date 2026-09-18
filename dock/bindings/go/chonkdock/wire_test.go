package chonkdock

// The pinned vectors from chonk-dock-proto's own test suite
// (hello_has_the_documented_byte_layout, input_is_a_fixed_twenty_bytes),
// so a refactor here fails against the reference bytes rather than in
// a session's dock.

import (
	"bytes"
	"encoding/binary"
	"math"
	"testing"
)

func TestHelloHasTheDocumentedByteLayout(t *testing.T) {
	token := bytes.Repeat([]byte{0xAB}, TokenBytes)
	msg, err := EncodeHello("clock", 1, token, WantPress|WantCrossing)
	if err != nil {
		t.Fatal(err)
	}
	if len(msg) != 33 {
		t.Fatalf("Hello is %d bytes, want 33", len(msg))
	}
	if !bytes.Equal(msg[0:4], []byte{0x01, 0, 0, 0}) {
		t.Errorf("header = % x", msg[0:4])
	}
	if binary.LittleEndian.Uint32(msg[4:8]) != ProtocolVersion {
		t.Errorf("proto = %d", binary.LittleEndian.Uint32(msg[4:8]))
	}
	if msg[8] != 1 || msg[9] != WantPress|WantCrossing || msg[10] != 5 || msg[11] != 0 {
		t.Errorf("fixed fields = % x", msg[8:12])
	}
	if !bytes.Equal(msg[12:28], token) || string(msg[28:]) != "clock" {
		t.Errorf("token/id = % x", msg[12:])
	}
}

func TestServerMessagesRoundTripStrictly(t *testing.T) {
	// A Welcome built by hand at the documented layout.
	body := make([]byte, 0, 32)
	body = binary.LittleEndian.AppendUint32(body, 112)
	body = binary.LittleEndian.AppendUint32(body, math.Float32bits(2.0))
	body = binary.LittleEndian.AppendUint16(body, 16)
	body = binary.LittleEndian.AppendUint16(body, 0)
	body = binary.LittleEndian.AppendUint32(body, 0)
	body = append(body, "nextstep-classic"...)
	msg, err := DecodeServer(append([]byte{0x81, 0, 0, 0}, body...))
	if err != nil {
		t.Fatal(err)
	}
	if msg.Kind != "welcome" || msg.Theme.TilePx != 112 || msg.Theme.Scale != 2.0 || msg.Theme.ThemeID != "nextstep-classic" {
		t.Errorf("welcome = %+v", msg)
	}

	// Input is a fixed 20 bytes: Scroll, no button, delta -1.
	in := []byte{0x83, 0, 0, 0, 3, 0, 0, 0}
	in = binary.LittleEndian.AppendUint32(in, 1)
	in = binary.LittleEndian.AppendUint32(in, 2)
	var minusOne int32 = -1
	in = binary.LittleEndian.AppendUint32(in, uint32(minusOne))
	msg, err = DecodeServer(in)
	if err != nil {
		t.Fatal(err)
	}
	if msg.Input.Kind != InputScroll || msg.Input.Button != ButtonNone || msg.Input.Delta != -1 {
		t.Errorf("input = %+v", msg.Input)
	}
}

func TestMalformedInputIsRejectedNotClamped(t *testing.T) {
	nan := make([]byte, 0, 20)
	nan = binary.LittleEndian.AppendUint32(nan, 56)
	nan = binary.LittleEndian.AppendUint32(nan, 0x7FC00000) // NaN scale
	nan = append(nan, 0, 0, 0, 0, 0, 0, 0, 0)
	for _, bad := range [][]byte{
		{},                                    // empty
		{0x99, 0, 0, 0},                       // unknown kind
		{0x85, 1, 0, 0, 0, 0, 0, 0},           // reserved header byte set
		{0x85, 0, 0, 0, 1, 2, 3, 4, 5},        // trailing byte after Ping
		{0x86, 0, 0, 0, 9, 0, 0, 0},           // undefined goodbye reason
		{0x84, 0, 0, 0, 2, 0, 0, 0},           // visibility neither 0 nor 1
		append([]byte{0x81, 0, 0, 0}, nan...), // NaN scale
	} {
		if _, err := DecodeServer(bad); err == nil {
			t.Errorf("accepted % x", bad)
		}
	}
}

func TestFrameFitsAgreesWithTheReference(t *testing.T) {
	// The cases pinned in chonk-dock-proto's lib.rs tests.
	for _, ok := range []struct {
		px    uint32
		units uint8
		want  bool
	}{
		{56, 1, true}, {112, 4, true}, {168, 2, true},
		{224, 4, false}, {168, 4, false}, {256, 4, false},
		{0, 1, false}, {56, 0, false}, {257, 1, false}, {56, 5, false},
	} {
		if got := FrameFits(ok.px, ok.units); got != ok.want {
			t.Errorf("FrameFits(%d, %d) = %v, want %v", ok.px, ok.units, got, ok.want)
		}
	}
}
