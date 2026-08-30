//! A second implementation of the format, reading rather than writing,
//! used to check the first.
//!
//! The unit tests in `format.rs` pin exact byte strings, which catches a
//! change to the encoder but shares the encoder's *understanding* of the
//! format: if the author mis-read the specification, the expected bytes
//! in those tests were mis-written the same way and every one of them
//! still passes. This file exists to break that symmetry. The parser
//! below is written from the specification's field table as a client
//! would write it — walking the property field by field, deriving every
//! offset from the lengths it has just read — and never from the
//! encoder's source. If the encoder's padding is wrong in a way the
//! unit tests agreed with, this parser walks off the end of a record and
//! the round trip fails.
//!
//! That is the same reasoning `chonk-dock-proto` applies to its codec:
//! the decoder is the thing that proves the encoder, and a format with
//! no decoder in the repository is a format nobody has read back.
//!
//! An integration test rather than a unit test on purpose — it can only
//! use the crate's public API, so it also checks that publishing a
//! complete settings map is something a caller can actually express.

use chonk_xsettings::format::{ByteOrder, serialize_with_byte_order};
use chonk_xsettings::{DesktopAppearance, SettingValue, Settings, keys};

/// One setting as a reader recovers it.
#[derive(Clone, Debug, PartialEq)]
struct ReadSetting {
    name: String,
    last_change_serial: u32,
    value: SettingValue,
}

/// The whole property as a reader recovers it.
#[derive(Clone, Debug, PartialEq)]
struct ReadProperty {
    byte_order: u8,
    serial: u32,
    settings: Vec<ReadSetting>,
}

/// A cursor over the property, deliberately strict: every accessor
/// checks that the bytes it needs are there, so a short or misaligned
/// property is a test failure with a position in it rather than a panic
/// from slice indexing.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    msb_first: bool,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], msb_first: bool) -> Self {
        Self {
            bytes,
            at: 0,
            msb_first,
        }
    }

    fn take(&mut self, count: usize) -> &'a [u8] {
        assert!(
            self.at + count <= self.bytes.len(),
            "property ended after {} bytes while reading {count} more at offset {}",
            self.bytes.len(),
            self.at
        );
        let slice = &self.bytes[self.at..self.at + count];
        self.at += count;
        slice
    }

    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }

    fn u16(&mut self) -> u16 {
        let bytes: [u8; 2] = self.take(2).try_into().unwrap();
        if self.msb_first {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        }
    }

    fn u32(&mut self) -> u32 {
        let bytes: [u8; 4] = self.take(4).try_into().unwrap();
        if self.msb_first {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    }

    fn i32(&mut self) -> i32 {
        self.u32() as i32
    }

    fn string(&mut self, len: usize) -> String {
        String::from_utf8(self.take(len).to_vec()).expect("a setting name or value must be UTF-8")
    }

    /// Skips the padding after a field of `len` bytes, insisting that it
    /// is zero — the encoder promises zeros, and a non-zero pad byte
    /// here would mean the reader and the writer disagree about where
    /// the padding starts.
    fn skip_padding(&mut self, len: usize) {
        let pad = 3 - ((len + 3) % 4);
        for byte in self.take(pad) {
            assert_eq!(*byte, 0, "pad bytes must be zero, at offset {}", self.at);
        }
        assert_eq!(self.at % 4, 0, "padding must restore four-byte alignment");
    }
}

