//! Freedesktop application discovery: scanning and parsing the
//! `.desktop` entries every installed app ships, into the flat
//! [`AppEntry`] list the Applications menu and the launcher dock both
//! consume.
//!
//! The module follows the freedesktop Desktop Entry specification, but
//! is deliberately split into a pure core and a thin filesystem shell:
//! parsing ([`parse_desktop_entry`]) and cross-directory collation
//! ([`collate_scanned`]) operate on plain strings and are exhaustively
//! unit-tested, while [`scan_applications`] only walks the XDG
//! directories and feeds what it finds into that core. The `TryExec`
//! existence probe likewise goes through a function-pointer seam so the
//! skip logic tests without a real `$PATH`.
//!
//! Deliberate simplifications, each documented where it lives:
//! - Only the plain `Name` key is honored; localized `Name[xx]`
//!   variants are ignored. Chonkstep's own chrome is untranslated, so a
//!   localized menu label would be the odd one out anyway.
//! - Entries carrying `OnlyShowIn` are skipped outright. That key
//!   restricts an entry to specific registered desktop environments
//!   (GNOME's control center panels, KDE service menus, ...), and
//!   chonkstep is not a registered desktop, so no value of the list can
//!   ever name us. `NotShowIn` is ignored for the same reason: it can
//!   never match us either, so those entries stay visible.
//! - The desktop-file id drops the `.desktop` suffix (matching the
//!   `org.mozilla.firefox` shape documented on [`AppEntry::id`]): every
//!   file carries the same extension, so it adds nothing to the
//!   identity that launcher pins persist.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// One launchable application, distilled from its `.desktop` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    /// The desktop-file id (`org.mozilla.firefox`), the stable identity
    /// launcher pins persist.
    pub id: String,
    pub name: String,
    /// Parsed argv with the spec's `%f`/`%u`-style field codes removed.
    pub exec: Vec<String>,
    /// `Terminal=true`: launch inside the themed terminal.
    pub terminal: bool,
    pub category: AppCategory,
    /// `StartupWMClass`, when declared — the strongest signal for
    /// matching a running window back to its application.
    pub startup_wm_class: Option<String>,
}

/// The single menu bucket an app resolves to, from the freedesktop
/// main-category registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppCategory {
    Accessories,
    Development,
    Games,
    Graphics,
    Internet,
    Multimedia,
    Office,
    Science,
    Settings,
    System,
    Other,
}

impl AppCategory {
    pub fn label(self) -> &'static str {
        match self {
            AppCategory::Accessories => "Accessories",
            AppCategory::Development => "Development",
            AppCategory::Games => "Games",
            AppCategory::Graphics => "Graphics",
            AppCategory::Internet => "Internet",
            AppCategory::Multimedia => "Multimedia",
            AppCategory::Office => "Office",
            AppCategory::Science => "Science",
            AppCategory::Settings => "Settings",
            AppCategory::System => "System",
            AppCategory::Other => "Other",
        }
    }
}

/// Scans the XDG application directories and returns every launchable
/// entry, deduplicated by desktop-file id (user entries override
/// system ones), sorted by name.
pub fn scan_applications() -> Vec<AppEntry> {
    collate_scanned(read_desktop_sources(&xdg_application_dirs()), &program_on_path)
}

/// Parses one `.desktop` file's text. `None` for anything that should
/// not appear in a menu (not an application, `NoDisplay`, `Hidden`,
/// unparsable).
///
/// `TryExec` (when present) is probed against the real `$PATH` here;
/// tests exercise that skip through [`parse_with_lookup`]'s seam
/// instead so they never depend on what happens to be installed.
#[allow(dead_code)] // The scan pipeline goes through `parse_with_lookup`'s testable seam; this is the module's standalone entry point, kept as documented API surface.
pub fn parse_desktop_entry(id: &str, text: &str) -> Option<AppEntry> {
    parse_with_lookup(id, text, &program_on_path)
}

