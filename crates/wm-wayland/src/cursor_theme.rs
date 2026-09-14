//! Named cursor shapes, drawn from the user's Xcursor theme.
//!
//! A client that binds `wp_cursor_shape_manager_v1` stops loading a
//! cursor theme of its own. It names a shape (`text`, `pointer`,
//! `ew-resize`, `wait`...) and expects the compositor to draw it; Smithay
//! hands that name over as `CursorImageStatus::Named`. This module turns
//! the name into pixels.
//!
//! Resolving a theme walks `XCURSOR_PATH` and every `Inherits` chain and
//! reads files. That is blocking filesystem work, and the files are
//! untrusted input. Neither belongs on the thread that draws the desktop
//! and reads input, so one worker thread owns the theme. The compositor
//! posts it the set of nominal sizes its outputs need; the worker answers
//! with one decoded image per shape and size over a bounded calloop
//! channel, and the compositor thread only wraps the delivered pixels in
//! render buffers. Until an image arrives, and for good when the theme
//! lacks a shape or the file is missing, oversized or malformed, the
//! pointer is ChonkStep's own arrow, which is what every named shape drew
//! before this module existed.
//!
//! Sizes follow the outputs, not the UI scale alone: each distinct output
//! scale gets `round(base × scale)`, and the theme's nearest image is
//! drawn pixel for pixel. It is never scaled by GL, for the same reason
//! the hand-drawn arrow is rasterised at the UI scale rather than
//! filtered up (see `renderer::push_cursor_elements`).
//!
//! Only the first frame of an animated shape (`wait`, `progress`) is
//! delivered. Animating one is a follow-up.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use calloop::channel::{self, Event, SyncSender};
use calloop::LoopHandle;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{MemoryBuffer, MemoryRenderBuffer};
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::utils::{Logical, Point as SPoint, Transform};
use wm_theme_api::{Point, Rect, Size};

use crate::state::{Compositor, CursorSprite};

/// Largest cursor file read. Adwaita's animated `wait` and `progress`
/// are 4.7 MiB each (60 frames at six sizes), so the cap leaves room for
/// a richer theme while keeping one read bounded.
const MAX_CURSOR_FILE_BYTES: u64 = 16 << 20;
/// Largest image side accepted. Real themes top out near 256.
const MAX_FRAME_SIDE: u32 = 512;
/// Images in one file, across every size.
const MAX_IMAGES: usize = 1024;
/// Frames of one animation at one nominal size. Adwaita uses 60.
const MAX_FRAMES: usize = 64;
/// Table-of-contents entries in one file, images and comments together.
const MAX_TOC_ENTRIES: u32 = 4096;
/// Distinct nominal sizes loaded at once. A desk with more distinct
/// output scales than this shows the arrow on the largest of them.
const MAX_NOMINALS: usize = 8;
/// Deliveries waiting for the compositor. When it is full the worker
/// waits, so a slow compositor bounds the memory in flight.
const DELIVERY_QUEUE: usize = 16;

const XCURSOR_MAGIC: &[u8; 4] = b"Xcur";
const XCURSOR_IMAGE_TYPE: u32 = 0xfffd_0002;
const XCURSOR_IMAGE_HEADER_BYTES: u64 = 36;

/// Every shape a client can name except `default`, which always draws
/// ChonkStep's own arrow: version 1's names plus version 2's `dnd-ask`
/// and `all-resize`.
const SHAPES: [CursorIcon; 35] = [
    CursorIcon::ContextMenu,
    CursorIcon::Help,
    CursorIcon::Pointer,
    CursorIcon::Progress,
    CursorIcon::Wait,
    CursorIcon::Cell,
    CursorIcon::Crosshair,
    CursorIcon::Text,
    CursorIcon::VerticalText,
    CursorIcon::Alias,
    CursorIcon::Copy,
    CursorIcon::Move,
    CursorIcon::NoDrop,
    CursorIcon::NotAllowed,
    CursorIcon::Grab,
    CursorIcon::Grabbing,
    CursorIcon::EResize,
    CursorIcon::NResize,
    CursorIcon::NeResize,
    CursorIcon::NwResize,
    CursorIcon::SResize,
    CursorIcon::SeResize,
    CursorIcon::SwResize,
    CursorIcon::WResize,
    CursorIcon::EwResize,
    CursorIcon::NsResize,
    CursorIcon::NeswResize,
    CursorIcon::NwseResize,
    CursorIcon::ColResize,
    CursorIcon::RowResize,
    CursorIcon::AllScroll,
    CursorIcon::ZoomIn,
    CursorIcon::ZoomOut,
    CursorIcon::DndAsk,
    CursorIcon::AllResize,
];