/// Parses an `_XSETTINGS_SETTINGS` property body the way a client would.
fn parse(bytes: &[u8]) -> ReadProperty {
    // The byte-order byte is read before anything else can be, which is
    // the entire reason it is a single byte at offset zero.
    assert!(!bytes.is_empty(), "a property is never empty");
    let byte_order = bytes[0];
    assert!(byte_order <= 1, "byte order is 0 or 1");

    let mut reader = Reader::new(bytes, byte_order == 1);
    assert_eq!(reader.u8(), byte_order);
    for byte in reader.take(3) {
        assert_eq!(*byte, 0, "the three header pad bytes are zero");
    }
    let serial = reader.u32();
    let count = reader.u32();

    let mut settings = Vec::new();
    for index in 0..count {
        assert_eq!(
            reader.at % 4,
            0,
            "setting {index} does not start on a four-byte boundary"
        );
        let type_code = reader.u8();
        assert_eq!(reader.u8(), 0, "the per-setting pad byte is zero");
        let name_len = reader.u16() as usize;
        let name = reader.string(name_len);
        reader.skip_padding(name_len);
        let last_change_serial = reader.u32();

        let value = match type_code {
            0 => SettingValue::Integer(reader.i32()),
            1 => {
                let value_len = reader.u32() as usize;
                let value = reader.string(value_len);
                reader.skip_padding(value_len);
                SettingValue::String(value)
            }
            2 => {
                // red, blue, green, alpha — the order the specification
                // prints and the reference client reads.
                let red = reader.u16();
                let blue = reader.u16();
                let green = reader.u16();
                let alpha = reader.u16();
                SettingValue::Color {
                    red,
                    green,
                    blue,
                    alpha,
                }
            }
            other => panic!("unknown setting type {other} for setting {index}"),
        };

        settings.push(ReadSetting {
            name,
            last_change_serial,
            value,
        });
    }

    assert_eq!(
        reader.at,
        bytes.len(),
        "the property has {} trailing bytes the reader could not account for",
        bytes.len() - reader.at
    );

    ReadProperty {
        byte_order,
        serial,
        settings,
    }
}

/// Everything the encoder can express, in one map: both string-length
/// alignments, both name-length alignments, all three value types, and a
/// non-ASCII value.
fn kitchen_sink() -> Settings {
    let mut settings = Settings::new();
    settings.set("Xft/DPI", 196_608);
    settings.set("Gdk/WindowScalingFactor", 2);
    settings.set("Gtk/ThemeName", "NeXT");
    settings.set("Net/ThemeName", "NeXTSTEP-ish");
    settings.set("Gtk/FontName", "Grün Sans 10");
    settings.set("Gtk/CursorThemeName", "a");
    settings.set("A", "abc");
    settings.set(
        "Net/Accent",
        SettingValue::Color {
            red: 0x0102,
            green: 0x0304,
            blue: 0x0506,
            alpha: 0xffff,
        },
    );
    settings
}

fn expected(settings: &Settings) -> Vec<ReadSetting> {
    settings
        .iter()
        .map(|(name, value)| ReadSetting {
            name: name.to_string(),
            last_change_serial: settings.last_change_serial(name).unwrap(),
            value: value.clone(),
        })
        .collect()
}

#[test]
fn an_independent_reader_recovers_every_setting() {
    let settings = kitchen_sink();
    let parsed = parse(&settings.serialize());

    assert_eq!(parsed.byte_order, 0);
    assert_eq!(parsed.serial, settings.serial());
    assert_eq!(parsed.settings.len(), settings.len());
    assert_eq!(parsed.settings, expected(&settings));
}

#[test]
fn an_independent_reader_recovers_the_msb_first_encoding_too() {
    let settings = kitchen_sink();
    let parsed = parse(&serialize_with_byte_order(&settings, ByteOrder::MsbFirst));

    assert_eq!(parsed.byte_order, 1);
    assert_eq!(parsed.serial, settings.serial());
    assert_eq!(parsed.settings, expected(&settings));
}

#[test]
fn both_byte_orders_describe_the_same_settings() {
    let settings = kitchen_sink();
    let lsb = parse(&serialize_with_byte_order(&settings, ByteOrder::LsbFirst));
    let msb = parse(&serialize_with_byte_order(&settings, ByteOrder::MsbFirst));
    assert_eq!(lsb.settings, msb.settings);
    assert_eq!(lsb.serial, msb.serial);
}

#[test]
fn an_empty_property_reads_back_as_no_settings() {
    let parsed = parse(&Settings::new().serialize());
    assert_eq!(parsed.serial, 0);
    assert!(parsed.settings.is_empty());
}

