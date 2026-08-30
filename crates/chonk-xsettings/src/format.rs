//! The `_XSETTINGS_SETTINGS` property: the settings map and its exact
//! byte layout.
//!
//! Pure — no X connection, no atoms, no I/O. That separation is the
//! whole reason this module exists apart from [`crate::manager`]: the
//! binary format is the part that can be *wrong in a way nobody
//! notices*, and it is the only part that can be tested on a build
//! machine with no X server. Every client on the display parses these
//! bytes; a manager that gets the padding wrong does not produce an
//! error anywhere, it produces a GTK application whose font size came
//! out of the middle of a string. So the format lives here, behind
//! [`serialize`], with byte-exact tests, and the X code on top of it
//! does nothing but hand the resulting `Vec<u8>` to `ChangeProperty`.
//!
//! # The format
//!
//! From the XSETTINGS specification (freedesktop.org, `xsettings.txt`).
//! The property is stored on the manager's own window, with both the
//! property atom and the property *type* atom being
//! `_XSETTINGS_SETTINGS`, at format 8 — that is, the X server treats it
//! as an opaque byte string and performs no byte swapping of its own,
//! which is exactly why the format has to carry its own byte-order
//! marker.
//!
//! ```text
//! header
//!   CARD8    byte-order        0 = LSB first, 1 = MSB first
//!   BYTE     padding[3]        must be zero
//!   CARD32   serial            bumped on every change
//!   CARD32   n-settings
//! setting, repeated n-settings times
//!   CARD8    type              0 = integer, 1 = string, 2 = color
//!   BYTE     padding           must be zero
//!   CARD16   name-len
//!   STRING8  name[name-len]
//!   BYTE     padding[pad(name-len)]
//!   CARD32   last-change-serial
//!   <value, per type>
//! value, type = integer
//!   INT32    value
//! value, type = string
//!   CARD32   value-len
//!   STRING8  value[value-len]
//!   BYTE     padding[pad(value-len)]
//! value, type = color
//!   CARD16   red
//!   CARD16   blue              <- yes, blue. See `SettingValue::Color`.
//!   CARD16   green
//!   CARD16   alpha
//! ```
//!
//! where `pad(n) = (4 - n % 4) % 4`, written in the specification as
//! `3 - ((n + 3) mod 4)`. The two are the same function; the
//! `padding_for` helper below implements the first form and a test
//! asserts it against the second.
//!
//! # Why the padding is the dangerous part
//!
//! Note what the padding buys: the header is twelve bytes, so the first
//! setting starts on a four-byte boundary, and *every* variable-length
//! field is followed by enough padding to restore that boundary. The
//! consequence is that each setting record is itself a whole number of
//! four-byte words, and the reader can walk the list by adding record
//! sizes without ever re-deriving alignment.
//!
//! That is also what makes a padding bug so quiet. Get it wrong and the
//! bytes still parse — the reader just reads `last-change-serial` from
//! four bytes that were the tail of a name, then reads the next
//! setting's type from the middle of a value, and keeps going. There is
//! no checksum, no length prefix on the record, and no terminator. The
//! first symptom is a client rendering at some absurd DPI, three layers
//! away from the code that wrote the byte. Hence `padding_for` being
//! a named, separately tested function rather than an inline
//! expression, and hence the debug assertion after every setting record
//! that the buffer is still four-byte aligned.
//!
//! # Byte order
//!
//! [`serialize`] always writes LSB-first and stamps the header
//! accordingly. This is legal on any host: the byte-order field exists
//! precisely so that a manager may write its native order and leave the
//! swapping to readers, and every conforming client (the reference
//! `xsettings-client.c` that GTK vendors, Qt's own copy of it) checks
//! the field and swaps. Fixing it at LSB-first rather than using
//! `#[cfg(target_endian)]` means the bytes this crate produces are the
//! same everywhere, which is what makes the byte-exact tests below
//! meaningful rather than a tautology restating the host's endianness.
//! [`serialize_with_byte_order`] is there for the tests that check the
//! MSB path, and for a caller that has a reason to prefer it.

use std::collections::BTreeMap;