/// One image, decoded on the worker. No GL and no Wayland objects.
pub(crate) struct DecodedImage {
    /// Premultiplied ARGB32 in the file's little-endian byte order, which
    /// is already the byte layout of `Fourcc::Argb8888`.
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    hotspot: (i32, i32),
}

/// One shape at one nominal size, as the worker delivers it.
pub(crate) struct DecodedCursor {
    icon: CursorIcon,
    nominal: u32,
    image: DecodedImage,
}

/// Why a theme file was not used.
#[derive(Debug)]
enum Refusal {
    Unreadable(std::io::Error),
    TooLarge(u64),
    Malformed(&'static str),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreadable(error) => write!(f, "unreadable: {error}"),
            Refusal::TooLarge(bytes) => write!(f, "{bytes} bytes is over the {MAX_CURSOR_FILE_BYTES}-byte cap"),
            Refusal::Malformed(reason) => write!(f, "malformed: {reason}"),
        }
    }
}

/// The logical cursor size at `scale`, in physical pixels. A broken
/// scale is treated as 1 rather than trusted into a zero or huge size.
pub(crate) fn nominal_size(base: u32, scale: f64) -> u32 {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    ((f64::from(base) * scale).round() as u32).clamp(1, MAX_FRAME_SIDE)
}

/// The distinct nominal sizes a desk with these output scales needs,
/// smallest first and at most [`MAX_NOMINALS`].
fn nominal_set(base: u32, scales: &[f64]) -> Vec<u32> {
    let mut nominals: Vec<u32> = scales.iter().map(|scale| nominal_size(base, *scale)).collect();
    nominals.sort_unstable();
    nominals.dedup();
    nominals.truncate(MAX_NOMINALS);
    nominals
}

/// Looks a shape up by each of its names in order, the spec name first
/// and then the legacy aliases older themes ship instead (`xterm` for
/// `text`, `hand2` for `pointer`), and returns the first that `lookup`
/// resolves.
fn find_shape<T>(icon: CursorIcon, mut lookup: impl FnMut(&str) -> Option<T>) -> Option<T> {
    std::iter::once(icon.name()).chain(icon.alt_names().iter().copied()).find_map(&mut lookup)
}

/// The available image size closest to `nominal`. A tie goes to the
/// larger image: a pointer slightly too big stays legible, and one too
/// small is the failure a user who pinned a large size is avoiding.
fn nearest_size(sizes: impl IntoIterator<Item = u32>, nominal: u32) -> Option<u32> {
    sizes.into_iter().min_by_key(|size| (size.abs_diff(nominal), std::cmp::Reverse(*size)))
}