/// Finds the entry a running window most plausibly belongs to, from
/// its `WM_CLASS` class string — `StartupWMClass` first, then name,
/// then executable basename, all case-insensitive.
///
/// The three signals run as sequential passes over the whole slice, so
/// a later entry's explicit `StartupWMClass` still beats an earlier
/// entry's mere name coincidence; within one pass the first entry wins.
/// Comparison is ASCII-case-insensitive: `WM_CLASS` is ASCII in
/// practice, and non-ASCII names simply compare exactly.
pub fn match_window_class(entries: &[AppEntry], wm_class: &str) -> Option<usize> {
    entries
        .iter()
        .position(|e| e.startup_wm_class.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(wm_class)))
        .or_else(|| entries.iter().position(|e| e.name.eq_ignore_ascii_case(wm_class)))
        .or_else(|| {
            entries
                .iter()
                .position(|e| e.exec.first().is_some_and(|argv0| basename(argv0).eq_ignore_ascii_case(wm_class)))
        })
}

/// The `applications` directories to scan, highest priority first:
/// `$XDG_DATA_HOME` (default `~/.local/share`), then each entry of
/// `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`) in order.
/// Empty environment values count as unset, per the XDG basedir spec.
fn xdg_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let data_home = env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("HOME").ok().filter(|v| !v.is_empty()).map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(data_home) = data_home {
        dirs.push(data_home.join("applications"));
    }
    let data_dirs =
        env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    dirs
}

/// Reads every `.desktop` file under the given directories into
/// `(dir_rank, id, text)` tuples for [`collate_scanned`] — this is the
/// entire filesystem-touching half of the scan, kept logic-free on
/// purpose. One level of subdirectories is included, with the subdir
/// name joined into the id with `-` exactly as the spec derives
/// desktop-file ids (`applications/extras/foo.desktop` -> `extras-foo`);
/// deeper nesting is rare in the wild and the spec's id scheme cannot
/// distinguish it from a literal `-` anyway, so one level is where we
/// stop. Directory listings are sorted so ids collide deterministically
/// regardless of readdir order; unreadable or non-UTF-8 files are
/// skipped rather than aborting the whole scan.
fn read_desktop_sources(dirs: &[PathBuf]) -> Vec<(usize, String, String)> {
    let mut sources = Vec::new();
    for (rank, dir) in dirs.iter().enumerate() {
        let Ok(reader) = fs::read_dir(dir) else { continue };
        let mut paths: Vec<PathBuf> = reader.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                let Some(subdir_name) = path.file_name().and_then(|n| n.to_str()) else { continue };
                let Ok(sub_reader) = fs::read_dir(&path) else { continue };
                let mut sub_paths: Vec<PathBuf> = sub_reader.flatten().map(|e| e.path()).collect();
                sub_paths.sort();
                for sub_path in sub_paths {
                    if sub_path.is_dir() {
                        continue; // one level only
                    }
                    if let Some(stem) = desktop_stem(&sub_path) {
                        if let Ok(text) = fs::read_to_string(&sub_path) {
                            sources.push((rank, format!("{subdir_name}-{stem}"), text));
                        }
                    }
                }
            } else if let Some(stem) = desktop_stem(&path) {
                if let Ok(text) = fs::read_to_string(&path) {
                    sources.push((rank, stem.to_string(), text));
                }
            }
        }
    }
    sources
}

/// The desktop-file id a directory entry contributes, or `None` for
/// anything that is not a `.desktop` file.
fn desktop_stem(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.strip_suffix(".desktop")
}

/// The pure half of [`scan_applications`]: collates raw
/// `(dir_rank, id, text)` tuples into the final sorted entry list.
///
/// Deduplication happens by id BEFORE parsing, lowest rank winning
/// (ties go to the first tuple seen, matching filesystem iteration
/// order within one directory). Parsing after deduplication is what
/// gives the spec's override-to-delete behavior for free: a user file
/// with `Hidden=true` shadows the system file of the same id first,
/// and only then gets dropped by the parser — removing the app from
/// the menu entirely instead of letting the system copy resurface.
fn collate_scanned(sources: Vec<(usize, String, String)>, program_exists: &dyn Fn(&str) -> bool) -> Vec<AppEntry> {
    // Linear scan instead of a map: a real system carries a few hundred
    // entries at most, and this keeps first-seen tie-breaking obvious.
    let mut chosen: Vec<(usize, String, String)> = Vec::new();
    for (rank, id, text) in sources {
        match chosen.iter_mut().find(|(_, chosen_id, _)| *chosen_id == id) {
            Some(existing) if rank < existing.0 => *existing = (rank, id, text),
            Some(_) => {}
            None => chosen.push((rank, id, text)),
        }
    }
    let mut entries: Vec<AppEntry> =
        chosen.iter().filter_map(|(_, id, text)| parse_with_lookup(id, text, program_exists)).collect();
    // Case-insensitive by name so "gimp" files next to "GIMP"; id as the
    // tie-break keeps equal names deterministic across runs.
    entries.sort_by_cached_key(|e| (e.name.to_lowercase(), e.id.clone()));
    entries
}

