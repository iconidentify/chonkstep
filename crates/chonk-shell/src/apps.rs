
use std::collections::{hash_map::Entry, HashMap};
use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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

/// The most one `.desktop` file is read for. The files are
/// user-writable and the rescan runs on a worker, so a pathological
/// file costs worker time only — but a menu row is a handful of keys,
/// nothing legitimate is anywhere near this, and a bound keeps that
/// worker's time and memory proportional to a directory's file count
/// rather than to whatever somebody dropped in it.
const MAX_DESKTOP_FILE_BYTES: u64 = 1 << 20;

/// How often the application directories' mtimes are compared, from
/// the shell tick. A few `stat`s a second on paths that exist — the
/// same cadence, and the same argument for polling over inotify, as
/// `omarchy_menu` makes for its two definition files.
const DIRECTORY_POLL_INTERVAL: Duration = Duration::from_secs(1);

// Generations belong to the process, not to one index's lifetime: a
// root menu that opened against one index can outlive any number of
// rescans, and its pick must match none of them. Starts at one so a
// `RootMenuBounds::default()` (generation zero) never resolves an app.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(1))
        .expect("application index generation space exhausted")
}

/// The scanned application list as the shell holds it: an immutable,
/// shared snapshot stamped with a generation. `Desktop` owns the one
/// current copy and the shell reads through it, so the menu, a pick
/// and the session-layout matcher can never disagree about which list
/// is current; the `Arc` makes a swap a pointer exchange rather than a
/// copy of every entry.
///
/// The generation is the stale-pick guard. The Applications submenu
/// is built from one generation and a pick from it carries that
/// generation back ([`crate::desktop::RootMenuAction::LaunchApp`]);
/// [`Self::entry`] refuses an index from any other generation, so a
/// menu opened before a rescan landed cannot launch whatever now sits
/// at that position of a re-sorted list — the same guard the Omarchy
/// submenu keeps across a definition reload.
#[derive(Clone, Debug)]
pub struct AppIndex {
    entries: Arc<[AppEntry]>,
    generation: u64,
}

