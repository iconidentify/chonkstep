//! The two session binaries' reportable build identity.
//!
//! A package version identifies a release family, not the exact source
//! that produced one executable, and the GNU ELF build ID is what joins
//! a coredump to its separate debug file. Keeping both answers here
//! makes the X11 and Wayland entry points print the same contract.

use std::path::Path;

use object::Object;

/// `git describe --tags --always --dirty` from build time.
///
/// A source-archive packager supplies the description through
/// `CHONKSTEP_GIT_DESCRIBE`, because an archive has no `.git` directory.
/// A direct non-git build says `v<version>+unknown-source` rather than
/// pretending the package version identifies unknown source exactly.
pub const SOURCE_ID: &str = env!("CHONKSTEP_SOURCE_ID");

/// The GNU ELF build ID of the executable running this code, as lower
/// case hexadecimal.
pub fn current_elf_build_id() -> Result<String, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running executable: {error}"))?;
    elf_build_id(executable)
}

/// The GNU ELF build ID recorded in `path`, as lower-case hexadecimal.
///
/// This reads the linker's actual `NT_GNU_BUILD_ID` note rather than
/// embedding a second value that could drift from it. It is public so
/// binary integration tests can compare `--version` with the file that
/// was executed.
pub fn elf_build_id(path: impl AsRef<Path>) -> Result<String, String> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|error| format!("{} is not a readable object: {error}", path.display()))?;
    let id = file
        .build_id()
        .map_err(|error| format!("cannot read {} build ID: {error}", path.display()))?
        .ok_or_else(|| format!("{} has no GNU ELF build ID", path.display()))?;
    Ok(hex(id))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_identity_is_one_nonempty_line() {
        assert!(!SOURCE_ID.is_empty());
        assert!(!SOURCE_ID.chars().any(char::is_control));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_test_executable_has_a_hexadecimal_gnu_build_id() {
        let id = current_elf_build_id().unwrap();
        assert!(!id.is_empty());
        assert!(id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}