/// Reads a whole file, refusing one larger than `cap`. The read itself
/// is capped too, so a file that grows after its size was checked cannot
/// push past the limit.
fn read_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, Refusal> {
    let file = std::fs::File::open(path).map_err(Refusal::Unreadable)?;
    let metadata = file.metadata().map_err(Refusal::Unreadable)?;
    if !metadata.is_file() {
        return Err(Refusal::Malformed("not a regular file"));
    }
    if metadata.len() > cap {
        return Err(Refusal::TooLarge(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(cap + 1).read_to_end(&mut bytes).map_err(Refusal::Unreadable)?;
    if bytes.len() as u64 > cap {
        return Err(Refusal::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Walks the file's table of contents and every image header before the
/// `xcursor` parser allocates anything. The parser trusts the table: a
/// file of a few megabytes whose entries all point at one large image
/// would decode into gigabytes. Here each image must fit inside the
/// file, the counts and sides are capped, and the images together may
/// not claim more pixel bytes than the file holds.
fn validate(bytes: &[u8]) -> Result<(), Refusal> {
    let len = bytes.len() as u64;
    let u32_at = |at: u64| -> Option<u32> {
        let start = usize::try_from(at).ok()?;
        let word = bytes.get(start..start.checked_add(4)?)?;
        Some(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    };
    if bytes.get(..4) != Some(XCURSOR_MAGIC.as_slice()) {
        return Err(Refusal::Malformed("not an Xcursor file"));
    }
    let (Some(header), Some(entries)) = (u32_at(4), u32_at(12)) else {
        return Err(Refusal::Malformed("truncated header"));
    };
    if entries > MAX_TOC_ENTRIES {
        return Err(Refusal::Malformed("too many table entries"));
    }
    let mut images = 0_usize;
    let mut pixel_bytes = 0_u64;
    let mut frames: HashMap<u32, usize> = HashMap::new();
    for index in 0..u64::from(entries) {
        let entry = u64::from(header) + index * 12;
        let (Some(kind), Some(size), Some(position)) = (u32_at(entry), u32_at(entry + 4), u32_at(entry + 8)) else {
            return Err(Refusal::Malformed("truncated table"));
        };
        if kind != XCURSOR_IMAGE_TYPE {
            continue;
        }
        images += 1;
        if images > MAX_IMAGES {
            return Err(Refusal::Malformed("too many images"));
        }
        let at = u64::from(position);
        let (Some(width), Some(height)) = (u32_at(at + 16), u32_at(at + 20)) else {
            return Err(Refusal::Malformed("truncated image"));
        };
        if !(1..=MAX_FRAME_SIDE).contains(&width) || !(1..=MAX_FRAME_SIDE).contains(&height) {
            return Err(Refusal::Malformed("image side out of range"));
        }
        let count = frames.entry(size).or_default();
        *count += 1;
        if *count > MAX_FRAMES {
            return Err(Refusal::Malformed("too many frames at one size"));
        }
        let pixels = 4 * u64::from(width) * u64::from(height);
        if at + XCURSOR_IMAGE_HEADER_BYTES + pixels > len {
            return Err(Refusal::Malformed("truncated image"));
        }
        pixel_bytes += pixels;
    }
    if images == 0 {
        return Err(Refusal::Malformed("no images"));
    }
    if pixel_bytes > len {
        return Err(Refusal::Malformed("images claim more pixels than the file holds"));
    }
    Ok(())
}

/// Decodes a cursor file into the first frame nearest each nominal size.
fn decode(bytes: &[u8], nominals: &[u32]) -> Result<Vec<(u32, DecodedImage)>, Refusal> {
    validate(bytes)?;
    let images = xcursor::parser::parse_xcursor(bytes).ok_or(Refusal::Malformed("unparsable image"))?;
    let mut decoded = Vec::with_capacity(nominals.len());
    for &nominal in nominals {
        let Some(best) = nearest_size(images.iter().map(|image| image.size), nominal) else {
            continue;
        };
        let Some(image) = images.iter().find(|image| image.size == best) else {
            continue;
        };
        decoded.push((
            nominal,
            DecodedImage {
                pixels: image.pixels_rgba.clone(),
                width: image.width,
                height: image.height,
                hotspot: (image.xhot as i32, image.yhot as i32),
            },
        ));
    }
    Ok(decoded)
}

/// The worker's side of the theme: resolved lazily on the worker, never
/// on the compositor thread.
struct ThemeSource {
    name: String,
    theme: Option<xcursor::CursorTheme>,
    /// Shapes already warned about, so a broken file is logged once and
    /// not again on every scale change.
    reported: HashSet<CursorIcon>,
}

impl ThemeSource {
    fn load(&mut self, icon: CursorIcon, nominals: &[u32]) -> Vec<DecodedCursor> {
        let theme = self.theme.get_or_insert_with(|| xcursor::CursorTheme::load(&self.name));
        let Some(path) = find_shape(icon, |name| theme.load_icon(name)) else {
            return Vec::new();
        };
        match read_bounded(&path, MAX_CURSOR_FILE_BYTES).and_then(|bytes| decode(&bytes, nominals)) {
            Ok(images) => images.into_iter().map(|(nominal, image)| DecodedCursor { icon, nominal, image }).collect(),
            Err(refusal) => {
                if self.reported.insert(icon) {
                    tracing::warn!(
                        theme = %self.name,
                        shape = icon.name(),
                        path = %path.display(),
                        %refusal,
                        "refusing a cursor theme file; the arrow stands in for this shape"
                    );
                }
                Vec::new()
            }
        }
    }
}

/// The newest request, handed from the compositor to the worker. Latest
/// wins: a request the worker has not started is simply replaced, so a
/// burst of scale changes cannot queue up work.
#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    wake: Condvar,
}

#[derive(Default)]
struct MailboxState {
    wanted: Option<Vec<u32>>,
    closed: bool,
}

impl Mailbox {
    fn lock(&self) -> MutexGuard<'_, MailboxState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn post(&self, nominals: Vec<u32>) {
        self.lock().wanted = Some(nominals);
        self.wake.notify_one();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_one();
    }

    /// Blocks the worker until there is a request, or `None` once the
    /// compositor has gone.
    fn take(&self) -> Option<Vec<u32>> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(nominals) = state.wanted.take() {
                return Some(nominals);
            }
            state = self.wake.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Whether the request in progress has been replaced or abandoned.
    fn superseded(&self) -> bool {
        let state = self.lock();
        state.closed || state.wanted.is_some()
    }
}

/// The worker loop, generic over where images come from and where they
/// go so it can be exercised without a theme or an event loop.
fn serve(
    mailbox: &Mailbox,
    theme: &str,
    mut load: impl FnMut(CursorIcon, &[u32]) -> Vec<DecodedCursor>,
    mut deliver: impl FnMut(DecodedCursor) -> bool,
) {
    while let Some(nominals) = mailbox.take() {
        let mut delivered = 0_usize;
        let mut complete = true;
        for icon in SHAPES {
            if mailbox.superseded() {
                complete = false;
                break;
            }
            for cursor in load(icon, &nominals) {
                if !deliver(cursor) {
                    return;
                }
                delivered += 1;
            }
        }
        if complete && delivered == 0 {
            tracing::warn!(
                theme,
                "the cursor theme provides no named cursor shapes; clients naming a shape get the arrow"
            );
        } else if complete {
            tracing::info!(theme, ?nominals, delivered, "named cursor shapes loaded");
        }
    }
}

/// Named-shape sprites on the compositor thread, keyed by shape and
/// nominal size. Lives inside `CursorSet`, so every scene builder reaches
/// it without a new parameter. Bounded by the closed shape set times
/// [`MAX_NOMINALS`].
pub(crate) struct ThemedCursors {
    base: u32,
    sprites: HashMap<(CursorIcon, u32), CursorSprite>,
}

impl Default for ThemedCursors {
    fn default() -> Self {
        Self { base: 24, sprites: HashMap::new() }
    }
}

impl ThemedCursors {
    /// The theme's image for `icon` on an output at `scale`, if the
    /// worker has delivered one.
    pub(crate) fn get(&self, icon: CursorIcon, scale: f64) -> Option<&CursorSprite> {
        self.sprites.get(&(icon, nominal_size(self.base, scale)))
    }

    fn insert(&mut self, cursor: DecodedCursor) {
        let DecodedImage { pixels, width, height, hotspot } = cursor.image;
        let memory = MemoryBuffer::from_slice(&pixels, Fourcc::Argb8888, (width as i32, height as i32));
        let sprite = CursorSprite {
            // Buffer scale 1: the image is already at the output's pixel
            // size, exactly like the hand-drawn set (`state::import_cursor`).
            buffer: MemoryRenderBuffer::from_memory(memory, 1, Transform::Normal, None),
            hotspot,
            size: Size::new(width, height),
        };
        self.sprites.insert((cursor.icon, cursor.nominal), sprite);
    }
}

/// The compositor's handle on the worker.
pub(crate) struct ThemeLoader {
    mailbox: Arc<Mailbox>,
    /// The output scales the current request was computed from. Compared
    /// by value every dispatch pass, so an unchanged desk allocates
    /// nothing.
    scales: Vec<f64>,
    /// The nominal sizes last requested. A delivery for any other size
    /// answers a superseded request and is dropped.
    nominals: Vec<u32>,
}

impl Drop for ThemeLoader {
    fn drop(&mut self) {
        self.mailbox.close();
    }
}

/// Starts the worker and registers its delivery channel. Returns `None`
/// when either fails; named shapes then keep drawing the arrow.
///
/// Called after the startup environment is final (`ensure_xcursor_size`,
/// `apply_session_env`), so the worker reads the same `XCURSOR_PATH` and
/// `XDG_DATA_DIRS` the session's clients inherit.
pub(crate) fn init(loop_handle: &LoopHandle<'static, Compositor>) -> Option<ThemeLoader> {
    let theme = std::env::var("XCURSOR_THEME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let (sender, receiver) = channel::sync_channel::<DecodedCursor>(DELIVERY_QUEUE);
    if let Err(error) = loop_handle.insert_source(receiver, |event, _, comp| match event {
        Event::Msg(cursor) => accept(comp, cursor),
        Event::Closed => tracing::debug!("cursor theme worker stopped"),
    }) {
        tracing::warn!(?error, "could not register cursor theme deliveries; named shapes draw the arrow");
        return None;
    }
    let mailbox = Arc::new(Mailbox::default());
    let worker_mailbox = Arc::clone(&mailbox);
    let spawned = std::thread::Builder::new().name("chonkstep-cursor-theme".into()).spawn(move || {
        let mut source = ThemeSource { name: theme.clone(), theme: None, reported: HashSet::new() };
        let deliver = |cursor| send(&sender, cursor);
        serve(&worker_mailbox, &theme, |icon, nominals| source.load(icon, nominals), deliver);
    });
    if let Err(error) = spawned {
        tracing::warn!(?error, "could not start the cursor theme worker; named shapes draw the arrow");
        return None;
    }
    Some(ThemeLoader { mailbox, scales: Vec::new(), nominals: Vec::new() })
}

fn send(sender: &SyncSender<DecodedCursor>, cursor: DecodedCursor) -> bool {
    sender.send(cursor).is_ok()
}

/// Posts a new request when the set of output scales has changed, and
/// drops sprites for sizes no output needs any more. Runs every dispatch
/// pass; the comparison is the whole cost of an unchanged desk.
pub(crate) fn reconcile(comp: &mut Compositor) {
    let Some(loader) = comp.cursor_theme.as_mut() else {
        return;
    };
    let scales = &comp.wm.backend().monitor_scales;
    if scales.is_empty() || loader.scales == *scales {
        return;
    }
    loader.scales.clone_from(scales);
    let themed = &mut comp.cursors.themed;
    let nominals = nominal_set(themed.base, scales);
    themed.sprites.retain(|(_, nominal), _| nominals.contains(nominal));
    if nominals != loader.nominals {
        loader.nominals.clone_from(&nominals);
        loader.mailbox.post(nominals);
    }
}

/// Sets the logical base size the nominal sizes are computed from. Called
/// once, before the first [`reconcile`].
pub(crate) fn set_base_size(comp: &mut Compositor, base: u32) {
    comp.cursors.themed.base = base;
}

/// Takes one delivery on the compositor thread.
fn accept(comp: &mut Compositor, cursor: DecodedCursor) {
    let Some(loader) = comp.cursor_theme.as_ref() else {
        return;
    };
    if !loader.nominals.contains(&cursor.nominal) {
        return;
    }
    let (icon, nominal) = (cursor.icon, cursor.nominal);
    comp.cursors.themed.insert(cursor);
    if shows(comp, icon, nominal) {
        comp.wm.backend_mut().mark_damaged();
    }
}

/// Whether the pointer or a tablet tool is showing `icon` at `nominal`
/// right now, so that a delivery repaints only when it changes the screen.
fn shows(comp: &Compositor, icon: CursorIcon, nominal: u32) -> bool {
    let backend = comp.wm.backend();
    let base = comp.cursors.themed.base;
    let visible_at = |status: &CursorImageStatus, position: SPoint<f64, Logical>| {
        if !matches!(status, CursorImageStatus::Named(named) if *named == icon) {
            return false;
        }
        let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
        nominal_size(base, backend.scale_at(Rect::new(at, Size::new(1, 1)))) == nominal
            && matches!(crate::input::pointer_subject(backend, position), crate::input::PointerSubject::Client)
    };
    visible_at(&comp.cursor_status, comp.pointer_location)
        || comp.tablet_cursors.iter().any(|tool| visible_at(&tool.status, tool.position))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One Xcursor image chunk: header, then `side × side` pixels of one
    /// premultiplied ARGB value.
    fn image_chunk(nominal: u32, side: u32, hotspot: (u32, u32), argb: u32) -> Vec<u8> {
        let mut chunk = Vec::new();
        for word in [36, XCURSOR_IMAGE_TYPE, nominal, 1, side, side, hotspot.0, hotspot.1, 0] {
            chunk.extend_from_slice(&u32::to_le_bytes(word));
        }
        for _ in 0..side * side {
            chunk.extend_from_slice(&argb.to_le_bytes());
        }
        chunk
    }

    /// A complete file. Each entry is `(nominal, side, table position
    /// override)`; `None` places the image after the previous one.
    fn cursor_file(images: &[(u32, u32, u32)]) -> Vec<u8> {
        let header = 16_u32;
        let table = 12 * images.len() as u32;
        let mut chunks = Vec::new();
        let mut entries = Vec::new();
        let mut position = header + table;
        for &(nominal, side, argb) in images {
            let chunk = image_chunk(nominal, side, (side / 4, side / 2), argb);
            entries.push((nominal, position));
            position += chunk.len() as u32;
            chunks.extend(chunk);
        }
        let mut file = Vec::new();
        file.extend_from_slice(XCURSOR_MAGIC);
        for word in [header, 0x0001_0000, images.len() as u32] {
            file.extend_from_slice(&word.to_le_bytes());
        }
        for (nominal, position) in entries {
            for word in [XCURSOR_IMAGE_TYPE, nominal, position] {
                file.extend_from_slice(&word.to_le_bytes());
            }
        }
        file.extend(chunks);
        file
    }

    #[test]
    fn a_shape_falls_back_through_its_legacy_names_in_order() {
        let mut asked = Vec::new();
        // An old theme that ships only `hand1`, the second alias.
        let found = find_shape(CursorIcon::Pointer, |name| {
            asked.push(name.to_string());
            (name == "hand1").then(|| format!("cursors/{name}"))
        });
        assert_eq!(found.as_deref(), Some("cursors/hand1"));
        assert_eq!(asked, ["pointer", "hand2", "hand1"], "the spec name first, then the aliases in order");

        let mut asked = Vec::new();
        let found = find_shape(CursorIcon::Text, |name| {
            asked.push(name.to_string());
            None::<()>
        });
        assert_eq!(found, None);
        assert_eq!(asked, ["text", "xterm", "ibeam"], "a missing shape tries every alias, then gives up");

        assert_eq!(find_shape(CursorIcon::Text, |name| (name == "text").then_some(name.len())), Some(4));
        assert!(!SHAPES.contains(&CursorIcon::Default), "`default` is always ChonkStep's own arrow");
    }

    #[test]
    fn the_nearest_size_wins_and_a_tie_goes_to_the_larger_image() {
        let sizes = [24, 30, 36, 48, 72, 96];
        assert_eq!(nearest_size(sizes, 24), Some(24));
        assert_eq!(nearest_size(sizes, 36), Some(36));
        assert_eq!(nearest_size(sizes, 40), Some(36));
        assert_eq!(nearest_size(sizes, 42), Some(48), "equidistant from 36 and 48");
        assert_eq!(nearest_size(sizes, 8), Some(24));
        assert_eq!(nearest_size(sizes, 500), Some(96));
        assert_eq!(nearest_size([], 24), None);

        assert_eq!(nominal_size(24, 1.5), 36);
        assert_eq!(nominal_size(24, 1.25), 30);
        assert_eq!(nominal_size(48, 2.0), 96);
        assert_eq!(nominal_size(24, f64::NAN), 24, "a broken scale is treated as 1");
        assert_eq!(nominal_size(256, 4.0), MAX_FRAME_SIDE);
        assert_eq!(nominal_set(24, &[2.0, 1.0, 1.0, 1.5]), [24, 36, 48], "one size per distinct scale");

        let file = cursor_file(&[(24, 24, 0xff00_00ff), (48, 48, 0xffff_0000)]);
        let decoded = decode(&file, &[24, 36, 96]).expect("a well-formed file decodes");
        let summary: Vec<_> = decoded.iter().map(|(nominal, image)| (*nominal, image.width, image.hotspot)).collect();
        assert_eq!(summary, [(24, 24, (6, 12)), (36, 48, (12, 24)), (96, 48, (12, 24))]);
        assert_eq!(&decoded[1].1.pixels[..4], &0xffff_0000_u32.to_le_bytes(), "the file's bytes are Argb8888's layout");
    }

    #[test]
    fn oversized_and_malformed_files_are_refused() {
        let good = cursor_file(&[(24, 24, 0xff00_00ff)]);
        assert!(decode(&good, &[24]).is_ok());

        assert!(matches!(decode(b"Xcu", &[24]), Err(Refusal::Malformed(_))), "shorter than the magic");
        assert!(matches!(decode(&good[..good.len() - 1], &[24]), Err(Refusal::Malformed(_))), "truncated pixels");
        let mut wrong_magic = good.clone();
        wrong_magic[0] = b'P';
        assert!(matches!(decode(&wrong_magic, &[24]), Err(Refusal::Malformed(_))));

        let side_too_large = cursor_file(&[(600, MAX_FRAME_SIDE + 1, 0)]);
        assert!(matches!(decode(&side_too_large, &[24]), Err(Refusal::Malformed("image side out of range"))));

        let mut zero_side = good.clone();
        zero_side[16 + 12 + 16..16 + 12 + 20].copy_from_slice(&0_u32.to_le_bytes());
        assert!(matches!(decode(&zero_side, &[24]), Err(Refusal::Malformed(_))));

        let mut huge_table = good.clone();
        huge_table[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode(&huge_table, &[24]), Err(Refusal::Malformed("too many table entries"))));

        // Two table entries aimed at the one image the file holds: a small
        // file that would decode into a copy per entry.
        let mut aliased = cursor_file(&[(24, 64, 0); 2]);
        let first_position = aliased[16 + 8..16 + 12].to_vec();
        aliased[16 + 12 + 8..16 + 24].copy_from_slice(&first_position);
        aliased.truncate(16 + 24 + XCURSOR_IMAGE_HEADER_BYTES as usize + 64 * 64 * 4);
        assert!(xcursor::parser::parse_xcursor(&aliased).is_some_and(|images| images.len() == 2), "the parser alone would decode it twice");
        assert!(matches!(decode(&aliased, &[24]), Err(Refusal::Malformed("images claim more pixels than the file holds"))));

        let too_many_frames = cursor_file(&[(24, 1, 0); MAX_FRAMES + 1]);
        assert!(matches!(decode(&too_many_frames, &[24]), Err(Refusal::Malformed("too many frames at one size"))));

        let directory = std::env::temp_dir().join(format!("chonkstep-cursor-theme-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("text");
        std::fs::write(&path, &good).unwrap();
        assert!(read_bounded(&path, good.len() as u64).is_ok());
        assert!(matches!(read_bounded(&path, good.len() as u64 - 1), Err(Refusal::TooLarge(_))));
        assert!(matches!(read_bounded(&directory, MAX_CURSOR_FILE_BYTES), Err(Refusal::Malformed(_))));
        assert!(matches!(read_bounded(&directory.join("missing"), MAX_CURSOR_FILE_BYTES), Err(Refusal::Unreadable(_))));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// The request returns at once while the loader is still busy, and
    /// nothing is delivered until it finishes: the compositor keeps
    /// drawing the arrow in the meantime, instead of waiting on a disk.
    #[test]
    fn a_slow_theme_answers_on_the_worker_without_holding_the_requester() {
        let mailbox = Arc::new(Mailbox::default());
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let (delivered, deliveries) = std::sync::mpsc::channel::<CursorIcon>();
        let worker_mailbox = Arc::clone(&mailbox);
        let worker = std::thread::spawn(move || {
            let load = |icon: CursorIcon, nominals: &[u32]| {
                if icon != CursorIcon::Text {
                    return Vec::new();
                }
                gate.recv().unwrap();
                let image = DecodedImage { pixels: vec![0; 4], width: 1, height: 1, hotspot: (0, 0) };
                vec![DecodedCursor { icon, nominal: nominals[0], image }]
            };
            serve(&worker_mailbox, "slow", load, |cursor| delivered.send(cursor.icon).is_ok());
        });

        let started = std::time::Instant::now();
        mailbox.post(vec![24]);
        assert!(started.elapsed() < std::time::Duration::from_millis(100), "posting never waits on the loader");
        assert!(deliveries.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "nothing before the load ends");
        release.send(()).unwrap();
        assert_eq!(deliveries.recv_timeout(std::time::Duration::from_secs(5)), Ok(CursorIcon::Text));
        mailbox.close();
        worker.join().unwrap();
    }
}
