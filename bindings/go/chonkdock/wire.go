// Package chonkdock implements the chonkstep dockapp protocol,
// version 1, in stdlib Go.
//
// A dockapp is a separate process that draws one (or a few) chonkstep
// dock tiles and pushes finished pixels to the desktop shell over a
// private SOCK_SEQPACKET Unix socket. It holds no display connection,
// crashes alone, restyles without restarting, and survives shell
// restarts. The wire contract this package implements is documented in
// docs/dockapp-protocol.md in the chonkstep repository; the normative
// implementation is crates/chonk-dock-proto.
//
// This file is the codec: pure, no I/O. Decoding is strict on purpose,
// mirroring the reference decoder — unknown kinds, non-zero reserved
// bytes, trailing bytes, out-of-range enums and unusable floats are
// rejected rather than clamped, because a client that silently
// mis-parses a message draws garbage with no error to explain it.
package chonkdock

import (
	"encoding/binary"
	"fmt"
	"math"
	"unicode/utf8"
)

// Protocol constants. See docs/dockapp-protocol.md section 3 for the
// reasoning behind each value.
const (
	ProtocolVersion   = 1
	TokenBytes        = 16
	MaxMessageBytes   = 256 * 1024
	MaxFrameBytes     = MaxMessageBytes - 64
	MaxTilePx         = 256
	MaxScale          = 8.0
	MaxTileUnits      = 4
	MaxIDBytes        = 64
	MaxLogBytes       = 256
	MaxThemeIDBytes   = 64
	MaxThemeTOMLBytes = 128 * 1024
)

// Message kinds. Client->shell in the low space, shell->client with
// the high bit set.
const (
	kindHello        = 0x01
	kindFrame        = 0x02
	kindPong         = 0x03
	kindLog          = 0x04
	kindWelcome      = 0x81
	kindThemeChanged = 0x82
	kindInput        = 0x83
	kindVisibility   = 0x84
	kindPing         = 0x85
	kindGoodbye      = 0x86
)

// Input-mask bits for Hello's wants field.
const (
	WantPress    = 1 << 0
	WantRelease  = 1 << 1
	WantScroll   = 1 << 2
	WantCrossing = 1 << 3
	WantAll      = WantPress | WantRelease | WantScroll | WantCrossing
)

// InputEvent kinds.
const (
	InputPress   = 1
	InputRelease = 2
	InputScroll  = 3
	InputEnter   = 4
	InputLeave   = 5
)

// InputEvent buttons (0 = none). Middle and Right exist on the wire
// but the shell never sends them: middle is the dock's reorder gesture
// and right opens the per-tile menu.
const (
	ButtonNone   = 0
	ButtonLeft   = 1
	ButtonMiddle = 2
	ButtonRight  = 3
)

// Log levels.
const (
	LogError = 1
	LogWarn  = 2
	LogInfo  = 3
	LogDebug = 4
)

// GoodbyeReason says why the shell is closing a connection.
type GoodbyeReason uint8

// Goodbye reasons.
const (
	GoodbyeShutdown      GoodbyeReason = 1
	GoodbyeProtocolError GoodbyeReason = 2
	GoodbyeUnauthorized  GoodbyeReason = 3
	GoodbyeReplaced      GoodbyeReason = 4
	GoodbyeTileTooLarge  GoodbyeReason = 5
	GoodbyeOverflow      GoodbyeReason = 6
	GoodbyeRemoved       GoodbyeReason = 7
)

func (r GoodbyeReason) String() string {
	switch r {
	case GoodbyeShutdown:
		return "Shutdown"
	case GoodbyeProtocolError:
		return "ProtocolError"
	case GoodbyeUnauthorized:
		return "Unauthorized"
	case GoodbyeReplaced:
		return "Replaced"
	case GoodbyeTileTooLarge:
		return "TileTooLarge"
	case GoodbyeOverflow:
		return "Overflow"
	case GoodbyeRemoved:
		return "Removed"
	default:
		return fmt.Sprintf("GoodbyeReason(%d)", uint8(r))
	}
}