/// The longest setting *name* this crate will publish.
///
/// The format's `name-len` is a `CARD16`, so 65535 is the hard ceiling
/// the encoding imposes. Nothing anyone publishes comes close — the
/// longest name in real use is around twenty bytes — so this is a
/// bound on absurdity rather than a budget, and its only job is to make
/// the `as u16` cast in [`serialize`] provably lossless instead of
/// merely obviously lossless.
pub const MAX_NAME_BYTES: usize = u16::MAX as usize;

/// The longest string *value* this crate will publish.
///
/// The encoding's `value-len` is a `CARD32`, so the real constraint is
/// not the format but the X protocol: the whole property has to fit in
/// one `ChangeProperty` request, and a request that exceeds the
/// server's maximum length fails as a whole — taking every *other*
/// setting in the map down with the oversized one. Since the values
/// this desktop publishes are theme names and font descriptions, a
/// 64 KiB value is already a bug in the caller; refusing it here keeps
/// that bug local to one setting instead of blanking the property.
pub const MAX_STRING_BYTES: usize = 64 * 1024;

/// Which end of a multi-byte field comes first in the serialised
/// property.
///
/// Carried in the first byte of the header. A client is required to
/// read that byte and swap if it differs from its own order, so this is
/// a statement about the bytes, not a compatibility knob — both values
/// are equally correct and equally understood.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ByteOrder {
    /// Least significant byte first (x86, ARM as everyone runs it).
    /// Encoded as `0`.
    #[default]
    LsbFirst,
    /// Most significant byte first. Encoded as `1`.
    MsbFirst,
}

impl ByteOrder {
    /// The value the header's first byte carries for this order.
    pub fn code(self) -> u8 {
        match self {
            Self::LsbFirst => 0,
            Self::MsbFirst => 1,
        }
    }

    fn u16(self, value: u16) -> [u8; 2] {
        match self {
            Self::LsbFirst => value.to_le_bytes(),
            Self::MsbFirst => value.to_be_bytes(),
        }
    }

    fn u32(self, value: u32) -> [u8; 4] {
        match self {
            Self::LsbFirst => value.to_le_bytes(),
            Self::MsbFirst => value.to_be_bytes(),
        }
    }
}

/// The type codes the specification assigns, named so that the match in
/// [`serialize`] reads as the table it is transcribing.
const TYPE_INTEGER: u8 = 0;
const TYPE_STRING: u8 = 1;
const TYPE_COLOR: u8 = 2;

/// One setting's value.
///
/// The specification defines exactly these three types and no
/// extension mechanism, so this enum is closed and will stay closed: a
/// fourth type code is a new revision of the specification, not a new
/// variant someone adds locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingValue {
    /// A signed 32-bit integer. Carries everything numeric XSETTINGS
    /// publishes, including the several quantities that are really
    /// fixed-point — `Xft/DPI` is a DPI in 1024ths of a point, and
    /// there is no float type to put it in. See
    /// [`crate::appearance`], which owns those conversions so that
    /// callers never write the 1024 themselves.
    Integer(i32),
    /// A byte string. The specification calls it `STRING8` and says
    /// nothing about encoding; every real client treats theme and font
    /// names as UTF-8, and holding a Rust `String` makes that the only
    /// thing this crate can produce.
    String(String),
    /// An RGBA colour, each channel a `CARD16`.
    ///
    /// **The wire order is red, blue, green, alpha.** That is not a
    /// typo here and it is not a typo in the specification either: the
    /// published grammar lists `blue` second, and the reference client
    /// implementation that GTK and Qt both vendor
    /// (`xsettings-client.c`) fetches them in that same order. Two
    /// implementations agreeing on a mistake is a format. Writing the
    /// "obvious" R, G, B, A order here would swap green and blue in
    /// every client on the display, which is the sort of bug that gets
    /// blamed on a theme for a week. [`serialize`] does it correctly
    /// and a test pins it.
    ///
    /// Nothing this desktop publishes today is a colour; the variant
    /// exists so that the module is a complete implementation of the
    /// format rather than a subset that silently truncates a map
    /// somebody extends later.
    Color {
        /// Red channel, 0..=65535.
        red: u16,
        /// Green channel, 0..=65535.
        green: u16,
        /// Blue channel, 0..=65535.
        blue: u16,
        /// Alpha channel, 0..=65535. The specification has no notion of
        /// premultiplication; 65535 is opaque.
        alpha: u16,
    },
}