/// [`parse_desktop_entry`] with the `TryExec` existence probe injected,
/// so every skip path is testable without a filesystem or a `$PATH`.
fn parse_with_lookup(id: &str, text: &str, program_exists: &dyn Fn(&str) -> bool) -> Option<AppEntry> {
    let keys = desktop_entry_group(text);
    let get = |key: &str| keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

    // Type is required by the spec; anything but Application (Link,
    // Directory) has no argv to launch.
    if get("Type") != Some("Application") {
        return None;
    }
    // NoDisplay means "installed but not for menus" (e.g. a handler
    // registered only for MIME associations); Hidden means the user
    // "deleted" the entry without write access to the system copy.
    if get("NoDisplay") == Some("true") || get("Hidden") == Some("true") {
        return None;
    }
    // OnlyShowIn restricts the entry to specific registered desktop
    // environments. Chonkstep is not one, so no OnlyShowIn list can
    // ever include us — even an empty list means "show nowhere".
    if get("OnlyShowIn").is_some() {
        return None;
    }
    let name = get("Name").filter(|n| !n.is_empty())?;
    // TryExec exists precisely for "the .desktop file outlived its
    // binary" (leftover packaging, shared /usr over NFS): if the named
    // program is not installed, the entry must not be offered.
    if let Some(try_exec) = get("TryExec").filter(|t| !t.is_empty()) {
        if !program_exists(try_exec) {
            return None;
        }
    }
    let exec_value = get("Exec").filter(|e| !e.is_empty())?;
    let words = split_exec_words(&unescape_string_value(exec_value))?;
    let exec: Vec<String> = words.iter().filter_map(|word| strip_field_codes(word)).collect();
    if exec.is_empty() {
        // Nothing left to launch (e.g. `Exec=%f`).
        return None;
    }

    Some(AppEntry {
        id: id.to_string(),
        name: name.to_string(),
        exec,
        terminal: get("Terminal") == Some("true"),
        category: get("Categories").map(map_category).unwrap_or(AppCategory::Other),
        startup_wm_class: get("StartupWMClass").filter(|c| !c.is_empty()).map(str::to_string),
    })
}

/// Extracts the `[Desktop Entry]` group's key/value pairs, and only
/// that group's: keys before the header don't belong to any group, and
/// parsing stops outright at the next `[group]` header, so keys in
/// `[Desktop Action ...]` groups (which legitimately carry their own
/// `Name`/`Exec`/`NoDisplay`) can never bleed into the main entry —
/// not even through a bogus repeated `[Desktop Entry]` header later in
/// the file. Comment and blank lines are skipped, whitespace around
/// `=` is trimmed (the spec ignores space around the delimiter, and
/// trailing whitespace in values is invariably accidental), and for a
/// duplicated key the first occurrence wins (the spec calls duplicates
/// an error without picking a winner; first-wins means a lookup is a
/// simple forward find). Localized keys like `Name[de]` naturally
/// remain distinct from `Name` here and are simply never looked up.
fn desktop_entry_group(text: &str) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut inside = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if inside {
                break;
            }
            inside = line == "[Desktop Entry]";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if !keys.iter().any(|(k, _)| k == key) {
                keys.push((key.to_string(), value.trim().to_string()));
            }
        }
    }
    keys
}