impl Default for AppIndex {
    /// An empty index with a generation of its own — never zero, so an
    /// empty index and "no index" stay distinguishable.
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl AppIndex {
    /// Stamps `entries` with a fresh generation.
    pub fn new(entries: Vec<AppEntry>) -> Self {
        Self { entries: entries.into(), generation: next_generation() }
    }

    /// The entries, name-sorted as [`collate_scanned`] delivers them.
    pub fn entries(&self) -> &[AppEntry] {
        &self.entries
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The entry at `index` of the index stamped `generation`, or
    /// `None` when this is a different generation (or the index is
    /// out of range) — the stale-menu guard the type doc describes.
    pub fn entry(&self, generation: u64, index: usize) -> Option<&AppEntry> {
        (generation == self.generation).then(|| self.entries.get(index)).flatten()
    }
}

/// What identifies the watched directories: each one's mtime, `None`
/// when it is not there — which is itself a change worth seeing, since
/// a package can create `/usr/local/share/applications` on a machine
/// that never had one.
type DirectorySignature = Vec<(PathBuf, Option<SystemTime>)>;

/// One completed walk of the application directories: the entries it
/// produced and the directory mtimes it observed *before* walking, so
/// that a file landing mid-walk still differs from this baseline and
/// starts the next walk instead of being lost between two.
pub struct Scan {
    entries: Vec<AppEntry>,
    directories: DirectorySignature,
}

impl Scan {
    pub fn entries(&self) -> &[AppEntry] {
        &self.entries
    }

    /// The walk's entries as a new [`AppIndex`] generation.
    pub fn into_index(self) -> AppIndex {
        AppIndex::new(self.entries)
    }
}

/// Scans the XDG application directories and returns every launchable
/// entry, deduplicated by desktop-file id (user entries override
/// system ones), sorted by name. Synchronous file I/O: the startup
/// call runs before the first frame, and every later walk goes
/// through [`Rescanner`]'s worker thread.
pub fn scan_applications() -> Scan {
    scan_directories(&xdg_application_dirs())
}

fn scan_directories(dirs: &[PathBuf]) -> Scan {
    let directories = directory_signature(dirs);
    let entries = collate_scanned(read_desktop_sources(dirs), &program_on_path);
    Scan { entries, directories }
}

/// Every directory a walk of `dirs` reads — each root and its existing
/// first-level subdirectories, the same one level
/// [`read_desktop_sources`] descends — with its current mtime. A
/// directory's mtime moves on every create, delete and rename inside
/// it, which is exactly the set of events that add or remove a
/// `.desktop` file (`pacman`, Omarchy's web-app installer and a user
/// dropping a file in place all create; an override is deleted to
/// bring the system copy back), so these few `stat`s stand in for a
/// recursive watch. An edit that rewrites an existing file in place
/// moves only that file's mtime and is picked up by the next reload.
fn directory_signature(dirs: &[PathBuf]) -> DirectorySignature {
    let mut signature = Vec::new();
    for dir in dirs {
        signature.push((dir.clone(), mtime(dir)));
        let Ok(reader) = fs::read_dir(dir) else { continue };
        let mut subdirs: Vec<PathBuf> = reader.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subdirs.sort();
        for subdir in subdirs {
            let modified = mtime(&subdir);
            signature.push((subdir, modified));
        }
    }
    signature
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// Clears the rescanner's in-flight flag when the worker finishes —
/// or unwinds — so a walk that panics can never wedge every later one
/// behind a flag nobody will clear.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Keeps the application index current for the life of the session
/// without reading a `.desktop` file on the shell thread. Two halves,
/// both driven from [`Self::tick`]: a once-a-second comparison of the
/// directory mtimes the last walk recorded (a handful of `stat`s, the
/// same kind of poll `omarchy_menu` runs on its definition files), and
/// a worker thread that re-walks the directories when one moved or
/// when a reload asked for a fresh look.
///
/// At most one walk is in flight; a request that arrives while one
/// runs is remembered and started when it lands, so two walks can
/// never race to publish with the staler one winning. A finished walk
/// waits in `latest` and becomes a new [`AppIndex`] generation on the
/// next tick, which also adopts the walk's pre-walk directory
/// signature as the poll's new baseline — anything that changed after
/// the walk observed it still reads as a change on the next poll.
pub struct Rescanner {
    roots: Arc<[PathBuf]>,
    directories: DirectorySignature,
    last_poll: Instant,
    pending: bool,
    in_flight: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Scan>>>,
}

impl Rescanner {
    /// Watches the XDG application directories from where `scan` —
    /// the startup walk — left off.
    pub fn new(scan: &Scan, now: Instant) -> Self {
        Self::watching(xdg_application_dirs(), scan, now)
    }

    fn watching(roots: Vec<PathBuf>, scan: &Scan, now: Instant) -> Self {
        Self {
            roots: roots.into(),
            directories: scan.directories.clone(),
            last_poll: now,
            pending: false,
            in_flight: Arc::new(AtomicBool::new(false)),
            latest: Arc::new(Mutex::new(None)),
        }
    }

    /// Asks for a fresh walk whatever the directories say — a config
    /// reload is the user's way of saying "look again", and it is also
    /// what catches the one change the mtime poll cannot see, a file
    /// rewritten in place.
    pub fn request(&mut self) {
        self.pending = true;
    }

    /// The shell-tick hook. Never blocks: takes a landed walk if one
    /// is waiting, compares the directory mtimes at
    /// [`DIRECTORY_POLL_INTERVAL`], and starts a walk on its own thread
    /// if one is due and none is running. Returns the index a landed
    /// walk produced, for the caller to swap in.
    pub fn tick(&mut self, now: Instant) -> Option<AppIndex> {
        let landed = self.take_landed();
        if now.duration_since(self.last_poll) >= DIRECTORY_POLL_INTERVAL {
            self.last_poll = now;
            if self.poll_directories() {
                tracing::info!("an application directory changed; rescanning");
                self.pending = true;
            }
        }
        self.service();
        landed
    }

    /// Whether any watched directory's mtime moved since the last look,
    /// re-baselining every entry either way. Rebaselining on the poll
    /// rather than on the walk that follows means a change during the
    /// walk is seen twice at worst (once here, once against the walk's
    /// own signature) and lost never.
    fn poll_directories(&mut self) -> bool {
        let mut changed = false;
        for (path, previous) in &mut self.directories {
            let current = mtime(path);
            changed |= *previous != current;
            *previous = current;
        }
        changed
    }

    fn take_landed(&mut self) -> Option<AppIndex> {
        // A poisoned lock reads as "nothing landed": the worker that
        // panicked never published, and the index in use stays in use.
        let scan = self.latest.lock().ok()?.take()?;
        self.directories = scan.directories.clone();
        Some(AppIndex::new(scan.entries))
    }

    /// Starts a walk if one is due and none is running. Returns
    /// immediately in every case.
    fn service(&mut self) {
        if !self.pending || self.in_flight.load(Ordering::Acquire) {
            return;
        }
        self.pending = false;
        self.in_flight.store(true, Ordering::Release);
        let flag = InFlight(Arc::clone(&self.in_flight));
        let latest = Arc::clone(&self.latest);
        let roots = Arc::clone(&self.roots);
        let spawned = std::thread::Builder::new().name("chonkstep-app-rescan".to_string()).spawn(move || {
            // Published before the flag clears (the guard drops last),
            // so a tick can never find the flag down and the slot empty
            // with a walk still to come.
            let _flag = flag;
            let started = Instant::now();
            let scan = scan_directories(&roots);
            tracing::debug!(
                count = scan.entries.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "application directories walked"
            );
            if let Ok(mut slot) = latest.lock() {
                *slot = Some(scan);
            }
        });
        if let Err(error) = spawned {
            tracing::warn!(?error, "could not start the application rescan thread; the menu keeps its last index");
        }
    }
}

/// Parses one `.desktop` file's text. `None` for anything that should
/// not appear in a menu (not an application, `NoDisplay`, `Hidden`,
/// unparsable).
///
/// `TryExec` (when present) is probed against the real `$PATH` here;
/// tests exercise that skip through `parse_with_lookup`'s seam
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
/// regardless of readdir order; unreadable, non-UTF-8 or oversized
/// files are skipped rather than aborting the whole scan.
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
                        if let Some(text) = read_desktop_text(&sub_path) {
                            sources.push((rank, format!("{subdir_name}-{stem}"), text));
                        }
                    }
                }
            } else if let Some(stem) = desktop_stem(&path) {
                if let Some(text) = read_desktop_text(&path) {
                    sources.push((rank, stem.to_string(), text));
                }
            }
        }
    }
    sources
}