// DecodeError means the peer's bytes could not be read as a v1
// message. A client's correct response is to drop the connection: the
// two ends disagree about the protocol, and continuing would be
// guessing.
type DecodeError struct{ What string }

func (e *DecodeError) Error() string { return "chonkdock: undecodable message: " + e.What }

func decodeErr(format string, args ...any) error {
	return &DecodeError{What: fmt.Sprintf(format, args...)}
}

// ThemeState is the geometry and palette a dockapp draws with, carried
// by Welcome and ThemeChanged. ThemeID is the fast path (look the
// palette up if you ship them); ThemeTOML is the correctness path (a
// serialized theme table, possibly empty). A client that can use
// neither falls back to its own default colors — wrong colors, never a
// blank tile.
type ThemeState struct {
	TilePx    uint32
	Scale     float32
	ThemeID   string
	ThemeTOML string
}

// InputEvent is one pointer event, in coordinates local to this
// dockapp's own tile.
type InputEvent struct {
	Kind   uint8 // Input* constants
	Button uint8 // Button* constants; ButtonNone for Scroll/Enter/Leave
	X, Y   int32
	Delta  int32 // scroll notches, signed; 0 for everything but Scroll
}

// ServerMessage is one decoded shell->client datagram. Exactly one of
// the payload fields is meaningful, selected by Kind.
type ServerMessage struct {
	Kind    string // "welcome" | "theme_changed" | "input" | "visibility" | "ping" | "goodbye"
	Theme   ThemeState
	Input   InputEvent
	Visible bool
	Seq     uint32
	Reason  GoodbyeReason
}

// IsValidID reports whether id satisfies the wire's id rule:
// 1..=64 bytes of [A-Za-z0-9._:-].
func IsValidID(id string) bool {
	if len(id) == 0 || len(id) > MaxIDBytes {
		return false
	}
	for i := 0; i < len(id); i++ {
		c := id[i]
		ok := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' ||
			c >= '0' && c <= '9' || c == '-' || c == '_' || c == '.' || c == ':'
		if !ok {
			return false
		}
	}
	return true
}

// FrameFits reports whether a tile of this geometry can be carried by
// the v1 inline transport. The shell refuses a Hello for which this is
// false, with Goodbye{TileTooLarge}.
func FrameFits(tilePx uint32, tileUnits uint8) bool {
	if tilePx == 0 || tilePx > MaxTilePx || tileUnits == 0 || tileUnits > MaxTileUnits {
		return false
	}
	return uint64(tilePx)*uint64(tilePx)*uint64(tileUnits)*4 <= MaxFrameBytes
}

func header(kind byte, capacity int) []byte {
	out := make([]byte, 4, 4+capacity)
	out[0] = kind
	return out
}

// EncodeHello builds the message a dockapp opens with. token is the 16
// raw bytes decoded from CHONKSTEP_DOCK_TOKEN.
func EncodeHello(id string, tileUnits uint8, token []byte, wants uint8) ([]byte, error) {
	if !IsValidID(id) {
		return nil, fmt.Errorf("chonkdock: invalid dockapp id %q", id)
	}
	if len(token) != TokenBytes {
		return nil, fmt.Errorf("chonkdock: token must be %d bytes, got %d", TokenBytes, len(token))
	}
	if wants&^uint8(WantAll) != 0 {
		return nil, fmt.Errorf("chonkdock: undefined wants bits %#x", wants)
	}
	out := header(kindHello, 24+len(id))
	out = binary.LittleEndian.AppendUint32(out, ProtocolVersion)
	out = append(out, tileUnits, wants, byte(len(id)), 0)
	out = append(out, token...)
	out = append(out, id...)
	return out, nil
}