/// The first of the spec's two escape layers on an `Exec` value: the
/// generic string-value escapes `\s` `\n` `\t` `\r` `\\`, which the
/// spec explicitly says are applied BEFORE the quoting rule. This
/// ordering is what makes the well-known "four backslashes for one"
/// example work: file text `\\\\` becomes `\\` here, which the quoted
/// word splitter then collapses to a single literal `\`. A backslash
/// before any other character (or at end of value) is kept verbatim
/// rather than erroring — lenience costs nothing and real files do
/// contain sloppy escapes.
fn unescape_string_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The second escape layer: splits an (already string-unescaped) `Exec`
/// value into words per the spec's quoting rule. Arguments separate on
/// unquoted whitespace; a double-quoted section may appear anywhere in
/// a word, and inside it a backslash escapes the next character (the
/// spec only requires escaping `"` `` ` `` `$` `\`, but accepting any
/// escaped character is strictly more lenient and never changes the
/// meaning of a conforming value). `None` on an unterminated quote or a
/// dangling backslash — a malformed Exec is grounds to drop the whole
/// entry rather than guess at an argv.
fn split_exec_words(value: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    // `Some` from the first character of a word — the distinction from
    // an empty String is what lets an explicit `""` survive as an
    // empty argument instead of vanishing.
    let mut current: Option<String> = None;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
            }
            '"' => {
                let word = current.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        None => return None, // unterminated quote
                        Some('"') => break,
                        Some('\\') => word.push(chars.next()?),
                        Some(inner) => word.push(inner),
                    }
                }
            }
            other => current.get_or_insert_with(String::new).push(other),
        }
    }
    if let Some(word) = current.take() {
        words.push(word);
    }
    Some(words)
}

/// The field codes the spec defines for `Exec` lines. `%f`/`%F` and
/// `%u`/`%U` are file/URL placeholders (we launch from a menu, so there
/// is never a file to substitute), `%i`/`%c`/`%k` expand icon/name/path
/// metadata, and `%d` `%D` `%n` `%N` `%v` `%m` are deprecated no-ops.
const FIELD_CODES: &str = "fFuUdDnNickvm";