/// One `.desktop` file's text, or `None` when it is unreadable, not
/// UTF-8, or past [`MAX_DESKTOP_FILE_BYTES`] — read through a `take`
/// rather than sized first, so a file that grows between the two
/// steps still cannot be read past the bound.
fn read_desktop_text(path: &Path) -> Option<String> {
    let mut text = String::new();
    // A FIFO named *.desktop must not block startup or monopolize the
    // only rescan worker. Check the opened file, so replacing a regular
    // file with a FIFO between stat and open cannot bypass the guard.
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    file.take(MAX_DESKTOP_FILE_BYTES + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= MAX_DESKTOP_FILE_BYTES).then_some(text)
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
fn collate_scanned(
    sources: Vec<(usize, String, String)>,
    program_exists: &dyn Fn(&str) -> bool,
) -> Vec<AppEntry> {
    // Index by id so a large installed application set does not search
    // every earlier entry for every source. Replacing only a strictly
    // lower rank preserves the first-seen winner within one directory.
    // Move both strings into the map; duplicate ids need no extra copy.
    let mut chosen: HashMap<String, (usize, String)> = HashMap::new();
    for (rank, id, text) in sources {
        match chosen.entry(id) {
            Entry::Occupied(mut existing) if rank < existing.get().0 => {
                existing.insert((rank, text));
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(slot) => {
                slot.insert((rank, text));
            }
        }
    }
    let mut entries: Vec<AppEntry> = chosen
        .into_iter()
        .filter_map(|(id, (_, text))| parse_with_lookup(&id, &text, program_exists))
        .collect();
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

    /// The previous collation algorithm, retained only as a behavioral
    /// oracle and timing baseline. This deliberately does not share the
    /// indexed deduplication used in production.
    fn collate_linear(sources: Vec<(usize, String, String)>) -> Vec<AppEntry> {
        let mut chosen: Vec<(usize, String, String)> = Vec::new();
        for (rank, id, text) in sources {
            match chosen.iter_mut().find(|(_, chosen_id, _)| *chosen_id == id) {
                Some(existing) if rank < existing.0 => *existing = (rank, id, text),
                Some(_) => {}
                None => chosen.push((rank, id, text)),
            }
        }
        let mut entries: Vec<_> = chosen
            .iter()
            .filter_map(|(_, id, text)| parse_with_lookup(id, text, &|program| program != "absent"))
            .collect();
        entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.id.clone()));
        entries
    }

    fn collation_fixture(count: usize) -> Vec<(usize, String, String)> {
        let mut sources = Vec::with_capacity(count * 4);
        for index in 0..count {
            let id = format!("org.example.app-{index:05}");
            // Equal names exercise the final id tie-break, independent
            // of randomized hash iteration. Every source layer also
            // supplies a different executable, making wrong winners
            // observable even when the labels match.
            let source = |rank: usize, extra: &str| {
                (
                    rank,
                    id.clone(),
                    format!(
                        "[Desktop Entry]\nType=Application\nName={}\nExec=layer-{rank}-{index}\n{extra}\n",
                        ["Alpha", "alpha", "βeta", "zeta"][index % 4]
                    ),
                )
            };
            sources.push(source(3, ""));
            let extra = match index % 7 {
                0 => "Hidden=true",
                1 => "TryExec=absent",
                2 => "NoDisplay=true",
                3 => "Exec=", // an invalid override also hides its system copy
                _ => "",
            };
            sources.push(source(0, extra));
            sources.push(source(0, "Exec=same-rank-later"));
            sources.push(source(2, ""));
        }
        sources
    }

    #[test]
    fn indexed_collation_preserves_overrides_and_sorting_for_large_shuffled_catalogues() {
        let mut sources = collation_fixture(1024);
        // Fixed-seed Fisher-Yates keeps the fixture reproducible while
        // mixing directory ranks and same-rank ties throughout it.
        let mut random = 0x613e_8c5b_u64;
        for index in (1..sources.len()).rev() {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            sources.swap(index, (random as usize) % (index + 1));
        }
        let expected = collate_linear(sources.clone());
        assert!(!expected.is_empty());
        for _ in 0..3 {
            // Each call gets a new randomized map seed. Output order
            // and the selected entry must remain identical every time.
            assert_eq!(
                collate_scanned(sources.clone(), &|program| program != "absent"),
                expected
            );
        }
    }

    /// Pure collation only: excludes fixture construction, directory
    /// I/O and real TryExec probes. This measures the changed algorithm,
    /// not whole-session startup, and imposes no flaky timing threshold.
    #[test]
    #[ignore = "manual collation timing; run with --release --ignored --nocapture"]
    fn benchmark_application_collation() {
        use std::hint::black_box;
        use std::time::Instant;

        for count in [64, 512, 4096] {
            let sources = collation_fixture(count);
            let expected = collate_linear(sources.clone());
            let mut linear = Vec::new();
            let mut indexed = Vec::new();
            for round in 0..8 {
                // Alternate order to avoid consistently giving one
                // algorithm the warmer caches. Clone outside timing.
                for use_index in [round % 2 == 0, round % 2 != 0] {
                    let input = sources.clone();
                    let start = Instant::now();
                    let entries = if use_index {
                        collate_scanned(black_box(input), &|program| program != "absent")
                    } else {
                        collate_linear(black_box(input))
                    };
                    let elapsed = start.elapsed();
                    assert_eq!(black_box(&entries), &expected);
                    if round != 0 {
                        if use_index {
                            indexed.push(elapsed);
                        } else {
                            linear.push(elapsed);
                        }
                    }
                }
            }
            linear.sort_unstable();
            indexed.sort_unstable();
            eprintln!(
                "application collation: {count} ids, {} sources, linear median {:?}, indexed median {:?}",
                sources.len(),
                linear[linear.len() / 2],
                indexed[indexed.len() / 2]
            );
        }
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

    #[test]
    fn a_fifo_cannot_block_the_application_scan_worker() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        let fifo = dir.join("blocked.desktop");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path is a live NUL-terminated string; mkfifo retains
        // no pointer and creates only this test's private fixture.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        fs::write(dir.join("good.desktop"), named_fixture("Good")).unwrap();
        std::os::unix::fs::symlink(&fifo, dir.join("linked.desktop")).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(read_desktop_sources(&[dir])).unwrap());
        let sources = receiver.recv_timeout(Duration::from_secs(2)).expect("special files must not stall the scan");
        assert_eq!(sources.iter().map(|(_, id, _)| id.as_str()).collect::<Vec<_>>(), vec!["good"]);
    }

    #[test]
    fn oversized_desktop_files_are_skipped_by_the_walk() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("applications");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("alpha.desktop"), named_fixture("Alpha")).unwrap();
        // Valid text one byte past the bound: the size alone must
        // exclude it, not a parse failure.
        let mut huge = named_fixture("Huge");
        huge.push('#');
        huge.push_str(&"x".repeat(MAX_DESKTOP_FILE_BYTES as usize + 1 - huge.len()));
        fs::write(dir.join("huge.desktop"), &huge).unwrap();

        let sources = read_desktop_sources(&[dir]);
        let ids: Vec<&str> = sources.iter().map(|(_, id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["alpha"]);
    }

    // -- the index and its rescanner --

    #[test]
    fn an_index_resolves_a_pick_only_from_its_own_generation() {
        let first = AppIndex::new(vec![entry("Alpha", "alpha", None), entry("Beta", "beta", None)]);
        let second = AppIndex::new(vec![entry("Beta", "beta", None), entry("Alpha", "alpha", None)]);
        assert!(second.generation() > first.generation(), "generations only ever advance");
        assert_eq!(first.entry(first.generation(), 1).map(|e| e.name.as_str()), Some("Beta"));
        // The same count, a different order: index 1 from the first
        // generation must resolve to nothing against the second, not
        // to whatever now sits there.
        assert!(second.entry(first.generation(), 1).is_none());
        assert_eq!(second.entry(second.generation(), 1).map(|e| e.name.as_str()), Some("Alpha"));
        assert!(first.entry(first.generation(), 2).is_none(), "out of range dissolves too");
        let empty = AppIndex::default();
        assert!(empty.entries().is_empty());
        assert_ne!(empty.generation(), 0, "even an empty index has a generation of its own");
    }

    fn scratch_applications() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("applications");
        fs::create_dir(&dir).unwrap();
        (root, dir)
    }

    fn install(dir: &Path, id: &str, name: &str) {
        fs::write(dir.join(format!("{id}.desktop")), named_fixture(name)).unwrap();
    }

    /// A different directory mtime for sure, without sleeping past the
    /// filesystem's timestamp granularity: set it explicitly, the way
    /// `omarchy_follow`'s tests do for their files.
    fn bump(dir: &Path, seconds_ahead: u64) {
        fs::File::open(dir).unwrap().set_modified(SystemTime::now() + Duration::from_secs(seconds_ahead)).unwrap();
    }

    fn names(index: &AppIndex) -> Vec<&str> {
        index.entries().iter().map(|e| e.name.as_str()).collect()
    }

    /// Ticks — with the synthetic clock advanced a poll interval each
    /// time, so every tick is also a poll — until a walk lands. The
    /// walk runs on its own thread; this is the only wait in the test.
    fn land(rescanner: &mut Rescanner, clock: &mut Instant) -> AppIndex {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            *clock += DIRECTORY_POLL_INTERVAL;
            if let Some(index) = rescanner.tick(*clock) {
                return index;
            }
            assert!(Instant::now() < deadline, "the rescan never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_file_added_or_removed_after_the_scan_lands_a_new_generation_off_thread() {
        let (_root, dir) = scratch_applications();
        install(&dir, "alpha", "Alpha");
        let scan = scan_directories(std::slice::from_ref(&dir));
        let mut clock = Instant::now();
        let mut rescanner = Rescanner::watching(vec![dir.clone()], &scan, clock);
        let startup = scan.into_index();
        assert_eq!(names(&startup), ["Alpha"]);

        // Nothing changed: a poll finds the baseline and starts nothing.
        clock += DIRECTORY_POLL_INTERVAL;
        assert!(rescanner.tick(clock).is_none());
        assert!(!rescanner.in_flight.load(Ordering::Acquire), "an unchanged directory must not cost a walk");

        // An install during the session: the directory's mtime moves,
        // the poll notices, the walk lands a new generation with the
        // new entry in sorted place.
        install(&dir, "beta", "Beta");
        bump(&dir, 10);
        let with_beta = land(&mut rescanner, &mut clock);
        assert_eq!(names(&with_beta), ["Alpha", "Beta"]);
        assert!(with_beta.generation() > startup.generation());

        // And a removal takes the row away again.
        fs::remove_file(dir.join("beta.desktop")).unwrap();
        bump(&dir, 20);
        let without = land(&mut rescanner, &mut clock);
        assert_eq!(names(&without), ["Alpha"]);
        assert!(without.generation() > with_beta.generation());
        // A menu opened against the previous generation resolves its
        // pick to nothing against this one.
        assert!(without.entry(with_beta.generation(), 1).is_none());
    }

    #[test]
    fn a_requested_walk_runs_without_a_directory_change_and_requests_coalesce() {
        let (_root, dir) = scratch_applications();
        install(&dir, "alpha", "Alpha");
        let scan = scan_directories(std::slice::from_ref(&dir));
        let mut clock = Instant::now();
        let mut rescanner = Rescanner::watching(vec![dir.clone()], &scan, clock);
        let startup = scan.into_index();

        // A reload's "look again": same directories, a fresh walk.
        rescanner.request();
        assert!(rescanner.tick(clock).is_none(), "the walk is asynchronous; nothing lands on the tick that starts it");
        // Requests while that walk runs fold into one more walk, not
        // one per request.
        rescanner.request();
        rescanner.request();
        let first = land(&mut rescanner, &mut clock);
        assert_eq!(names(&first), ["Alpha"]);
        assert!(first.generation() > startup.generation());
        let second = land(&mut rescanner, &mut clock);
        assert!(second.generation() > first.generation());
        // ...and then quiet: a few more polls land nothing and start
        // nothing.
        for _ in 0..3 {
            clock += DIRECTORY_POLL_INTERVAL;
            assert!(rescanner.tick(clock).is_none());
        }
        assert!(!rescanner.pending);
    }

    #[test]
    fn a_subdirectory_created_after_the_scan_joins_the_watch() {
        let (_root, dir) = scratch_applications();
        install(&dir, "alpha", "Alpha");
        let scan = scan_directories(std::slice::from_ref(&dir));
        let mut clock = Instant::now();
        let mut rescanner = Rescanner::watching(vec![dir.clone()], &scan, clock);
        assert_eq!(rescanner.directories.len(), 1, "the root alone, no subdirectories yet");

        // Creating the subdirectory moves the root's mtime; the walk
        // that follows records the new directory in its signature.
        let extras = dir.join("extras");
        fs::create_dir(&extras).unwrap();
        bump(&dir, 10);
        let unchanged = land(&mut rescanner, &mut clock);
        assert_eq!(names(&unchanged), ["Alpha"]);
        assert_eq!(rescanner.directories.len(), 2, "the walk's signature now covers the subdirectory");

        // A file dropped into the subdirectory alone — the root's
        // mtime does not move for that — is still noticed.
        install(&extras, "gamma", "Gamma");
        bump(&extras, 20);
        let with_gamma = land(&mut rescanner, &mut clock);
        assert_eq!(names(&with_gamma), ["Alpha", "Gamma"]);
        assert_eq!(with_gamma.entries()[1].id, "extras-gamma");
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
