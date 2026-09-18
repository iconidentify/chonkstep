//! Allocation-free field boundaries and lazy decoding for nmcli terse output.
//! Escapes apply to the next Unicode character; a dangling escape is ignored,
//! preserving the original parser's tolerant behavior on malformed text.

use std::borrow::Cow;

pub(super) fn fields(line: &str) -> Fields<'_> {
    Fields {
        remaining: Some(line),
    }
}

pub(super) struct Fields<'a> {
    remaining: Option<&'a str>,
}

impl<'a> Iterator for Fields<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.remaining.take()?;
        let mut chars = line.char_indices();
        while let Some((position, character)) = chars.next() {
            match character {
                '\\' => {
                    chars.next();
                }
                ':' => {
                    self.remaining = Some(&line[position + 1..]);
                    return Some(&line[..position]);
                }
                _ => {}
            }
        }
        Some(line)
    }
}

pub(super) fn decode(field: &str) -> Cow<'_, str> {
    if !field.contains('\\') {
        return Cow::Borrowed(field);
    }
    let mut decoded = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(escaped) = chars.next() {
                decoded.push(escaped);
            }
        } else {
            decoded.push(character);
        }
    }
    Cow::Owned(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original_split(line: &str) -> Vec<String> {
        let mut fields = vec![String::new()];
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        fields.last_mut().unwrap().push(escaped);
                    }
                }
                ':' => fields.push(String::new()),
                _ => fields.last_mut().unwrap().push(c),
            }
        }
        fields
    }

    #[test]
    fn all_short_escape_and_unicode_inputs_match_the_original_parser() {
        let alphabet = ['a', ':', '\\', 'é', '界'];
        for length in 0..=6 {
            for mut encoded in 0..alphabet.len().pow(length) {
                let mut input = String::new();
                for _ in 0..length {
                    input.push(alphabet[encoded % alphabet.len()]);
                    encoded /= alphabet.len();
                }
                let actual: Vec<_> = fields(&input).map(decode).collect();
                assert_eq!(actual, original_split(&input), "input {input:?}");
            }
        }
    }

    #[test]
    fn plain_fields_borrow_and_only_escaped_fields_allocate() {
        let actual: Vec<_> = fields(r"plain:Lab\:5G:界::").map(decode).collect();
        assert_eq!(actual, ["plain", "Lab:5G", "界", "", ""]);
        assert!(matches!(actual[0], Cow::Borrowed(_)));
        assert!(matches!(actual[1], Cow::Owned(_)));
        assert!(matches!(actual[2], Cow::Borrowed(_)));
        assert!(matches!(actual[3], Cow::Borrowed(_)));
        assert!(matches!(actual[4], Cow::Borrowed(_)));
    }
}