// EncodeFrame builds one tile frame. pixels is premultiplied RGBA8,
// top row first, width*height*4 bytes with no row padding.
func EncodeFrame(generation, width, height uint32, pixels []byte) ([]byte, error) {
	if width == 0 || height == 0 || width > MaxTilePx || height > MaxTilePx*MaxTileUnits {
		return nil, fmt.Errorf("chonkdock: frame geometry %dx%d is out of range", width, height)
	}
	expected := int(width) * int(height) * 4
	if len(pixels) != expected {
		return nil, fmt.Errorf("chonkdock: frame needs %d pixel bytes, got %d", expected, len(pixels))
	}
	if expected > MaxFrameBytes {
		return nil, fmt.Errorf("chonkdock: frame of %d bytes exceeds the cap", expected)
	}
	out := header(kindFrame, 12+len(pixels))
	out = binary.LittleEndian.AppendUint32(out, generation)
	out = binary.LittleEndian.AppendUint32(out, width)
	out = binary.LittleEndian.AppendUint32(out, height)
	out = append(out, pixels...)
	return out, nil
}

// EncodePong answers a Ping, echoing its seq.
func EncodePong(seq uint32) []byte {
	return binary.LittleEndian.AppendUint32(header(kindPong, 4), seq)
}

// EncodeLog builds a diagnostic line for the session journal. The text
// is sanitized and truncated exactly as the reference encoder does:
// control characters, the Unicode line/paragraph separators, bidi
// controls and zero-width characters are dropped, and the result is
// clipped to 256 bytes on a rune boundary.
func EncodeLog(level uint8, text string) ([]byte, error) {
	if level < LogError || level > LogDebug {
		return nil, fmt.Errorf("chonkdock: undefined log level %d", level)
	}
	clean := make([]byte, 0, min(len(text), MaxLogBytes))
	for _, r := range text {
		dangerous := r < 0x20 || r == 0x7F || (r >= 0x80 && r <= 0x9F) ||
			r == 0x2028 || r == 0x2029 ||
			(r >= 0x202A && r <= 0x202E) || (r >= 0x2066 && r <= 0x2069) ||
			r == 0x200B || r == 0x200C || r == 0x200D || r == 0xFEFF
		if dangerous {
			continue
		}
		if len(clean)+utf8.RuneLen(r) > MaxLogBytes {
			break
		}
		clean = utf8.AppendRune(clean, r)
	}
	out := header(kindLog, 4+len(clean))
	out = append(out, level, 0)
	out = binary.LittleEndian.AppendUint16(out, uint16(len(clean)))
	out = append(out, clean...)
	return out, nil
}

func decodeThemeState(body []byte, what string) (ThemeState, error) {
	if len(body) < 16 {
		return ThemeState{}, decodeErr("%s ended inside its fixed fields", what)
	}
	tilePx := binary.LittleEndian.Uint32(body[0:4])
	scaleBits := binary.LittleEndian.Uint32(body[4:8])
	idLen := int(binary.LittleEndian.Uint16(body[8:10]))
	if body[10] != 0 || body[11] != 0 {
		return ThemeState{}, decodeErr("%s reserved field was not zero", what)
	}
	tomlLen := int(binary.LittleEndian.Uint32(body[12:16]))
	scale := math.Float32frombits(scaleBits)
	// The BadFloat rule: a scale a tile cannot be drawn at is rejected
	// in the codec, so a decoded state is always equal to itself and
	// "resend when changed" loops terminate.
	if math.IsNaN(float64(scale)) || math.IsInf(float64(scale), 0) || scale <= 0 || scale > MaxScale {
		return ThemeState{}, decodeErr("unusable scale (bits %#010x)", scaleBits)
	}
	if idLen > MaxThemeIDBytes {
		return ThemeState{}, decodeErr("theme_id of %d bytes is over its cap", idLen)
	}
	if tomlLen > MaxThemeTOMLBytes {
		return ThemeState{}, decodeErr("theme_toml of %d bytes is over its cap", tomlLen)
	}
	if len(body) != 16+idLen+tomlLen {
		return ThemeState{}, decodeErr("%s length %d disagrees with its declared strings", what, len(body))
	}
	id := body[16 : 16+idLen]
	toml := body[16+idLen:]
	if !utf8.Valid(id) || !utf8.Valid(toml) {
		return ThemeState{}, decodeErr("%s carries invalid UTF-8", what)
	}
	return ThemeState{TilePx: tilePx, Scale: scale, ThemeID: string(id), ThemeTOML: string(toml)}, nil
}

