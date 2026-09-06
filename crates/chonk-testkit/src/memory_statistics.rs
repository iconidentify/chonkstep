//! Versioned, numeric-only diagnostics from an opt-in allocation-profile build.

use std::collections::BTreeMap;

const REQUIRED: &[&str] = &[
    "version",
    "rust_live_bytes",
    "rust_peak_bytes",
    "rust_operations",
    "rust_allocated_bytes",
    "glibc_supported",
    "glibc_arena_bytes",
    "glibc_in_use_bytes",
    "glibc_free_bytes",
    "glibc_mapped_bytes",
    "glibc_top_releasable_bytes",
    "font_faces",
    "glyph_images",
    "glyph_image_bytes",
    "glyph_outlines",
    "glyph_outline_bytes",
];

/// Numeric allocator/cache snapshot with byte units explicit in field names.
/// Missing, duplicate or malformed fields are errors, never zero measurements.
#[derive(Clone, Debug)]
pub struct MemoryStatistics(BTreeMap<String, u64>);

impl MemoryStatistics {
    /// Read a named measurement. Unknown fields are absent, not zero.
    pub fn get(&self, name: &str) -> Option<u64> {
        self.0.get(name).copied()
    }

    /// Consume the validated snapshot as ordinary numeric fields for recording.
    pub fn into_values(self) -> BTreeMap<String, u64> {
        self.0
    }

    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let payload = line
            .strip_prefix("memory-stats ")
            .ok_or_else(|| format!("unexpected memory-statistics reply: {line}"))?;
        let mut values = BTreeMap::new();
        for pair in payload.split_whitespace() {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("malformed memory-statistics field: {pair}"))?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                return Err(format!("invalid memory-statistics key: {key}"));
            }
            let value = value
                .parse::<u64>()
                .map_err(|error| format!("invalid memory-statistics value for {key}: {error}"))?;
            if values.insert(key.to_owned(), value).is_some() {
                return Err(format!("duplicate memory-statistics field: {key}"));
            }
        }
        for name in REQUIRED {
            if !values.contains_key(*name) {
                return Err(format!("missing memory-statistics field: {name}"));
            }
        }
        if values["version"] != 1 || values["glibc_supported"] > 1 {
            return Err("unsupported memory-statistics version/capability".into());
        }
        Ok(Self(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> String {
        let fields = REQUIRED
            .iter()
            .map(|name| format!("{name}={}", u8::from(*name == "version")))
            .collect::<Vec<_>>()
            .join(" ");
        format!("memory-stats {fields}")
    }

    #[test]
    fn complete_numeric_snapshot_preserves_large_counts_and_serializes_as_an_object() {
        let line = valid().replace(
            "rust_allocated_bytes=0",
            "rust_allocated_bytes=18446744073709551615",
        );
        let parsed = MemoryStatistics::parse(&line).unwrap();
        assert_eq!(parsed.get("rust_allocated_bytes"), Some(u64::MAX));
        assert_eq!(parsed.get("absent"), None);
        assert_eq!(
            serde_json::to_value(parsed.into_values()).unwrap()["version"],
            1
        );
    }

    #[test]
    fn incomplete_unsupported_duplicate_and_nonnumeric_replies_are_not_measurements() {
        for line in [
            "err unknown command".into(),
            valid().replace("rust_live_bytes=0 ", ""),
            valid().replace("version=1", "version=2"),
            valid().replace("glibc_supported=0", "glibc_supported=2"),
            valid().replace("rust_live_bytes=0", "rust_live_bytes=-1"),
            valid().replace("rust_live_bytes=0", "rust_live_bytes=oops"),
            format!("{} rust_live_bytes=0", valid()),
        ] {
            assert!(MemoryStatistics::parse(&line).is_err(), "accepted {line}");
        }
    }
}
