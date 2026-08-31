//! The Shelf, as a dockapp: clipboard history in a stack of dock tiles.
//!
//! NeXTSTEP's Shelf was the flagship "hold this for me" surface. This
//! is that idea rebuilt on the dockapp platform: a three-tile stack
//! that records what you copy and gives any entry back with a click —
//! and because it is an out-of-process instrument, a bug in it costs
//! one tile, never the desktop. It is also the multi-tile
//! demonstration: `tile_units = 3` exercises the protocol's stacked
//! geometry (`MAX_TILE_UNITS` is 4) and tile-local input routing (a
//! click's `y` selects the entry).
//!
//! # How a process with no display connection reads the clipboard
//!
//! It doesn't — and that is the platform's own claim ("no clipboard to
//! read" is in `chonk-dock-proto`'s docs). The dockapp process holds no
//! display connection; the shell even launches it with
//! `WAYLAND_DISPLAY` removed. Clipboard access is delegated to child
//! processes from the `wl-clipboard` package (`wl-paste` to sample,
//! `wl-copy` to give back), which are handed a display name the shelf
//! re-derives itself: `$CHONK_SHELF_WAYLAND_DISPLAY` if set, else the
//! first `wayland-*` socket in `$XDG_RUNTIME_DIR`. On a machine running
//! one session that is unambiguous; with nested compositors, set the
//! variable in the registration's `exec` array
//! (`["env", "CHONK_SHELF_WAYLAND_DISPLAY=wayland-2", "chonk-shelf"]`).
//!
//! If `wl-clipboard` is not installed, or no display socket can be
//! found, the shelf degrades to a static tile saying so instead of
//! exiting: a dead face teaches nothing, a labeled one names the fix.
//!
//! # Threads, because the workspace ban is real
//!
//! `wl-paste` is polled with `Command::output` — a blocking call, which
//! the workspace `clippy.toml` bans by default after a built-in widget
//! once froze the whole compositor with one. The rules that make it
//! safe here: the poll runs on a dedicated sampler thread this program
//! owns, and the only thing that freezes if `wl-paste` never returns is
//! the freshness of this one tile. The draw and input callbacks (the
//! SDK's own loop thread) never block on a child: `wl-copy` is spawned,
//! fed and reaped entirely on a throwaway thread.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chonk_ui::dockapp::{self, Handlers, InputKind, LogLevel, Options, Pixmap};
use chonk_ui::model::{FontSpec, TextAlign};
use chonk_ui::{paint, tile as tilekit};

/// Stacked tiles requested, one clipboard entry per tile. Three of the
/// protocol's maximum four, leaving the dock some column to breathe.
const SHELF_UNITS: u8 = 3;

/// How often the sampler asks `wl-paste` what the clipboard holds.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Entries longer than this are stored in full (so a click gives back
/// every byte) but never *drawn* in full — the tile shows a preview.
const PREVIEW_CHARS: usize = 96;

/// What the sampler learned, shared with the draw/input callbacks.
struct ShelfState {
    /// Newest first, at most [`SHELF_UNITS`] entries.
    entries: Mutex<VecDeque<String>>,
    /// Bumped on every change, so `draw` can answer "did anything
    /// change?" with one atomic load instead of a lock and a compare.
    version: AtomicU64,
    /// The degrade path: `wl-paste` missing or no display socket found.
    /// The tile then explains itself instead of showing stale nothing.
    unavailable: AtomicBool,
}