#[test]
fn every_name_and_value_length_combination_survives_the_round_trip() {
    // Sixteen alignment combinations of the two variable-length fields,
    // one per property so a failure names the lengths involved.
    for name_len in 1..=8usize {
        for value_len in 0..=8usize {
            let name: String = std::iter::repeat_n('n', name_len).collect();
            let value: String = std::iter::repeat_n('v', value_len).collect();
            let mut settings = Settings::new();
            assert!(settings.set(&name, value.as_str()));

            let parsed = parse(&settings.serialize());
            assert_eq!(
                parsed.settings,
                vec![ReadSetting {
                    name: name.clone(),
                    last_change_serial: 1,
                    value: SettingValue::String(value.clone()),
                }],
                "name of {name_len} bytes with a value of {value_len} did not round trip"
            );
        }
    }
}

#[test]
fn a_published_appearance_reads_back_as_the_settings_it_promised() {
    // The end-to-end shape: what the desktop means, through the typed
    // layer, through the encoder, and back out as a client sees it.
    let appearance = DesktopAppearance::new(2.0, "NeXT")
        .with_icon_theme("NeXT-icons")
        .with_cursor_theme("Adwaita")
        .with_font_name("Sans 10");
    let settings = appearance.to_settings();
    let parsed = parse(&settings.serialize());

    let lookup = |name: &str| {
        parsed
            .settings
            .iter()
            .find(|setting| setting.name == name)
            .unwrap_or_else(|| panic!("{name} should have been published"))
            .value
            .clone()
    };

    assert_eq!(lookup(keys::XFT_DPI), SettingValue::Integer(196_608));
    assert_eq!(lookup(keys::GDK_UNSCALED_DPI), SettingValue::Integer(98_304));
    assert_eq!(
        lookup(keys::GDK_WINDOW_SCALING_FACTOR),
        SettingValue::Integer(2)
    );
    assert_eq!(lookup(keys::GTK_CURSOR_THEME_SIZE), SettingValue::Integer(48));
    assert_eq!(
        lookup(keys::NET_THEME_NAME),
        SettingValue::String("NeXT".to_string())
    );
    assert_eq!(
        lookup(keys::GTK_THEME_NAME),
        SettingValue::String("NeXT".to_string())
    );
    assert_eq!(
        lookup(keys::GTK_CURSOR_THEME_NAME),
        SettingValue::String("Adwaita".to_string())
    );
    assert_eq!(
        lookup(keys::GTK_FONT_NAME),
        SettingValue::String("Sans 10".to_string())
    );
}

#[test]
fn a_live_scale_change_is_visible_to_a_reader_as_a_higher_serial() {
    // The property a client re-reads after a `PropertyNotify`: the
    // header serial has moved, the settings that changed carry new
    // stamps, and the ones that did not still carry their old ones —
    // which is how a client avoids re-laying out every window for a
    // theme change it was not affected by.
    let mut settings = DesktopAppearance::new(1.0, "NeXT").to_settings();
    let before = parse(&settings.serialize());

    assert!(DesktopAppearance::new(2.0, "NeXT").apply_to(&mut settings));
    let after = parse(&settings.serialize());

    assert!(
        after.serial > before.serial,
        "a change must raise the manager serial"
    );

    let stamp = |property: &ReadProperty, name: &str| {
        property
            .settings
            .iter()
            .find(|setting| setting.name == name)
            .unwrap()
            .last_change_serial
    };
    assert!(stamp(&after, keys::XFT_DPI) > stamp(&before, keys::XFT_DPI));
    assert_eq!(
        stamp(&after, keys::GTK_THEME_NAME),
        stamp(&before, keys::GTK_THEME_NAME),
        "an unchanged setting must keep its stamp"
    );
    assert_eq!(
        stamp(&after, keys::GDK_UNSCALED_DPI),
        stamp(&before, keys::GDK_UNSCALED_DPI),
        "the unscaled DPI is constant across scales by design"
    );
}