impl SettingValue {
    /// The specification's type code for this variant.
    pub fn type_code(&self) -> u8 {
        match self {
            Self::Integer(_) => TYPE_INTEGER,
            Self::String(_) => TYPE_STRING,
            Self::Color { .. } => TYPE_COLOR,
        }
    }

    /// Whether this value is one the format can carry — see
    /// [`MAX_STRING_BYTES`]. Integers and colours are fixed width and
    /// always representable.
    fn is_representable(&self) -> bool {
        match self {
            Self::Integer(_) | Self::Color { .. } => true,
            Self::String(s) => s.len() <= MAX_STRING_BYTES,
        }
    }
}

impl From<i32> for SettingValue {
    fn from(value: i32) -> Self {
        Self::Integer(value)
    }
}

impl From<String> for SettingValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for SettingValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

/// A setting together with the serial at which it last changed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    value: SettingValue,
    last_change_serial: u32,
}

/// The settings map, plus the serial number that tells clients how much
/// of it they already know.
///
/// # What the serial is for
///
/// A client watches the manager window for `PropertyNotify` and re-reads
/// the whole property on every change — there is no partial update in
/// this protocol. The serials are how it avoids acting on settings that
/// did not actually move: it remembers the header serial it last read,
/// and treats a setting as changed when that setting's
/// `last-change-serial` is newer. Toolkits act on this. GTK re-reads a
/// font, re-renders every widget and re-lays out every window when
/// `Xft/DPI` changes, so a manager that stamped every setting as new on
/// every write would make an unrelated theme change cost a full
/// relayout in every application on the display.
///
/// # Why every mutation bumps the serial immediately
///
/// [`Settings::set`] increments the serial the moment a value actually
/// changes, and stamps that setting with the new value. A caller
/// changing three settings before publishing therefore advances the
/// serial by three and leaves three different `last-change-serial`
/// stamps behind.
///
/// The alternative — stage the changes, then assign one serial to the
/// whole batch at publish time — produces prettier numbers and a worse
/// API: it makes a `Settings` that has been mutated but not yet
/// committed serialise to something stale, which is a footgun in a type
/// whose entire job is to be serialised. The specification asks only
/// that the serial increase when settings change; it says nothing about
/// batches. Clients compare for *difference*, never for adjacency, so
/// gaps are free. Monotonicity is the property that matters, and it is
/// the one this design cannot get wrong.
///
/// Wrapping is deliberate (`wrapping_add`). At one change per serial a
/// wrap needs four billion of them, and the alternative — saturating —
/// would silently stop reporting changes at the ceiling, which is a
/// worse failure than the single missed update a wrap could theoretically
/// cause.
///
/// # Why the entries are ordered
///
/// A `BTreeMap`, not a `HashMap`. The specification does not care what
/// order settings appear in, but a stable order means the serialised
/// bytes are a function of the map's contents alone, which is what
/// makes the byte-exact tests in this module possible to write at all,
/// and what stops an unrelated edit from producing a diff-sized change
/// in the property.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    entries: BTreeMap<String, Entry>,
    serial: u32,
}

impl Settings {
    /// An empty map at serial 0.
    ///
    /// Serial 0 is never published with content: the first successful
    /// [`set`](Settings::set) takes it to 1. That is not required by the
    /// specification, it just means "serial 0" unambiguously reads as
    /// "nothing has ever been set" in a log line.
    pub fn new() -> Self {
        Self::default()
    }

    /// The manager serial: the value written into the property header.
    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// How many settings are in the map.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map has no settings. A perfectly legal thing to
    /// publish — see the empty-map test — and what a manager that has
    /// acquired the selection but not yet been told anything should put
    /// on the window, so that a client which arrives first finds a
    /// well-formed property rather than none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The current value of one setting, if it is set.
    pub fn get(&self, name: &str) -> Option<&SettingValue> {
        self.entries.get(name).map(|entry| &entry.value)
    }

    /// The serial at which one setting last changed — the value written
    /// into that setting's record. Exposed mostly so tests can assert
    /// the stamping rule without reparsing the property.
    pub fn last_change_serial(&self, name: &str) -> Option<u32> {
        self.entries.get(name).map(|entry| entry.last_change_serial)
    }