fn main() {
    let state = Arc::new(ShelfState {
        entries: Mutex::new(VecDeque::new()),
        version: AtomicU64::new(0),
        unavailable: AtomicBool::new(false),
    });

    match clipboard_display() {
        Some(display) => spawn_sampler(Arc::clone(&state), display),
        None => {
            state.unavailable.store(true, Ordering::Relaxed);
            state.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Text machinery lives outside the draw closure: FontSystem::new
    // scans the font directories, which is a startup cost, not a
    // per-frame one.
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    // What the last painted frame showed, so an unchanged shelf costs
    // the wire nothing. The pixmap itself is repainted every pass —
    // cheap at three tiles — so a forced send (first frame after a
    // connect or a retheme) is always sent from a fully drawn buffer.
    let mut last_painted: Option<(u64, u32, String)> = None;

    let draw_state = Arc::clone(&state);
    let input_state = Arc::clone(&state);

    let result = dockapp::run_with(
        "chonk-shelf",
        Options { tile_units: SHELF_UNITS, ..Options::default() },
        Handlers {
            draw: move |ctx: &dockapp::Ctx, pixmap: &mut Pixmap| {
                let version = draw_state.version.load(Ordering::Relaxed);
                let signature = (version, ctx.tile_px(), ctx.theme().id.clone());
                let changed = last_painted.as_ref() != Some(&signature);

                render(ctx, pixmap, &draw_state, &mut font_system, &mut swash_cache);
                last_painted = Some(signature);
                changed
            },
            input: move |ctx: &dockapp::Ctx, event: chonk_dock_proto::wire::InputEvent| {
                if event.kind != InputKind::Press {
                    return false;
                }
                let unit = (event.y / ctx.tile_px().max(1) as i32).clamp(0, i32::from(SHELF_UNITS) - 1) as usize;
                let text = input_state.entries.lock().ok().and_then(|entries| entries.get(unit).cloned());
                if let Some(text) = text {
                    match clipboard_display() {
                        Some(display) => {
                            recopy(&text, &display);
                            ctx.log(LogLevel::Info, &format!("shelf: re-copied entry {unit}"));
                        }
                        None => ctx.log(LogLevel::Warn, "shelf: no wayland display for wl-copy"),
                    }
                }
                false
            },
        },
    );

    if let Err(e) = result {
        eprintln!("chonk-shelf: {e}");
        std::process::exit(1);
    }
}

/// Paints the whole stack: one themed tile face per entry, newest on
/// top, each carrying a preview of its text in the theme's own menu
/// font and ink.
fn render(
    ctx: &dockapp::Ctx,
    pixmap: &mut Pixmap,
    state: &ShelfState,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
) {
    let size = ctx.tile_px();
    let theme = ctx.theme();
    let entries = state.entries.lock().map(|e| e.clone()).unwrap_or_default();
    let unavailable = state.unavailable.load(Ordering::Relaxed);

    // The theme's menu item font, resized to the tile: previews are
    // body text, and the menu's is the theme's opinion on body text.
    let base = &theme.menu.item_font;
    let preview_font = FontSpec { size: (size as f32 * 0.14).max(8.0), ..base.clone() };
    let label_font = FontSpec { size: (size as f32 * 0.12).max(7.0), ..base.clone() };
    let ink = tilekit::tile_ink(theme);
    let dim = tilekit::tile_ink_dim(theme);

    for unit in 0..u32::from(SHELF_UNITS) {
        let y = (unit * size) as i32;
        tilekit::draw_tile_base(pixmap, 0, y, size, theme);

        let inset = (size / 8).max(4) as i32;
        let well_w = size - 2 * inset as u32;
        let well_h = size - 2 * inset as u32;
        tilekit::draw_tile_well(pixmap, inset, y + inset, well_w, well_h, theme);

        let pad = inset + 3;
        let text_w = size.saturating_sub(2 * pad as u32);
        let text_h = size.saturating_sub(2 * pad as u32);
        match entries.get(unit as usize) {
            Some(entry) => {
                let preview = preview_of(entry);
                paint::draw_text(
                    pixmap, font_system, swash_cache, &preview, &preview_font, ink,
                    pad, y + pad, text_w, text_h, TextAlign::Left,
                );
            }
            None => {
                let label = if unavailable {
                    if unit == 0 { "install\nwl-clipboard" } else { "" }
                } else if unit == 0 {
                    "shelf\nempty"
                } else {
                    ""
                };
                paint::draw_text(
                    pixmap, font_system, swash_cache, label, &label_font, dim,
                    pad, y + pad, text_w, text_h, TextAlign::Center,
                );
            }
        }
    }
}

/// The first [`PREVIEW_CHARS`] characters, with runs of whitespace
/// collapsed so a copied paragraph reads as a phrase rather than a
/// staircase. The stored entry keeps every byte; only the drawing is
/// abbreviated.
fn preview_of(entry: &str) -> String {
    let mut out = String::with_capacity(PREVIEW_CHARS + 1);
    let mut in_space = false;
    for c in entry.chars() {
        if c.is_whitespace() {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
        if out.chars().count() >= PREVIEW_CHARS {
            out.push('…');
            break;
        }
    }
    out
}

/// The display name to hand `wl-paste`/`wl-copy`: the override
/// variable, or the first `wayland-*` socket in `$XDG_RUNTIME_DIR`.
/// `None` degrades the shelf to its labeled static face.
fn clipboard_display() -> Option<String> {
    if let Ok(display) = std::env::var("CHONK_SHELF_WAYLAND_DISPLAY") {
        if !display.is_empty() {
            return Some(display);
        }
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let mut sockets: Vec<String> = std::fs::read_dir(runtime)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
        .collect();
    sockets.sort();
    sockets.into_iter().next()
}

/// The sampler: one dedicated thread polling `wl-paste` once a second.
///
/// Polling rather than `wl-paste --watch` because the watch mode writes
/// entries back-to-back with no delimiter a reader can trust (clipboard
/// text may contain anything), while one process per poll has an
/// unambiguous start and end. A fork per second is the cost of the
/// dockapp's no-display-connection guarantee, and it is a cost this
/// process pays alone.
fn spawn_sampler(state: Arc<ShelfState>, display: String) {
    std::thread::spawn(move || {
        let mut wl_paste_seen = false;
        loop {
            // This is the shelf's own sampler thread, owned by this
            // loop and waited on by nothing: if wl-paste blocks
            // forever, the shelf's *contents* go stale and the desktop
            // loses nothing — the SDK loop keeps drawing and answering
            // pings on its own thread.
            #[allow(clippy::disallowed_methods)]
            let output = std::process::Command::new("wl-paste")
                .args(["--no-newline", "--type", "text"])
                .env("WAYLAND_DISPLAY", &display)
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    wl_paste_seen = true;
                    if let Ok(text) = String::from_utf8(out.stdout) {
                        if !text.trim().is_empty() {
                            push_entry(&state, text);
                        }
                    }
                }
                Ok(_) => {
                    // Nonzero exit: an empty or non-text clipboard.
                    // Normal, and proof the tool works.
                    wl_paste_seen = true;
                }
                Err(_) if !wl_paste_seen => {
                    // wl-paste is not installed (or not executable).
                    // Degrade once, visibly, and stop forking for it.
                    state.unavailable.store(true, Ordering::Relaxed);
                    state.version.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(_) => {}
            }
            std::thread::sleep(SAMPLE_INTERVAL);
        }
    });
}

/// Records one clipboard sample: consecutive duplicates are one entry,
/// an entry re-copied from the shelf floats back to the top, and the
/// stack holds at most [`SHELF_UNITS`] entries.
fn push_entry(state: &ShelfState, text: String) {
    let Ok(mut entries) = state.entries.lock() else { return };
    if entries.front() == Some(&text) {
        return;
    }
    entries.retain(|existing| existing != &text);
    entries.push_front(text);
    entries.truncate(usize::from(SHELF_UNITS));
    drop(entries);
    state.version.fetch_add(1, Ordering::Relaxed);
}

/// Feeds one entry back to the clipboard, entirely off the SDK's loop
/// thread: `wl-copy` is spawned here, but written to and reaped on a
/// throwaway thread, so a wedged clipboard costs a thread and not the
/// tile.
fn recopy(text: &str, display: &str) {
    let child = std::process::Command::new("wl-copy")
        .env("WAYLAND_DISPLAY", display)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let text = text.to_owned();
    std::thread::spawn(move || {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // Reaping a spawned child is legitimate on a thread dedicated
        // to it (the workspace clippy.toml's own words): nothing waits
        // on this thread, so nothing freezes if wl-copy never exits.
        #[allow(clippy::disallowed_methods)]
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ShelfState {
        ShelfState {
            entries: Mutex::new(VecDeque::new()),
            version: AtomicU64::new(0),
            unavailable: AtomicBool::new(false),
        }
    }

    #[test]
    fn the_shelf_holds_newest_first_and_at_most_its_tile_count() {
        let s = state();
        for text in ["one", "two", "three", "four"] {
            push_entry(&s, text.to_string());
        }
        let entries = s.entries.lock().unwrap();
        assert_eq!(entries.len(), usize::from(SHELF_UNITS));
        assert_eq!(entries[0], "four", "newest on top");
        assert!(!entries.contains(&"one".to_string()), "the oldest fell off");
    }

    #[test]
    fn a_repeated_copy_is_one_entry_and_a_recopy_floats_to_the_top() {
        let s = state();
        push_entry(&s, "a".into());
        push_entry(&s, "a".into());
        assert_eq!(s.entries.lock().unwrap().len(), 1, "consecutive duplicates coalesce");
        push_entry(&s, "b".into());
        push_entry(&s, "a".into());
        let entries = s.entries.lock().unwrap();
        assert_eq!(entries.len(), 2, "re-copying an entry must not duplicate it");
        assert_eq!(entries[0], "a");
    }

    #[test]
    fn every_change_bumps_the_version_and_a_no_op_does_not() {
        let s = state();
        push_entry(&s, "a".into());
        let after_first = s.version.load(Ordering::Relaxed);
        assert_eq!(after_first, 1);
        push_entry(&s, "a".into());
        assert_eq!(s.version.load(Ordering::Relaxed), after_first, "a duplicate is not a change the wire should see");
    }

    #[test]
    fn previews_are_bounded_and_single_line() {
        let long = "  line one\n\n\tline\ttwo  ".to_string() + &"x".repeat(500);
        let preview = preview_of(&long);
        assert!(preview.chars().count() <= PREVIEW_CHARS + 1, "bounded, ellipsis included");
        assert!(!preview.contains('\n'), "whitespace runs collapse to one space");
        assert!(preview.starts_with("line one line two"), "{preview:?}");
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn the_shelf_geometry_fits_the_protocol() {
        // tile_units = 3 must be carriable at every scale a real
        // session uses; this is the multi-tile case the protocol's
        // MAX_TILE_UNITS exists for.
        for scale in [1.0f32, 1.5, 2.0] {
            let tile = (56.0 * scale).round() as u32;
            assert!(chonk_dock_proto::frame_fits(tile, SHELF_UNITS), "{tile}px x {SHELF_UNITS}");
        }
    }
}