// DecodeServer decodes one shell->client datagram.
func DecodeServer(buf []byte) (ServerMessage, error) {
	var msg ServerMessage
	if len(buf) == 0 {
		return msg, decodeErr("empty message")
	}
	if len(buf) > MaxMessageBytes {
		return msg, decodeErr("message of %d bytes exceeds the cap", len(buf))
	}
	if len(buf) < 4 {
		return msg, decodeErr("message ended inside the header")
	}
	if buf[1] != 0 || buf[2] != 0 || buf[3] != 0 {
		return msg, decodeErr("reserved header bytes were not zero")
	}
	body := buf[4:]
	switch buf[0] {
	case kindWelcome, kindThemeChanged:
		theme, err := decodeThemeState(body, "Welcome/ThemeChanged")
		if err != nil {
			return msg, err
		}
		msg.Theme = theme
		if buf[0] == kindWelcome {
			msg.Kind = "welcome"
		} else {
			msg.Kind = "theme_changed"
		}
	case kindInput:
		if len(body) != 16 {
			return msg, decodeErr("Input is %d body bytes, want 16", len(body))
		}
		if body[0] < InputPress || body[0] > InputLeave {
			return msg, decodeErr("undefined input kind %d", body[0])
		}
		if body[1] > ButtonRight {
			return msg, decodeErr("undefined button %d", body[1])
		}
		if body[2] != 0 || body[3] != 0 {
			return msg, decodeErr("Input reserved field was not zero")
		}
		msg.Kind = "input"
		msg.Input = InputEvent{
			Kind:   body[0],
			Button: body[1],
			X:      int32(binary.LittleEndian.Uint32(body[4:8])),
			Y:      int32(binary.LittleEndian.Uint32(body[8:12])),
			Delta:  int32(binary.LittleEndian.Uint32(body[12:16])),
		}
	case kindVisibility:
		if len(body) != 4 {
			return msg, decodeErr("Visibility is %d body bytes, want 4", len(body))
		}
		if body[1] != 0 || body[2] != 0 || body[3] != 0 {
			return msg, decodeErr("Visibility reserved bytes were not zero")
		}
		if body[0] > 1 {
			return msg, decodeErr("undefined visibility value %d", body[0])
		}
		msg.Kind = "visibility"
		msg.Visible = body[0] == 1
	case kindPing:
		if len(body) != 4 {
			return msg, decodeErr("Ping is %d body bytes, want 4", len(body))
		}
		msg.Kind = "ping"
		msg.Seq = binary.LittleEndian.Uint32(body)
	case kindGoodbye:
		if len(body) != 4 {
			return msg, decodeErr("Goodbye is %d body bytes, want 4", len(body))
		}
		if body[1] != 0 || body[2] != 0 || body[3] != 0 {
			return msg, decodeErr("Goodbye reserved bytes were not zero")
		}
		if body[0] < uint8(GoodbyeShutdown) || body[0] > uint8(GoodbyeRemoved) {
			return msg, decodeErr("undefined goodbye reason %d", body[0])
		}
		msg.Kind = "goodbye"
		msg.Reason = GoodbyeReason(body[0])
	default:
		return msg, decodeErr("unknown message kind %#04x", buf[0])
	}
	return msg, nil
}