/// Removes field codes from one already-split word: `%%` collapses to a
/// literal `%`, every code in [`FIELD_CODES`] disappears even mid-word
/// (`--file=%f` -> `--file=`), and `%` before anything else — an
/// unknown code, or a trailing lone `%` — stays verbatim, since eating
/// unknown text risks corrupting an argv we don't understand. `None`
/// drops the word entirely: a word that consisted only of field codes
/// (the common trailing ` %U`) would otherwise leave a spurious empty
/// argument, while a word that was empty to begin with (explicit `""`)
/// is preserved.
fn strip_field_codes(word: &str) -> Option<String> {
    let mut out = String::with_capacity(word.len());
    let mut contained_code = false;
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(code) if FIELD_CODES.contains(code) => contained_code = true,
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    if contained_code && out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Maps a raw `Categories=` value to the single menu bucket, walking
/// the semicolon-separated list in order and taking the FIRST name
/// that is a main category in the freedesktop menu-spec registry —
/// additional categories (`Qt`, `KDE`, `2DGraphics`, ...) are refining
/// noise for our one-level menu, so they are simply passed over rather
/// than misfiled.
fn map_category(categories: &str) -> AppCategory {
    for category in categories.split(';') {
        let mapped = match category.trim() {
            "AudioVideo" | "Audio" | "Video" => Some(AppCategory::Multimedia),
            "Development" => Some(AppCategory::Development),
            "Education" | "Science" => Some(AppCategory::Science),
            "Game" => Some(AppCategory::Games),
            "Graphics" => Some(AppCategory::Graphics),
            "Network" => Some(AppCategory::Internet),
            "Office" => Some(AppCategory::Office),
            "Settings" => Some(AppCategory::Settings),
            "System" => Some(AppCategory::System),
            "Utility" => Some(AppCategory::Accessories),
            _ => None,
        };
        if let Some(bucket) = mapped {
            return bucket;
        }
    }
    AppCategory::Other
}

/// The real `TryExec` probe: an absolute (or any slash-containing) path
/// is checked directly, a bare name is searched along `$PATH` — the
/// same resolution `exec` itself would do. "Exists" means a regular
/// file with any execute bit set, matching the spec's "present and
/// executable" wording.
fn program_on_path(program: &str) -> bool {
    fn is_executable_file(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    if program.contains('/') {
        return is_executable_file(Path::new(program));
    }
    let Ok(path_var) = env::var("PATH") else { return false };
    path_var.split(':').filter(|dir| !dir.is_empty()).any(|dir| is_executable_file(&Path::new(dir).join(program)))
}

/// The final path component, for matching `WM_CLASS` against an `argv[0]`
/// that may be an absolute path.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every-string helper: a minimal valid entry with `extra` lines
    /// appended inside the `[Desktop Entry]` group.
    fn app_fixture(extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName=Fixture\nExec=fixture\n{extra}\n")
    }

    /// Parses a fixture whose only interesting line is its Exec, and
    /// returns the resulting argv.
    fn exec_of(exec_line: &str) -> Vec<String> {
        parse_desktop_entry("t", &format!("[Desktop Entry]\nType=Application\nName=T\n{exec_line}\n"))
            .expect("exec fixture should parse")
            .exec
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn entry(name: &str, exec0: &str, startup_wm_class: Option<&str>) -> AppEntry {
        AppEntry {
            id: name.to_lowercase(),
            name: name.to_string(),
            exec: vec![exec0.to_string()],
            terminal: false,
            category: AppCategory::Other,
            startup_wm_class: startup_wm_class.map(str::to_string),
        }
    }

    #[test]
    fn a_normal_entry_parses_completely() {
        let text = "[Desktop Entry]\n\
                    Version=1.0\n\
                    Type=Application\n\
                    Name=Image Viewer\n\
                    Comment=Look at pictures\n\
                    Exec=imgview --slideshow %F\n\
                    Terminal=false\n\
                    Categories=Graphics;Viewer;\n\
                    StartupWMClass=ImgViewMain\n";
        let entry = parse_desktop_entry("org.example.imgview", text).expect("should parse");
        assert_eq!(entry.id, "org.example.imgview");
        assert_eq!(entry.name, "Image Viewer");
        assert_eq!(entry.exec, argv(&["imgview", "--slideshow"]));
        assert!(!entry.terminal);
        assert_eq!(entry.category, AppCategory::Graphics);
        assert_eq!(entry.startup_wm_class.as_deref(), Some("ImgViewMain"));
    }

    #[test]
    fn terminal_true_and_absent_startup_wm_class_parse() {
        let entry = parse_desktop_entry("t", &app_fixture("Terminal=true")).expect("should parse");
        assert!(entry.terminal);
        assert_eq!(entry.startup_wm_class, None);
    }

    #[test]
    fn comments_blank_lines_and_spaces_around_equals_are_tolerated() {
        let text = "[Desktop Entry]\n\
                    # a comment, with an = sign in it\n\
                    \n\
                    Type = Application\n\
                    Name =  Spacey \n\
                    Exec= spacey\n";
        let entry = parse_desktop_entry("t", text).expect("should parse");
        assert_eq!(entry.name, "Spacey");
        assert_eq!(entry.exec, argv(&["spacey"]));
    }

    #[test]
    fn duplicate_keys_take_the_first_occurrence() {
        let text = "[Desktop Entry]\nType=Application\nName=First\nName=Second\nExec=x\n";
        assert_eq!(parse_desktop_entry("t", text).expect("should parse").name, "First");
    }

    // -- skip conditions, one by one --

    #[test]
    fn missing_type_is_skipped() {
        assert_eq!(parse_desktop_entry("t", "[Desktop Entry]\nName=X\nExec=x\n"), None);
    }

    #[test]
    fn non_application_type_is_skipped() {
        let text = "[Desktop Entry]\nType=Link\nName=X\nExec=x\nURL=https://example.org\n";
        assert_eq!(parse_desktop_entry("t", text), None);
    }

    #[test]
    fn missing_or_empty_name_is_skipped() {
        assert_eq!(parse_desktop_entry("t", "[Desktop Entry]\nType=Application\nExec=x\n"), None);
        assert_eq!(parse_desktop_entry("t", "[Desktop Entry]\nType=Application\nName=\nExec=x\n"), None);
    }

    #[test]
    fn missing_or_empty_exec_is_skipped() {
        assert_eq!(parse_desktop_entry("t", "[Desktop Entry]\nType=Application\nName=X\n"), None);
        assert_eq!(parse_desktop_entry("t", "[Desktop Entry]\nType=Application\nName=X\nExec=\n"), None);
    }

    #[test]
    fn nodisplay_true_is_skipped_but_false_is_not() {
        assert_eq!(parse_desktop_entry("t", &app_fixture("NoDisplay=true")), None);
        assert!(parse_desktop_entry("t", &app_fixture("NoDisplay=false")).is_some());
    }

    #[test]
    fn hidden_true_is_skipped() {
        assert_eq!(parse_desktop_entry("t", &app_fixture("Hidden=true")), None);
    }

    #[test]
    fn any_onlyshowin_value_is_skipped() {
        // We are not a registered desktop environment, so no OnlyShowIn
        // list can name us — presence of the key alone is disqualifying.
        assert_eq!(parse_desktop_entry("t", &app_fixture("OnlyShowIn=GNOME;")), None);
        assert_eq!(parse_desktop_entry("t", &app_fixture("OnlyShowIn=")), None);
    }

    #[test]
    fn notshowin_is_ignored_and_the_entry_stays() {
        // The inverse restriction can never match us either, so it must
        // not hide anything.
        assert!(parse_desktop_entry("t", &app_fixture("NotShowIn=KDE;")).is_some());
    }

    #[test]
    fn tryexec_not_found_skips_and_found_keeps_via_the_lookup_seam() {
        let lookup = |program: &str| program == "present";
        let found = app_fixture("TryExec=present");
        let missing = app_fixture("TryExec=absent");
        assert!(parse_with_lookup("t", &found, &lookup).is_some());
        assert_eq!(parse_with_lookup("t", &missing, &lookup), None);
        // An empty TryExec value counts as absent, not as "look up ''".
        assert!(parse_with_lookup("t", &app_fixture("TryExec="), &|_| false).is_some());
    }

    #[test]
    fn unterminated_exec_quote_is_skipped() {
        let text = "[Desktop Entry]\nType=Application\nName=X\nExec=app \"unterminated\n";
        assert_eq!(parse_desktop_entry("t", text), None);
    }

    #[test]
    fn exec_that_is_only_field_codes_is_skipped() {
        let text = "[Desktop Entry]\nType=Application\nName=X\nExec=%f\n";
        assert_eq!(parse_desktop_entry("t", text), None);
    }

    // -- group scoping --

    #[test]
    fn keys_after_a_second_group_header_are_ignored() {
        // The Desktop Action group's own Name/Exec/NoDisplay must not
        // bleed into the main entry, nor may a repeated [Desktop Entry]
        // header reopen it.
        let text = "[Desktop Entry]\n\
                    Type=Application\n\
                    Name=Real\n\
                    Exec=real\n\
                    [Desktop Action new-window]\n\
                    Name=Action Name\n\
                    Exec=other --flag\n\
                    NoDisplay=true\n\
                    [Desktop Entry]\n\
                    Name=Impostor\n";
        let entry = parse_desktop_entry("t", text).expect("should parse");
        assert_eq!(entry.name, "Real");
        assert_eq!(entry.exec, argv(&["real"]));
    }

    #[test]
    fn keys_before_the_desktop_entry_header_do_not_count() {
        let text = "Type=Application\n[Desktop Entry]\nName=X\nExec=x\n";
        assert_eq!(parse_desktop_entry("t", text), None);
    }

    // -- localized names --

    #[test]
    fn localized_name_keys_are_ignored_in_favor_of_the_plain_name() {
        let text = "[Desktop Entry]\nType=Application\nName[de]=Rechner\nName=Calculator\nExec=calc\n";
        assert_eq!(parse_desktop_entry("t", text).expect("should parse").name, "Calculator");
    }

    #[test]
    fn an_entry_with_only_localized_names_is_skipped() {
        let text = "[Desktop Entry]\nType=Application\nName[de]=Rechner\nExec=calc\n";
        assert_eq!(parse_desktop_entry("t", text), None);
    }

    // -- Exec quoting and escapes --

    #[test]
    fn quoted_arguments_keep_their_spaces() {
        assert_eq!(
            exec_of("Exec=\"/opt/Cool App/bin/cool\" --new-window"),
            argv(&["/opt/Cool App/bin/cool", "--new-window"])
        );
    }

    #[test]
    fn backslash_escapes_inside_quotes_follow_both_spec_layers() {
        // File text `\\` is the string-escape layer's backslash; the
        // surviving `\"` / `\$` are then the quoting layer's escapes.
        let exec = exec_of(r##"Exec=echo "say \\"hi\\"" "a\\$b" plain"##);
        assert_eq!(exec, argv(&["echo", "say \"hi\"", "a$b", "plain"]));
    }

    #[test]
    fn four_file_backslashes_inside_quotes_become_one_literal_backslash() {
        assert_eq!(exec_of(r##"Exec=app "back\\\\slash""##), argv(&["app", "back\\slash"]));
    }

    #[test]
    fn string_escape_sequences_apply_to_the_exec_value() {
        // \s is the string layer's space; being unquoted it then acts
        // as a separator in the quoting layer — the spec applies the
        // escape rule before the quoting rule, in exactly that order.
        assert_eq!(exec_of(r"Exec=a\sb"), argv(&["a", "b"]));
        // Quoted, the same escape survives as part of the argument.
        assert_eq!(exec_of(r#"Exec=app "one\stwo""#), argv(&["app", "one two"]));
    }

    #[test]
    fn runs_of_whitespace_separate_arguments_once() {
        assert_eq!(exec_of("Exec=app  \t one   two"), argv(&["app", "one", "two"]));
    }

    // -- field-code stripping --

    #[test]
    fn every_spec_field_code_is_stripped() {
        assert_eq!(exec_of("Exec=app %f %F %u %U %d %D %n %N %i %c %k %v %m end"), argv(&["app", "end"]));
    }

    #[test]
    fn double_percent_becomes_a_literal_percent() {
        assert_eq!(exec_of("Exec=app --pct=100%%"), argv(&["app", "--pct=100%"]));
    }

    #[test]
    fn mid_word_field_codes_are_stripped_in_place() {
        assert_eq!(exec_of("Exec=app --file=%f --url=%uX"), argv(&["app", "--file=", "--url=X"]));
    }

    #[test]
    fn unknown_percent_sequences_are_kept_verbatim() {
        assert_eq!(exec_of("Exec=app %z 100%x 50%"), argv(&["app", "%z", "100%x", "50%"]));
    }

    // -- category mapping --

    #[test]
    fn the_full_main_category_registry_maps_to_its_buckets() {
        let table = [
            ("AudioVideo;", AppCategory::Multimedia),
            ("Audio;", AppCategory::Multimedia),
            ("Video;", AppCategory::Multimedia),
            ("Development;", AppCategory::Development),
            ("Education;", AppCategory::Science),
            ("Science;", AppCategory::Science),
            ("Game;", AppCategory::Games),
            ("Graphics;", AppCategory::Graphics),
            ("Network;", AppCategory::Internet),
            ("Office;", AppCategory::Office),
            ("Settings;", AppCategory::Settings),
            ("System;", AppCategory::System),
            ("Utility;", AppCategory::Accessories),
            ("Qt;KDE;", AppCategory::Other),
            ("", AppCategory::Other),
        ];
        for (categories, want) in table {
            let entry = parse_desktop_entry("t", &app_fixture(&format!("Categories={categories}")))
                .expect("category fixture should parse");
            assert_eq!(entry.category, want, "Categories={categories}");
        }
    }

    #[test]
    fn the_first_main_category_wins_over_later_ones_and_non_main_noise() {
        let entry = parse_desktop_entry("t", &app_fixture("Categories=Qt;Audio;Development;"))
            .expect("should parse");
        assert_eq!(entry.category, AppCategory::Multimedia);
    }

    #[test]
    fn a_missing_categories_key_maps_to_other() {
        assert_eq!(parse_desktop_entry("t", &app_fixture("")).expect("should parse").category, AppCategory::Other);
    }

    // -- collation: dedup, override order, sorting --

    fn named_fixture(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=prog\n")
    }

    #[test]
    fn lower_dir_rank_wins_for_a_duplicate_id_regardless_of_input_order() {
        // The system (rank 1) copy arrives first; the user (rank 0)
        // copy must still replace it.
        let sources = vec![
            (1, "app".to_string(), named_fixture("System Copy")),
            (0, "app".to_string(), named_fixture("User Copy")),
        ];
        let entries = collate_scanned(sources, &|_| true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "User Copy");
    }

    #[test]
    fn equal_ranks_keep_the_first_seen_copy() {
        let sources = vec![
            (0, "app".to_string(), named_fixture("First")),
            (0, "app".to_string(), named_fixture("Second")),
        ];
        let entries = collate_scanned(sources, &|_| true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "First");
    }

    #[test]
    fn a_user_hidden_entry_erases_the_system_copy_entirely() {
        // Hidden=true in an overriding file is the spec's "user deleted
        // this app": the system copy must not resurface.
        let sources = vec![
            (0, "app".to_string(), app_fixture("Hidden=true")),
            (1, "app".to_string(), named_fixture("System Copy")),
            (1, "other".to_string(), named_fixture("Other")),
        ];
        let entries = collate_scanned(sources, &|_| true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Other");
    }

    #[test]
    fn entries_sort_by_name_case_insensitively() {
        // Case-sensitively "Banana" (B, 0x42) would sort before
        // "apple" (a, 0x61); the menu wants dictionary order.
        let sources = vec![
            (0, "b".to_string(), named_fixture("Banana")),
            (0, "a".to_string(), named_fixture("apple")),
            (0, "c".to_string(), named_fixture("Cherry")),
        ];
        let names: Vec<String> = collate_scanned(sources, &|_| true).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["apple".to_string(), "Banana".to_string(), "Cherry".to_string()]);
    }

    #[test]
    fn collation_passes_the_tryexec_seam_through_to_parsing() {
        let sources = vec![(0, "app".to_string(), app_fixture("TryExec=absent"))];
        assert!(collate_scanned(sources, &|_| false).is_empty());
    }

    // -- filesystem walk (the one thin non-pure piece) --

    #[test]
    fn the_walk_reads_one_subdirectory_level_and_joins_ids_with_dashes() {
        let root = env::temp_dir().join(format!("chonkstep-apps-walk-{}", std::process::id()));
        let dir = root.join("applications");
        fs::create_dir_all(dir.join("extras").join("deeper")).expect("create fixture tree");
        fs::write(dir.join("alpha.desktop"), "alpha text").expect("write fixture");
        fs::write(dir.join("notes.txt"), "not a desktop file").expect("write fixture");
        fs::write(dir.join("extras").join("beta.desktop"), "beta text").expect("write fixture");
        // Two levels down: must NOT be picked up.
        fs::write(dir.join("extras").join("deeper").join("gamma.desktop"), "gamma text").expect("write fixture");

        let sources = read_desktop_sources(&[dir]);
        let ids: Vec<&str> = sources.iter().map(|(_, id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "extras-beta"]);
        assert!(sources.iter().all(|(rank, _, _)| *rank == 0));
        assert_eq!(sources[0].2, "alpha text");

        fs::remove_dir_all(&root).expect("clean up fixture tree");
    }

    // -- match_window_class --

    #[test]
    fn startup_wm_class_matches_first_and_case_insensitively() {
        let entries = [entry("Alpha", "alpha-bin", None), entry("Beta", "/usr/bin/beta", Some("BetaWindow"))];
        assert_eq!(match_window_class(&entries, "betawindow"), Some(1));
    }

    #[test]
    fn a_later_startup_wm_class_beats_an_earlier_name_coincidence() {
        // "firefox" is entry 0's name but entry 1's declared class; the
        // explicit declaration is the stronger signal even though it
        // comes later in entry order.
        let entries = [entry("firefox", "other-bin", None), entry("Mozilla Firefox", "firefox-bin", Some("firefox"))];
        assert_eq!(match_window_class(&entries, "Firefox"), Some(1));
    }

    #[test]
    fn name_matches_when_no_startup_wm_class_does() {
        let entries = [entry("Alpha", "alpha-bin", None), entry("Beta", "beta-bin", Some("Unrelated"))];
        assert_eq!(match_window_class(&entries, "BETA"), Some(1));
    }

    #[test]
    fn exec_basename_is_the_last_resort_and_strips_directories() {
        let entries = [entry("Web Browser", "/opt/firefox/firefox", None)];
        assert_eq!(match_window_class(&entries, "Firefox"), Some(0));
    }

    #[test]
    fn within_one_tier_the_first_entry_in_order_wins() {
        let entries = [entry("Dup", "dup-one", None), entry("Dup", "dup-two", None)];
        assert_eq!(match_window_class(&entries, "dup"), Some(0));
    }

    #[test]
    fn no_signal_matching_returns_none() {
        let entries = [entry("Alpha", "alpha-bin", Some("AlphaWin"))];
        assert_eq!(match_window_class(&entries, "unrelated"), None);
        assert_eq!(match_window_class(&[], "anything"), None);
    }
}