    /// Every setting, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SettingValue)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.as_str(), &entry.value))
    }

    /// Sets one setting, returning whether this changed anything.
    ///
    /// Setting a value equal to the one already stored is a no-op: it
    /// does not touch the serial and returns `false`. That is what lets
    /// a caller re-apply a whole [`crate::appearance::DesktopAppearance`]
    /// unconditionally and still only write the property when something
    /// really moved — see [`crate::manager::XSettingsManager::update`],
    /// which uses the serial to decide whether an X round trip is
    /// needed at all.
    ///
    /// # Refusals
    ///
    /// A name or value the format cannot carry is refused: the map is
    /// left untouched, `false` is returned, and the reason is logged at
    /// warn level. It is not a `Result` and it is not a panic, and both
    /// of those are deliberate. Every name this crate publishes comes
    /// from a constant in [`crate::appearance::keys`], so a refusal
    /// means a caller has invented a name or handed over a pathological
    /// string; the right response is to keep publishing the settings
    /// that *are* valid rather than to blank the property or take the
    /// desktop down. See [`is_valid_name`] and [`MAX_STRING_BYTES`].
    pub fn set(&mut self, name: &str, value: impl Into<SettingValue>) -> bool {
        let value = value.into();
        if !is_valid_name(name) {
            tracing::warn!(
                name,
                "refusing to publish an XSETTINGS setting whose name the format cannot carry"
            );
            return false;
        }
        if !value.is_representable() {
            tracing::warn!(
                name,
                max = MAX_STRING_BYTES,
                "refusing to publish an oversized XSETTINGS string value; the whole property has \
                 to fit in one ChangeProperty request"
            );
            return false;
        }

        if self.entries.get(name).is_some_and(|e| e.value == value) {
            return false;
        }
        self.serial = self.serial.wrapping_add(1);
        self.entries.insert(
            name.to_string(),
            Entry {
                value,
                last_change_serial: self.serial,
            },
        );
        true
    }

    /// Removes one setting, returning whether it was there.
    ///
    /// Worth knowing what this does and does not achieve. Removing a
    /// setting bumps the serial and the setting stops appearing in the
    /// property, which is the correct way to say "this desktop no longer
    /// expresses an opinion about the cursor theme" — a client that
    /// starts afterwards falls back to its own default. A client that
    /// was *already running* generally will not: the reference client
    /// implementation merges what it reads into what it has, so a
    /// setting that disappears is usually remembered until the
    /// application restarts. That asymmetry is a property of the
    /// protocol, not of this function, and it is why
    /// [`crate::appearance::DesktopAppearance`] prefers publishing an
    /// explicit value over leaving a key out.
    pub fn remove(&mut self, name: &str) -> bool {
        if self.entries.remove(name).is_some() {
            self.serial = self.serial.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// The `_XSETTINGS_SETTINGS` property body for this map, LSB-first.
    /// Shorthand for [`serialize`].
    pub fn serialize(&self) -> Vec<u8> {
        serialize(self)
    }
}

/// Whether a setting name is one the format and the specification allow.
///
/// The specification gives names a grammar: components separated by
/// `/`, each component made of the characters `[A-Za-z0-9_-]`. This
/// checks exactly that, plus the encoding's own [`MAX_NAME_BYTES`]
/// ceiling and non-emptiness.
///
/// The one rule from the published grammar not enforced here is that a
/// component must *begin* with a letter. That is left out on purpose:
/// it rejects nothing anybody publishes, and the cost of being wrong in
/// that direction — silently dropping a key some future toolkit
/// invents — is worse than the cost of publishing a name a pedantic
/// client might not have expected. The characters, by contrast, are
/// checked strictly, because a name containing a NUL or a space is
/// genuinely un-parseable at the far end.
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    name.split('/').all(|component| {
        !component.is_empty()
            && component
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// Bytes of padding needed after a field of `len` bytes to restore
/// four-byte alignment.
///
/// The specification writes this as `3 - ((len + 3) mod 4)`, which is
/// the same function as `(4 - len % 4) % 4` for every `len`; the tests
/// assert the equivalence over a range rather than leaving the reader to
/// take it on faith, because the specification's form is the one that
/// gets transcribed by hand and mis-transcribed by hand.
fn padding_for(len: usize) -> usize {
    (4 - len % 4) % 4
}

/// A small append-only cursor that knows the byte order it is writing.
///
/// Exists so that the body of [`serialize`] reads as a transcription of
/// the specification's table with nothing in between — no `extend_from_slice`
/// of a `to_le_bytes()` at every field, and no chance of one field
/// quietly using a different order from its neighbour.
struct Writer {
    bytes: Vec<u8>,
    byte_order: ByteOrder,
}

impl Writer {
    fn new(byte_order: ByteOrder) -> Self {
        Self {
            bytes: Vec::new(),
            byte_order,
        }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&self.byte_order.u16(value));
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&self.byte_order.u32(value));
    }

    /// `INT32` is the same four bytes as `CARD32` in two's complement,
    /// which is what X11 and every client assume; going through
    /// `as u32` states that rather than relying on `i32::to_le_bytes`
    /// being reached by a different path from every other field.
    fn i32(&mut self, value: i32) {
        self.u32(value as u32);
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    /// Zero padding. The specification says the pad bytes are
    /// unspecified, but writing zeros makes the output a pure function
    /// of the input, which is the property the byte-exact tests rest on
    /// — and makes an `xxd` of the property readable by a human
    /// debugging a client.
    fn pad(&mut self, count: usize) {
        self.bytes.resize(self.bytes.len() + count, 0);
    }
}

/// Serialises a settings map into the `_XSETTINGS_SETTINGS` property
/// body, LSB-first.
///
/// This is the pure core of the crate: give it a map, get the exact
/// bytes that go on the window. No connection, no atoms, no failure
/// mode — every `Settings` is serialisable, because
/// [`Settings::set`] refused anything that would not have been.
pub fn serialize(settings: &Settings) -> Vec<u8> {
    serialize_with_byte_order(settings, ByteOrder::LsbFirst)
}

/// [`serialize`], with the header's byte order chosen explicitly.
///
/// Both orders are conforming and every real client handles both, so
/// this is not a compatibility switch; it exists so the MSB-first path
/// through the writer is exercised by a test rather than being code
/// that has never run.
pub fn serialize_with_byte_order(settings: &Settings, byte_order: ByteOrder) -> Vec<u8> {
    let mut w = Writer::new(byte_order);

    // Header. The three padding bytes are part of the format, not
    // alignment slack this code chose: a reader skips exactly three.
    w.u8(byte_order.code());
    w.pad(3);
    w.u32(settings.serial);
    // `len()` is bounded by the map, and a map large enough to overflow
    // a CARD32 would have exhausted memory long before; the cast is
    // written with `as` rather than `try_into().unwrap()` because there
    // is no reachable failure to report.
    w.u32(settings.entries.len() as u32);
    debug_assert_eq!(w.bytes.len(), 12, "the XSETTINGS header is twelve bytes");

    for (name, entry) in &settings.entries {
        w.u8(entry.value.type_code());
        // Per-setting padding byte. Specified as unused; zero.
        w.pad(1);
        // Lossless: `Settings::set` enforced `MAX_NAME_BYTES`.
        w.u16(name.len() as u16);
        w.raw(name.as_bytes());
        w.pad(padding_for(name.len()));
        w.u32(entry.last_change_serial);

        match &entry.value {
            SettingValue::Integer(value) => w.i32(*value),
            SettingValue::String(value) => {
                // Lossless: `Settings::set` enforced `MAX_STRING_BYTES`.
                w.u32(value.len() as u32);
                w.raw(value.as_bytes());
                w.pad(padding_for(value.len()));
            }
            SettingValue::Color {
                red,
                green,
                blue,
                alpha,
            } => {
                // red, BLUE, green, alpha — see `SettingValue::Color`.
                w.u16(*red);
                w.u16(*blue);
                w.u16(*green);
                w.u16(*alpha);
            }
        }

        // The invariant the whole format rests on: a setting record is
        // a whole number of four-byte words, so the next one starts
        // aligned. If this ever fires, a `pad` call above is missing or
        // wrong, and every client on the display is about to read
        // garbage.
        debug_assert_eq!(
            w.bytes.len() % 4,
            0,
            "setting {name:?} left the buffer unaligned"
        );
    }

    w.bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The specification's own expression for the padding rule, kept
    /// separate from the implementation so the two can be compared
    /// rather than assumed identical.
    fn padding_per_spec(len: usize) -> usize {
        3 - ((len + 3) % 4)
    }

    #[test]
    fn the_padding_rule_matches_the_expression_the_specification_prints() {
        for len in 0..=256 {
            assert_eq!(
                padding_for(len),
                padding_per_spec(len),
                "padding disagrees at len {len}"
            );
        }
    }

    #[test]
    fn padding_covers_every_alignment_case() {
        assert_eq!(padding_for(0), 0);
        assert_eq!(padding_for(1), 3);
        assert_eq!(padding_for(2), 2);
        assert_eq!(padding_for(3), 1);
        assert_eq!(padding_for(4), 0);
        assert_eq!(padding_for(5), 3);
        assert_eq!(padding_for(6), 2);
        assert_eq!(padding_for(7), 1);
        assert_eq!(padding_for(8), 0);
    }

    #[test]
    fn an_empty_map_serialises_to_a_bare_header() {
        let settings = Settings::new();
        assert_eq!(
            settings.serialize(),
            vec![
                0, // LSB first
                0, 0, 0, // header padding
                0, 0, 0, 0, // serial 0
                0, 0, 0, 0, // zero settings
            ]
        );
    }

    #[test]
    fn one_integer_setting_is_byte_exact() {
        let mut settings = Settings::new();
        assert!(settings.set("Xft/DPI", 98304));

        // Xft/DPI is seven bytes, so it needs one pad byte: the case
        // that exercises the "name is not a multiple of four" branch.
        let expected: Vec<u8> = vec![
            0, // LSB first
            0, 0, 0, // header padding
            1, 0, 0, 0, // serial 1 (one successful `set`)
            1, 0, 0, 0, // one setting
            0,    // type = integer
            0,    // per-setting padding
            7, 0, // name-len = 7
            b'X', b'f', b't', b'/', b'D', b'P', b'I', //
            0, // one pad byte back to alignment
            1, 0, 0, 0, // last-change-serial = 1
            0, 128, 1, 0, // 98304 = 0x00018000, LSB first
        ];
        assert_eq!(settings.serialize(), expected);
        assert_eq!(expected.len() % 4, 0);
    }

    #[test]
    fn a_string_whose_length_is_a_multiple_of_four_gets_no_value_padding() {
        // "Gtk/ThemeName" is 13 bytes (3 pad), "NeXT" is 4 (0 pad).
        let mut settings = Settings::new();
        assert!(settings.set("Gtk/ThemeName", "NeXT"));

        let expected: Vec<u8> = vec![
            0, 0, 0, 0, // byte order + header padding
            1, 0, 0, 0, // serial
            1, 0, 0, 0, // one setting
            1,    // type = string
            0,    // per-setting padding
            13, 0, // name-len
            b'G', b't', b'k', b'/', b'T', b'h', b'e', b'm', b'e', b'N', b'a', b'm', b'e', //
            0, 0, 0, // three pad bytes: 13 % 4 == 1
            1, 0, 0, 0, // last-change-serial
            4, 0, 0, 0, // value-len
            b'N', b'e', b'X', b'T',
            // and no value padding at all, because 4 % 4 == 0
        ];
        assert_eq!(settings.serialize(), expected);
    }

    #[test]
    fn a_string_whose_length_is_not_a_multiple_of_four_is_padded_to_the_next_word() {
        for (value, expected_pad) in [("", 0), ("a", 3), ("ab", 2), ("abc", 1), ("abcd", 0)] {
            let mut settings = Settings::new();
            assert!(settings.set("Gtk/ThemeName", value));
            let bytes = settings.serialize();

            // header 12 + type/pad/name-len 4 + name 13 + pad 3 +
            // serial 4 + value-len 4 = 40 bytes before the value.
            let value_start = 40;
            assert_eq!(&bytes[value_start..value_start + value.len()], value.as_bytes());
            let padding = &bytes[value_start + value.len()..];
            assert_eq!(
                padding.len(),
                expected_pad,
                "value {value:?} should get {expected_pad} pad bytes"
            );
            assert!(padding.iter().all(|&b| b == 0), "pad bytes must be zero");
            assert_eq!(bytes.len() % 4, 0, "value {value:?} left the property unaligned");
        }
    }

    #[test]
    fn every_name_length_leaves_the_record_word_aligned() {
        // Names of every length modulo 4, each carrying a value of
        // every length modulo 4, so all sixteen alignment combinations
        // of the two variable-length fields are covered.
        for name_len in 1..=9usize {
            for value_len in 0..=5usize {
                let name: String = std::iter::repeat_n('a', name_len).collect();
                let value: String = std::iter::repeat_n('z', value_len).collect();
                let mut settings = Settings::new();
                assert!(settings.set(&name, value.as_str()));
                let bytes = settings.serialize();
                assert_eq!(
                    bytes.len() % 4,
                    0,
                    "name of {name_len} and value of {value_len} left the property unaligned"
                );
                // 12 header + 4 + name + pad + 4 serial + 4 len + value + pad
                let expected_len = 12
                    + 4
                    + name_len
                    + padding_for(name_len)
                    + 4
                    + 4
                    + value_len
                    + padding_for(value_len);
                assert_eq!(bytes.len(), expected_len);
            }
        }
    }

    #[test]
    fn several_settings_are_written_in_name_order_and_counted() {
        let mut settings = Settings::new();
        assert!(settings.set("Xft/DPI", 98304));
        assert!(settings.set("Gtk/ThemeName", "NeXT"));
        assert!(settings.set("Gdk/WindowScalingFactor", 1));

        let bytes = settings.serialize();
        assert_eq!(&bytes[8..12], &[3, 0, 0, 0], "three settings");

        // Name order, not insertion order: Gdk < Gtk < Xft.
        let text = String::from_utf8_lossy(&bytes).to_string();
        let gdk = text.find("Gdk/WindowScalingFactor").expect("Gdk key present");
        let gtk = text.find("Gtk/ThemeName").expect("Gtk key present");
        let xft = text.find("Xft/DPI").expect("Xft key present");
        assert!(gdk < gtk && gtk < xft, "settings must be in name order");

        assert_eq!(
            settings.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            ["Gdk/WindowScalingFactor", "Gtk/ThemeName", "Xft/DPI"]
        );
    }

    #[test]
    fn the_colour_value_is_written_red_blue_green_alpha() {
        let mut settings = Settings::new();
        assert!(settings.set(
            "Net/Accent",
            SettingValue::Color {
                red: 0x1111,
                green: 0x2222,
                blue: 0x3333,
                alpha: 0x4444,
            }
        ));
        let bytes = settings.serialize();
        // header 12 + 4 + name "Net/Accent" (10) + pad 2 + serial 4 = 32
        assert_eq!(
            &bytes[32..],
            &[0x11, 0x11, 0x33, 0x33, 0x22, 0x22, 0x44, 0x44],
            "the specification and its reference client both order the channels R, B, G, A"
        );
    }

    #[test]
    fn msb_first_swaps_every_multi_byte_field_and_stamps_the_header() {
        let mut settings = Settings::new();
        assert!(settings.set("Xft/DPI", 98304));

        let expected: Vec<u8> = vec![
            1, // MSB first
            0, 0, 0, //
            0, 0, 0, 1, // serial 1
            0, 0, 0, 1, // one setting
            0, 0, // type, padding
            0, 7, // name-len, big-endian
            b'X', b'f', b't', b'/', b'D', b'P', b'I', 0, //
            0, 0, 0, 1, // last-change-serial
            0, 1, 128, 0, // 98304 big-endian
        ];
        assert_eq!(
            serialize_with_byte_order(&settings, ByteOrder::MsbFirst),
            expected
        );
    }

    #[test]
    fn setting_the_same_value_again_does_not_bump_the_serial() {
        let mut settings = Settings::new();
        assert!(settings.set("Xft/DPI", 98304));
        assert_eq!(settings.serial(), 1);

        assert!(!settings.set("Xft/DPI", 98304), "an unchanged value is a no-op");
        assert_eq!(settings.serial(), 1);
        assert_eq!(settings.serialize(), settings.serialize());

        assert!(settings.set("Xft/DPI", 196608), "a new value is a change");
        assert_eq!(settings.serial(), 2);
    }

    #[test]
    fn a_changed_setting_is_stamped_with_the_new_serial_and_its_neighbours_are_not() {
        let mut settings = Settings::new();
        settings.set("Gtk/ThemeName", "NeXT");
        settings.set("Xft/DPI", 98304);
        assert_eq!(settings.serial(), 2);
        assert_eq!(settings.last_change_serial("Gtk/ThemeName"), Some(1));
        assert_eq!(settings.last_change_serial("Xft/DPI"), Some(2));

        settings.set("Xft/DPI", 196608);
        assert_eq!(settings.serial(), 3);
        assert_eq!(
            settings.last_change_serial("Gtk/ThemeName"),
            Some(1),
            "an untouched setting keeps its old serial, or every client relayouts for nothing"
        );
        assert_eq!(settings.last_change_serial("Xft/DPI"), Some(3));

        // And the property agrees with the accessors.
        let bytes = settings.serialize();
        assert_eq!(&bytes[4..8], &[3, 0, 0, 0], "header carries the manager serial");
    }

    #[test]
    fn a_type_change_on_the_same_name_is_a_change() {
        let mut settings = Settings::new();
        assert!(settings.set("Net/ThemeName", "NeXT"));
        assert!(settings.set("Net/ThemeName", 4));
        assert_eq!(settings.serial(), 2);
        assert_eq!(settings.get("Net/ThemeName"), Some(&SettingValue::Integer(4)));
    }

    #[test]
    fn removing_a_setting_bumps_the_serial_and_shrinks_the_count() {
        let mut settings = Settings::new();
        settings.set("Gtk/ThemeName", "NeXT");
        settings.set("Xft/DPI", 98304);
        assert!(settings.remove("Gtk/ThemeName"));
        assert_eq!(settings.serial(), 3);
        assert_eq!(settings.len(), 1);
        assert_eq!(settings.serialize()[8..12], [1, 0, 0, 0]);

        assert!(
            !settings.remove("Gtk/ThemeName"),
            "removing what is not there changes nothing"
        );
        assert_eq!(settings.serial(), 3);
    }

    #[test]
    fn an_invalid_name_is_refused_without_touching_the_map() {
        let mut settings = Settings::new();
        settings.set("Xft/DPI", 98304);
        let before = settings.serialize();

        for name in ["", "Xft//DPI", "/Xft", "Xft/", "Xft DPI", "Xft/DPI\0", "Xft/DÜI"] {
            assert!(!settings.set(name, 1), "{name:?} should be refused");
        }
        assert_eq!(settings.serialize(), before, "a refusal must not disturb the map");
        assert_eq!(settings.serial(), 1);
    }

    #[test]
    fn name_validation_accepts_what_the_specification_allows() {
        for name in [
            "Xft/DPI",
            "Gdk/WindowScalingFactor",
            "Net/ThemeName",
            "Gtk/Cursor_Theme-Name",
            "A",
            "a/b/c/d",
        ] {
            assert!(is_valid_name(name), "{name:?} should be accepted");
        }
        assert!(!is_valid_name(&"a".repeat(MAX_NAME_BYTES + 1)));
    }

    #[test]
    fn an_oversized_string_value_is_refused() {
        let mut settings = Settings::new();
        assert!(settings.set("Gtk/ThemeName", "x".repeat(MAX_STRING_BYTES).as_str()));
        assert_eq!(settings.serial(), 1);

        assert!(!settings.set("Gtk/ThemeName", "x".repeat(MAX_STRING_BYTES + 1).as_str()));
        assert_eq!(settings.serial(), 1, "a refusal must not bump the serial");
        assert_eq!(
            settings.get("Gtk/ThemeName").map(|v| match v {
                SettingValue::String(s) => s.len(),
                _ => unreachable!(),
            }),
            Some(MAX_STRING_BYTES)
        );
    }

    #[test]
    fn a_utf8_value_is_measured_in_bytes_not_characters() {
        // The length field and the padding are both byte counts. A
        // name is ASCII by the grammar, but a *value* need not be, and
        // measuring a theme name in `chars()` would under-count the
        // length field and leave the reader one word out of step.
        let mut settings = Settings::new();
        assert!(settings.set("Gtk/ThemeName", "Grün")); // 5 bytes, 4 chars
        let bytes = settings.serialize();
        assert_eq!(&bytes[36..40], &[5, 0, 0, 0], "value-len counts bytes");
        assert_eq!(&bytes[40..45], "Grün".as_bytes());
        assert_eq!(bytes.len(), 48, "5 bytes of value plus 3 of padding");
    }
}
