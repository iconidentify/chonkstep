//! Original integer-grid title glyphs. Parse the checked-in drawings at compile
//! time, so neither layout nor painting allocates or reads a font for Latin-1.
#[derive(Clone, Copy)]
pub(super) struct Glyph {
    pub advance: u32,
    pub rows: [u16; 15],
}

const EMPTY: Glyph = Glyph { advance: 0, rows: [0; 15] };
const GLYPHS: [Glyph; 191] = parse(include_bytes!("title.atlas"));

pub(super) fn glyph(character: char) -> Option<&'static Glyph> {
    index(character as u32).map(|index| &GLYPHS[index])
}

const fn index(code: u32) -> Option<usize> {
    match code {
        0x20..=0x7e => Some((code - 0x20) as usize),
        0xa0..=0xff => Some((code - 0xa0 + 95) as usize),
        _ => None,
    }
}

const fn parse(bytes: &[u8]) -> [Glyph; 191] {
    let mut result = [EMPTY; 191];
    let mut cursor = 0;
    let mut count = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'#' || bytes[cursor] == b'\n' {
            while cursor < bytes.len() && bytes[cursor] != b'\n' { cursor += 1; }
            cursor += 1;
            continue;
        }
        let code = number(bytes, &mut cursor, 16);
        let advance = number(bytes, &mut cursor, 10);
        let mut row = number(bytes, &mut cursor, 10) as usize;
        assert!(advance > 0 && advance <= 16 && row < 15);
        let mut glyph = Glyph { advance, rows: [0; 15] };
        while cursor < bytes.len() && bytes[cursor] != b'\n' {
            let bits = number(bytes, &mut cursor, 16);
            assert!(row < 15 && bits < (1 << advance));
            glyph.rows[row] = bits as u16;
            row += 1;
        }
        let Some(index) = index(code) else { panic!("invalid atlas code point") };
        assert!(result[index].advance == 0);
        result[index] = glyph;
        count += 1;
        cursor += 1;
    }
    assert!(count == 191);
    result
}

const fn number(bytes: &[u8], cursor: &mut usize, radix: u32) -> u32 {
    while *cursor < bytes.len() && bytes[*cursor] == b' ' { *cursor += 1; }
    let mut value = 0;
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor] != b' ' && bytes[*cursor] != b'\n' {
        let digit = match bytes[*cursor] {
            b'0'..=b'9' => (bytes[*cursor] - b'0') as u32,
            b'a'..=b'f' => (bytes[*cursor] - b'a' + 10) as u32,
            _ => panic!("invalid atlas digit"),
        };
        assert!(digit < radix);
        value = value * radix + digit;
        *cursor += 1;
    }
    assert!(*cursor > start);
    value
}
