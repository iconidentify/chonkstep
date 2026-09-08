//! All filesystem and child-process work stays away from the compositor loop.
// Every blocking method in this module runs on its single dedicated I/O worker;
// no renderer, input callback or shell tick waits for these children.
#![allow(clippy::disallowed_methods)]
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wm_theme_api::DecorationBuffer;

pub(super) enum Job {
    Screenshot(DecorationBuffer),
    Record {
        output: String,
        geometry: String,
        filter: String,
    },
    Stop,
    Error(String),
    Shutdown,
}

pub(super) enum Update {
    RecordingEnded,
    ScreenshotDone,
}

struct Recording {
    child: Child,
    partial: PathBuf,
    destination: PathBuf,
    log: PathBuf,
}

#[derive(Clone, Copy)]
enum ReviewKind {
    Screenshot,
    Recording,
}

impl ReviewKind {
    fn program(self) -> &'static str {
        match self {
            // Omarchy defaults PNG to imv. Respect a user's replacement too.
            Self::Screenshot => "xdg-open",
            Self::Recording => "omacut",
        }
    }

    fn failure_title(self) -> &'static str {
        match self {
            Self::Screenshot => "Screenshot saved; image viewer unavailable",
            Self::Recording => "Recording saved; Omacut unavailable",
        }
    }
}

struct Review {
    child: Child,
    kind: ReviewKind,
    path: PathBuf,
    started: Instant,
}

#[derive(Default)]
struct Background {
    reviews: Vec<Review>,
    notifications: Vec<(Child, Instant)>,
    previews: Vec<RecordingPreview>,
}

const MAX_REVIEWS: usize = 16;
const MAX_NOTIFICATIONS: usize = 8;
const MAX_PREVIEWS: usize = 2;
const MAX_CLIPBOARD_JOBS: usize = 2;
const CLIPBOARD_PROBE: Duration = Duration::from_millis(100);

struct ClipboardCandidate {
    child: Child,
    path: PathBuf,
    deadline: Instant,
}

struct ClipboardResult {
    path: PathBuf,
    copied: bool,
}

/// Publication/review can finish immediately, while clipboard launch order
/// stays serialized. Service::screenshots remains charged until each result,
/// bounding the candidate and waiting paths together at two captures.
#[derive(Default)]
struct Clipboard {
    provider: Option<Child>,
    candidate: Option<ClipboardCandidate>,
    waiting: VecDeque<PathBuf>,
}

impl Clipboard {
    fn enqueue(&mut self, path: PathBuf) -> Result<(), PathBuf> {
        if self.waiting.len() + usize::from(self.candidate.is_some()) >= MAX_CLIPBOARD_JOBS {
            return Err(path);
        }
        self.waiting.push_back(path);
        Ok(())
    }

    fn deadline(&self) -> Option<Instant> {
        self.candidate.as_ref().map(|candidate| candidate.deadline)
    }

    fn poll(
        &mut self,
        now: Instant,
        mut spawn: impl FnMut(&Path) -> std::io::Result<Child>,
    ) -> Vec<ClipboardResult> {
        let mut completed = Vec::new();
        if self
            .provider
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
        {
            if let Some(mut child) = self.provider.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Some(mut candidate) = self.candidate.take() {
            match candidate.child.try_wait() {
                Ok(None) if now < candidate.deadline => self.candidate = Some(candidate),
                Ok(None) => {
                    // Preserve the previous provider until its replacement
                    // survives the same liveness probe used before. This is
                    // process liveness, not an extra Wayland ownership claim.
                    if let Some(mut old) = self.provider.take() {
                        let _ = old.kill();
                        let _ = old.wait();
                    }
                    self.provider = Some(candidate.child);
                    completed.push(ClipboardResult {
                        path: candidate.path,
                        copied: true,
                    });
                }
                Ok(Some(status)) => {
                    // A clipboard manager may take the data immediately and
                    // let wl-copy exit successfully. Leave the prior provider
                    // alone here; normal ownership loss makes it exit itself.
                    completed.push(ClipboardResult {
                        path: candidate.path,
                        copied: status.success(),
                    });
                }
                Err(_) => {
                    let _ = candidate.child.kill();
                    let _ = candidate.child.wait();
                    completed.push(ClipboardResult {
                        path: candidate.path,
                        copied: false,
                    });
                }
            }
        }
        // Never start a newer provider while an older one is still being
        // checked. Preserve the existing serialized liveness policy.
        while self.candidate.is_none() {
            let Some(path) = self.waiting.pop_front() else {
                break;
            };
            match spawn(&path) {
                Ok(child) => {
                    self.candidate = Some(ClipboardCandidate {
                        child,
                        path,
                        deadline: Instant::now() + CLIPBOARD_PROBE,
                    });
                }
                Err(_) => completed.push(ClipboardResult {
                    path,
                    copied: false,
                }),
            }
        }
        completed
    }

    fn shutdown(&mut self) {
        for mut child in self
            .provider
            .take()
            .into_iter()
            .chain(self.candidate.take().map(|candidate| candidate.child))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.waiting.clear();
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_clipboard(path: &Path) -> std::io::Result<Child> {
    // Foreground ownership lets us reap providers and keeps the exact saved
    // PNG bytes as input without retaining another encoded image in memory.
    let input = File::open(path)?;
    Command::new("wl-copy")
        .args(["--foreground", "--type", "image/png"])
        .stdin(input)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn review_command(kind: ReviewKind, path: &Path) -> Result<Command, String> {
    if !path.is_absolute() {
        return Err("The saved capture path must be absolute".into());
    }
    // Like wf-recorder, review apps must not inherit calloop's blocked signals.
    // The absolute path is one literal argument: spaces, shell metacharacters,
    // non-UTF8 names and option-looking basenames never become command syntax.
    let mut command = Command::new("env");
    command
        .args(["--default-signal=INT,TERM,HUP", "--", kind.program()])
        .arg(path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

struct RecordingPreview {
    child: Child,
    video: PathBuf,
    image: PathBuf,
    deadline: Instant,
}

impl RecordingPreview {
    fn start(video: &Path, directory: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let mut name = video.file_name().unwrap_or_default().to_os_string();
        name.push(".png");
        let image = directory.join(name);
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&image)?;
        // Decode just the first frame, including recordings shorter than 0.1s.
        // This child is polled alongside notifications: a slow decoder cannot
        // delay another screenshot, recording, or review launch.
        let child = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-threads", "1", "-i"])
            .arg(video)
            .args([
                "-frames:v", "1", "-vf",
                "scale=256:256:force_original_aspect_ratio=decrease",
                "-filter_threads", "1", "-threads", "1",
                "-f", "image2pipe", "-c:v", "png", "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(child) => Ok(Self {
                child,
                video: video.to_owned(),
                image,
                deadline: Instant::now() + Duration::from_secs(3),
            }),
            Err(error) => {
                let _ = std::fs::remove_file(image);
                Err(error)
            }
        }
    }

    /// None while pending; failed or timed-out previews use the media icon.
    fn poll(&mut self, now: Instant) -> Option<bool> {
        let ready = match self.child.try_wait() {
            Ok(None) if now < self.deadline => return None,
            Ok(Some(status)) => status.success()
                && tiny_skia::Pixmap::load_png(&self.image).is_ok(),
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                false
            }
        };
        if !ready {
            let _ = std::fs::remove_file(&self.image);
        }
        Some(ready)
    }
}

fn preview_directory() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".cache"))
                .filter(|path| path.is_absolute())
        })
        .map(|base| base.join("chonkstep/capture-previews"))
}

enum NotificationIcon<'a> {
    Image(&'a Path),
    Video,
    Warning,
}

impl NotificationIcon<'_> {
    fn argument(&self) -> (&'static str, &OsStr) {
        match self {
            Self::Image(path) => ("--icon", path.as_os_str()),
            // Quickshell's icon provider can return its pink checkerboard as
            // Image.Ready for missing themed names. Omarchy's glyph slot avoids
            // that provider entirely; other notification servers ignore the
            // hint and still show the result text.
            Self::Video => ("--hint", OsStr::new("string:omarchy-glyph:\u{f03d}")),
            Self::Warning => ("--hint", OsStr::new("string:omarchy-glyph:\u{f071}")),
        }
    }
}

impl Background {
    fn notify(&mut self, title: &str, message: &str) {
        self.notify_with_icon(title, message, NotificationIcon::Warning);
    }

    fn notify_with_icon(&mut self, title: &str, message: &str, icon: NotificationIcon<'_>) {
        tracing::info!(title, message, "capture result");
        // A broken notification service must not stall capture or accumulate
        // unlimited helper processes. The log always retains the result.
        if self.notifications.len() >= MAX_NOTIFICATIONS {
            return;
        }
        let (option, value) = icon.argument();
        if let Ok(child) = Command::new("notify-send")
            .args(["--app-name=Chonkstep Capture", option])
            .arg(value)
            .args(["--", title, message])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            self.notifications
                .push((child, Instant::now() + Duration::from_secs(2)));
        }
    }

    fn recording_saved(&mut self, video: &Path) {
        if self.previews.len() < MAX_PREVIEWS {
            if let Some(preview) = preview_directory()
                .and_then(|directory| RecordingPreview::start(video, &directory).ok())
            {
                self.previews.push(preview);
                return;
            }
        }
        self.notify_with_icon(
            "Recording saved",
            &video.display().to_string(),
            NotificationIcon::Video,
        );
    }

    fn review(&mut self, kind: ReviewKind, path: PathBuf) {
        let result = review_command(kind, &path)
            .and_then(|mut command| self.launch_review(kind, path.clone(), &mut command));
        if let Err(error) = result {
            self.notify(
                kind.failure_title(),
                &format!("{}\n{error}", path.display()),
            );
        }
    }

    fn launch_review(
        &mut self,
        kind: ReviewKind,
        path: PathBuf,
        command: &mut Command,
    ) -> Result<(), String> {
        if self.reviews.len() >= MAX_REVIEWS {
            return Err("Close an existing capture review window to open another".into());
        }
        let child = command
            .spawn()
            .map_err(|error| format!("{}: {error}", kind.program()))?;
        self.reviews.push(Review {
            child,
            kind,
            path,
            started: Instant::now(),
        });
        Ok(())
    }

    fn poll(&mut self) {
        let now = Instant::now();
        let mut finished = Vec::new();
        self.previews.retain_mut(|preview| {
            let Some(ready) = preview.poll(now) else { return true };
            finished.push((preview.video.clone(), ready.then(|| preview.image.clone())));
            false
        });
        for (video, image) in finished {
            // Keep successful previews in the cache so notification history
            // can reload them after the toast or notification helper exits.
            self.notify_with_icon(
                "Recording saved",
                &video.display().to_string(),
                image.as_deref().map_or(NotificationIcon::Video, NotificationIcon::Image),
            );
        }
        self.notifications
            .retain_mut(|(child, deadline)| match child.try_wait() {
                Ok(Some(_)) => false,
                Ok(None) if now < *deadline => true,
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    false
                }
            });
        for (kind, message) in self.reap_reviews() {
            self.notify(kind.failure_title(), &message);
        }
    }

    fn reap_reviews(&mut self) -> Vec<(ReviewKind, String)> {
        let mut failures = Vec::new();
        self.reviews.retain_mut(|review| match review.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                if !status.success() {
                    failures.push((review.kind, format!(
                        "{}\n{} exited with {status}. The saved file is intact.",
                        review.path.display(), review.kind.program()
                    )));
                }
                false
            }
            Err(error) => {
                tracing::warn!(%error, program = review.kind.program(), "could not reap capture review");
                false
            }
        });
        failures
    }

    fn interval(&self, recording: bool, clipboard: bool) -> Option<Duration> {
        if recording
            || !self.previews.is_empty()
            || !self.notifications.is_empty()
            || self
                .reviews
                .iter()
                .any(|review| review.started.elapsed() < Duration::from_secs(2))
        {
            Some(Duration::from_millis(100))
        } else if clipboard || !self.reviews.is_empty() {
            Some(Duration::from_secs(1))
        } else {
            None
        }
    }

    fn stop_notifications(&mut self) {
        for mut preview in self.previews.drain(..) {
            let _ = preview.child.kill();
            let _ = preview.child.wait();
            let _ = std::fs::remove_file(preview.image);
        }
        for (child, _) in &mut self.notifications {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.notifications.clear();
    }
}

pub(super) fn start() -> (
    SyncSender<Job>,
    Receiver<Update>,
    std::thread::JoinHandle<()>,
) {
    let (sender, jobs) = mpsc::sync_channel(2);
    let (updates, receiver) = mpsc::sync_channel(8);
    let worker = std::thread::spawn(move || {
        let mut recording: Option<Recording> = None;
        let mut clipboard = Clipboard::default();
        let mut background = Background::default();
        loop {
            // Reclaim completed launch slots and service clipboard deadlines
            // before blocking, without sleeping through unrelated commands.
            background.poll();
            for result in clipboard.poll(Instant::now(), spawn_clipboard) {
                let _ = updates.try_send(Update::ScreenshotDone);
                background.notify_with_icon(
                    if result.copied {
                        "Screenshot saved and copied"
                    } else {
                        "Screenshot saved (clipboard unavailable)"
                    },
                    &result.path.display().to_string(),
                    NotificationIcon::Image(&result.path),
                );
            }
            let mut interval =
                background.interval(recording.is_some(), clipboard.provider.is_some());
            if let Some(deadline) = clipboard.deadline() {
                let wait = deadline.saturating_duration_since(Instant::now());
                interval = Some(interval.map_or(wait, |interval| interval.min(wait)));
            }
            let job = match interval {
                Some(interval) => jobs.recv_timeout(interval),
                None => jobs
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match job {
                Ok(Job::Screenshot(pixels)) => {
                    match screenshot(pixels) {
                        Ok(path) => {
                            // The file is synced and published: opening it
                            // need not wait for the clipboard liveness check.
                            background.review(ReviewKind::Screenshot, path.clone());
                            if let Err(path) = clipboard.enqueue(path) {
                                let _ = updates.try_send(Update::ScreenshotDone);
                                background.notify_with_icon(
                                    "Screenshot saved (clipboard unavailable)",
                                    &path.display().to_string(),
                                    NotificationIcon::Image(&path),
                                );
                            }
                        }
                        Err(error) => {
                            let _ = updates.try_send(Update::ScreenshotDone);
                            background.notify("Screenshot failed", &error);
                        }
                    }
                }
                Ok(Job::Record {
                    output,
                    geometry,
                    filter,
                }) => {
                    if recording.is_none() {
                        match record(&output, &geometry, &filter) {
                            Ok(child) => recording = Some(child),
                            Err(error) => {
                                background.notify("Recording could not start", &error);
                                let _ = updates.try_send(Update::RecordingEnded);
                            }
                        }
                    }
                }
                Ok(Job::Stop) => {
                    if let Some(active) = recording.take() {
                        finish(active, &mut background, true);
                    }
                    let _ = updates.try_send(Update::RecordingEnded);
                }
                Ok(Job::Error(error)) => background.notify("Capture", &error),
                Ok(Job::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(active) = recording.take() {
                        // Logout preserves the recording without starting a
                        // new review application in a disappearing session.
                        finish(active, &mut background, false);
                    }
                    clipboard.shutdown();
                    background.stop_notifications();
                    // Review windows belong to the user: do not kill or wait
                    // for them at logout. The exiting compositor relinquishes
                    // live children to the system process supervisor.
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let exited = recording
                .as_mut()
                .is_some_and(|r| !matches!(r.child.try_wait(), Ok(None)));
            if exited {
                if let Some(mut active) = recording.take() {
                    let _ = active.child.wait();
                    background.notify(
                        "Recording stopped unexpectedly",
                        &format!(
                            "Recoverable file: {}\nDetails: {}",
                            active.partial.display(),
                            active.log.display()
                        ),
                    );
                }
                let _ = updates.try_send(Update::RecordingEnded);
            }
        }
    });
    (sender, receiver, worker)
}

fn directory(recording: bool) -> Result<PathBuf, String> {
    let override_key = if recording {
        "OMARCHY_SCREENRECORD_DIR"
    } else {
        "OMARCHY_SCREENSHOT_DIR"
    };
    if let Some(path) = std::env::var_os(override_key).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(path);
        return path
            .is_absolute()
            .then_some(path)
            .ok_or_else(|| format!("{override_key} must be an absolute directory path"));
    }
    let kind = if recording { "VIDEOS" } else { "PICTURES" };
    let base = Command::new("xdg-user-dir")
        .arg(kind)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute());
    let base = base
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(if recording { "Videos" } else { "Pictures" }))
        })
        .ok_or_else(|| "No home or capture directory is configured".to_string())?;
    Ok(base.join(if recording {
        "Recordings"
    } else {
        "Screenshots"
    }))
}

/// Reserve a unique, private temporary file on the destination filesystem.
/// Publishing uses a hard link (no replacement), so concurrent captures cannot
/// overwrite an existing screenshot even if clocks move backwards.
fn paths(recording: bool, extension: &str) -> Result<(PathBuf, PathBuf, File), String> {
    let directory = directory(recording)?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let stamp = Command::new("date")
        .arg("+%Y-%m-%d at %H.%M.%S")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "capture".into());
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!(
        "{} {stamp}-{unique}",
        if recording { "Recording" } else { "Screenshot" }
    );
    let destination = directory.join(format!("{name}.{extension}"));
    let partial = directory.join(format!(
        ".{name}.partial.{}",
        if recording { "mkv" } else { "png" }
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|e| e.to_string())?;
    Ok((partial, destination, file))
}

fn screenshot(pixels: DecorationBuffer) -> Result<PathBuf, String> {
    let (partial, destination, mut file) = paths(false, "png")?;
    let result = (|| {
        write_screenshot_png(pixels, &mut file)?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::hard_link(&partial, &destination).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&partial);
    result?;
    Ok(destination)
}

/// Encode owned premultiplied pixels using row-sized scratch. The previous
/// tiny-skia convenience method cloned the full image, then retained a complete
/// compressed stream and PNG vector before writing any bytes to the file.
fn write_screenshot_png(
    pixels: DecorationBuffer,
    destination: &mut impl Write,
) -> Result<(), String> {
    let size =
        tiny_skia::IntSize::from_wh(pixels.width, pixels.height).ok_or("Empty screenshot")?;
    let image = tiny_skia::Pixmap::from_vec(pixels.pixels, size)
        .ok_or("Invalid screenshot pixel length")?;
    // Buffer the small PNG chunk headers and row flushes into ordinary
    // filesystem writes. This memory is constant, independent of image height.
    let mut destination = std::io::BufWriter::with_capacity(
        64 * 1024,
        CheckedWriter {
            writer: destination,
            failed: false,
        },
    );
    let mut encoder = png::Encoder::new(&mut destination, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    write_png_rows(&image, &mut writer, 64 * 1024)?;
    // The row adapter checks compression and every IDAT write. Outer finish
    // checks IEND and the underlying flush before sync/publication is allowed.
    writer.finish().map_err(|e| e.to_string())
}

fn write_png_rows<W: Write>(
    image: &tiny_skia::Pixmap,
    writer: &mut png::Writer<W>,
    chunk_bytes: usize,
) -> Result<(), String> {
    let chunk_bytes = chunk_bytes.max(1);
    let failed = std::cell::Cell::new(false);
    let sink = IdatWriter {
        writer,
        bytes: Vec::with_capacity(chunk_bytes),
        chunk_bytes,
        error: None,
        failed: &failed,
    };
    let mut compressor = fdeflate::Compressor::new(sink).map_err(|e| e.to_string())?;
    let mut row = vec![0u8; image.width() as usize * 4 + 1];
    row[0] = 1; // PNG Sub, tiny-skia's original Fast encoding recipe.
    for source in image.pixels().chunks_exact(image.width() as usize) {
        if failed.get() {
            break;
        }
        let mut left = [0u8; 4];
        for (pixel, filtered) in source.iter().zip(row[1..].as_chunks_mut::<4>().0) {
            // Keep tiny-skia's exact alpha division and rounding. Fusing Sub
            // with demultiplication needs only one row of scratch.
            let color = pixel.demultiply();
            let current = [color.red(), color.green(), color.blue(), color.alpha()];
            *filtered = [
                current[0].wrapping_sub(left[0]),
                current[1].wrapping_sub(left[1]),
                current[2].wrapping_sub(left[2]),
                current[3].wrapping_sub(left[3]),
            ];
            left = current;
        }
        // Joining the filter byte and row preserves fdeflate's original
        // eight-byte grouping. png's StreamWriter splits these writes, which
        // sends opaque Sub rows through its slower trailing-zero path.
        compressor.write_data(&row).map_err(|e| e.to_string())?;
    }
    // fdeflate unwraps some header/checksum writes. IdatWriter never returns
    // an I/O error to it: latch the first error, discard subsequent output,
    // then report that error here before the outer PNG writer can finish.
    compressor.finish().map_err(|e| e.to_string())?.finish()
}

struct IdatWriter<'a, W: Write> {
    writer: &'a mut png::Writer<W>,
    bytes: Vec<u8>,
    chunk_bytes: usize,
    error: Option<png::EncodingError>,
    failed: &'a std::cell::Cell<bool>,
}

impl<W: Write> IdatWriter<'_, W> {
    fn flush_chunk(&mut self) {
        if self.error.is_none() && !self.bytes.is_empty() {
            if let Err(error) = self.writer.write_chunk(png::chunk::IDAT, &self.bytes) {
                self.error = Some(error);
                self.failed.set(true);
            }
            self.bytes.clear();
        }
    }

    fn finish(mut self) -> Result<(), String> {
        self.flush_chunk();
        match self.error {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }
}

impl<W: Write> Write for IdatWriter<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let consumed = bytes.len();
        while !bytes.is_empty() && self.error.is_none() {
            let count = bytes.len().min(self.chunk_bytes - self.bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.bytes.len() == self.chunk_bytes {
                self.flush_chunk();
            }
        }
        Ok(consumed)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_chunk();
        Ok(())
    }
}

/// Remember an underlying failure even when PNG finalization or BufWriter's
/// Drop tries again after a transient error. A partly written chunk must never
/// be retried and then published as duplicated or missing PNG bytes.
struct CheckedWriter<W> {
    writer: W,
    failed: bool,
}

impl<W: Write> Write for CheckedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.failed {
            return Err(std::io::Error::other("PNG output failed earlier"));
        }
        let result = match self.writer.write(bytes) {
            Ok(0) if !bytes.is_empty() => Err(std::io::ErrorKind::WriteZero.into()),
            result => result,
        };
        self.failed = result
            .as_ref()
            .is_err_and(|error| error.kind() != std::io::ErrorKind::Interrupted);
        result
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("PNG output failed earlier"));
        }
        let result = self.writer.flush();
        self.failed = result
            .as_ref()
            .is_err_and(|error| error.kind() != std::io::ErrorKind::Interrupted);
        result
    }
}

fn record(output: &str, geometry: &str, filter: &str) -> Result<Recording, String> {
    let (partial, destination, file) = paths(true, "mp4")?;
    drop(file);
    let log = partial.with_extension("log");
    let stderr = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            return Err(format!("Could not create recording log: {error}"));
        }
    };
    // Software H.264 is intentional: no VAAPI/NVIDIA assumption on Apple
    // Silicon / Asahi. Pad odd selections instead of dropping their last row.
    // Keep the recorder's explicit signal defaults for its SIGINT stop path.
    // General child signal delivery is established by termination.rs; this
    // older recorder-specific boundary also resets ignored dispositions.
    let child = Command::new("env")
        .args(["--default-signal=INT,TERM,HUP", "wf-recorder"])
        .args([
            "--no-dmabuf",
            "--no-damage",
            "-y",
            "-o",
            output,
            "-g",
            geometry,
            "-c",
            "libx264",
            "-p",
            "preset=veryfast",
            "-p",
            "crf=18",
            "-r",
            "60",
            "-x",
            "yuv420p",
            "-F",
            filter,
            "-f",
        ])
        .arg(&partial)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn();
    match child {
        Ok(child) => Ok(Recording {
            child,
            partial,
            destination,
            log,
        }),
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            let _ = std::fs::remove_file(&log);
            Err(format!("Install wf-recorder to record the screen: {error}"))
        }
    }
}

fn finish(mut active: Recording, background: &mut Background, review: bool) {
    // Signal only the child we own, never pkill/killall. SIGINT gives the muxer
    // time to flush its trailer. A crashed encoder still leaves a recoverable MKV.
    let _ = Command::new("kill")
        .args(["-INT", "--", &active.child.id().to_string()])
        .status();
    if !wait_child(&mut active.child, Duration::from_secs(8)) {
        background.notify(
            "Recording needs recovery",
            &format!(
                "{}\nDetails: {}",
                active.partial.display(),
                active.log.display()
            ),
        );
        return;
    }
    let temporary = active.partial.with_extension("mp4");
    let reserved = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .is_ok();
    // Keep muxer diagnostics with encoder diagnostics. A missing ffmpeg is
    // only one failure mode; disk errors and invalid media need their actual
    // explanation, not a misleading instruction to reinstall the encoder.
    let converted = reserved
        && OpenOptions::new()
            .append(true)
            .open(&active.log)
            .is_ok_and(|log| {
                Command::new("ffmpeg")
                    .args(["-nostdin", "-v", "error", "-y", "-i"])
                    .arg(&active.partial)
                    .args(["-c", "copy", "-movflags", "+faststart"])
                    .arg(&temporary)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(log)
                    .spawn()
                    .is_ok_and(|mut child| wait_child(&mut child, Duration::from_secs(30)))
            });
    if converted && std::fs::hard_link(&temporary, &active.destination).is_ok() {
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(&active.partial);
        let _ = std::fs::remove_file(&active.log);
        background.recording_saved(&active.destination);
        if review {
            background.review(ReviewKind::Recording, active.destination);
        }
    } else {
        if reserved {
            let _ = std::fs::remove_file(&temporary);
        }
        background.notify(
            "Recording preserved as MKV",
            &format!(
                "MP4 export failed. Video: {}\nDetails: {}",
                active.partial.display(),
                active.log.display()
            ),
        );
    }
}

fn wait_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod png_tests;

#[cfg(test)]
mod clipboard_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        directory: PathBuf,
        background: Background,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "chonk-capture-review-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            // PATH belongs to each Command, never the multithreaded test process.
            // GNU env is real; both review apps are isolated argument recorders.
            symlink("/usr/bin/env", directory.join("env")).unwrap();
            for program in ["xdg-open", "omacut"] {
                let executable = directory.join(program);
                std::fs::write(&executable, b"#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > \"$CAPTURE_REVIEW_LOG\"\nif [ \"$CAPTURE_REVIEW_WAIT\" = 1 ]; then exec /usr/bin/sleep 30; fi\nexit \"$CAPTURE_REVIEW_EXIT\"\n").unwrap();
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            Self {
                directory,
                background: Background::default(),
            }
        }

        fn command(&self, kind: ReviewKind, path: &Path, code: u8, wait: bool) -> Command {
            let mut command = review_command(kind, path).unwrap();
            command
                .current_dir(&self.directory)
                .env("PATH", &self.directory)
                .env("CAPTURE_REVIEW_LOG", self.directory.join("arguments"))
                .env("CAPTURE_REVIEW_EXIT", code.to_string())
                .env("CAPTURE_REVIEW_WAIT", if wait { "1" } else { "0" });
            command
        }

        fn wait_until(&mut self, mut ready: impl FnMut(&mut Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !ready(self) {
                assert!(Instant::now() < deadline, "fixture child did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.background.stop_notifications();
            for review in &mut self.background.reviews {
                let _ = review.child.kill();
                let _ = review.child.wait();
            }
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn review_routes_preserve_literal_non_utf8_and_hostile_filenames() {
        for kind in [ReviewKind::Screenshot, ReviewKind::Recording] {
            let mut fixture = Fixture::new();
            let basename = std::ffi::OsString::from_vec(
                b"--capture with spaces;$(touch injected)`touch injected`\n\xff.png".to_vec(),
            );
            let path = fixture.directory.join(basename);
            std::fs::write(&path, b"published capture").unwrap();
            let mut command = fixture.command(kind, &path, 0, false);
            fixture
                .background
                .launch_review(kind, path.clone(), &mut command)
                .unwrap();
            fixture.wait_until(|fixture| {
                assert!(fixture.background.reap_reviews().is_empty());
                fixture.background.reviews.is_empty()
            });
            let args = std::fs::read(fixture.directory.join("arguments")).unwrap();
            let mut expected = fixture
                .directory
                .join(kind.program())
                .as_os_str()
                .as_bytes()
                .to_vec();
            expected.push(0);
            expected.extend_from_slice(path.as_os_str().as_bytes());
            expected.push(0);
            assert_eq!(args, expected);
            assert_eq!(std::fs::read(&path).unwrap(), b"published capture");
            assert!(!fixture.directory.join("injected").exists());
        }
        assert!(review_command(ReviewKind::Screenshot, Path::new("-relative.png")).is_err());
    }

    #[test]
    fn failed_and_missing_reviewers_report_failure_without_touching_saved_file() {
        for missing in [false, true] {
            let mut fixture = Fixture::new();
            let kind = ReviewKind::Recording;
            let path = fixture.directory.join("saved recording.mp4");
            std::fs::write(&path, b"published recording").unwrap();
            if missing {
                std::fs::remove_file(fixture.directory.join(kind.program())).unwrap();
            }
            let mut command = fixture.command(kind, &path, 17, false);
            fixture
                .background
                .launch_review(kind, path.clone(), &mut command)
                .unwrap();
            let mut failures = Vec::new();
            fixture.wait_until(|fixture| {
                failures.extend(fixture.background.reap_reviews());
                fixture.background.reviews.is_empty()
            });
            assert_eq!(failures.len(), 1);
            assert_eq!(
                failures[0].0.failure_title(),
                "Recording saved; Omacut unavailable"
            );
            assert!(failures[0].1.contains(if missing { "127" } else { "17" }));
            assert!(failures[0].1.contains("The saved file is intact"));
            assert_eq!(std::fs::read(&path).unwrap(), b"published recording");
        }
    }

    #[test]
    fn review_launch_is_nonblocking_and_live_children_are_bounded() {
        let mut fixture = Fixture::new();
        let kind = ReviewKind::Screenshot;
        let path = fixture.directory.join("saved.png");
        let mut command = fixture.command(kind, &path, 0, true);
        for _ in 0..MAX_REVIEWS {
            fixture
                .background
                .launch_review(kind, path.clone(), &mut command)
                .unwrap();
        }
        // Every child is still running. A launch that waited for the viewer
        // would have stalled this test for 30 seconds per screenshot.
        assert!(fixture.background.reviews.iter_mut().all(|r| r
            .child
            .try_wait()
            .unwrap()
            .is_none()));
        assert!(fixture
            .background
            .launch_review(kind, path, &mut command)
            .is_err());
        assert_eq!(fixture.background.reviews.len(), MAX_REVIEWS);
        assert!(fixture.background.reap_reviews().is_empty());
    }

    #[test]
    fn spawn_failure_is_reported_without_retaining_a_child() {
        let mut fixture = Fixture::new();
        let path = fixture.directory.join("saved.png");
        let mut command = Command::new(fixture.directory.join("missing executable"));
        let error = fixture
            .background
            .launch_review(ReviewKind::Screenshot, path, &mut command)
            .unwrap_err();
        assert!(error.contains("xdg-open"));
        assert!(fixture.background.reviews.is_empty());
    }

    #[test]
    fn inactive_capture_worker_has_no_periodic_wakeups() {
        let mut fixture = Fixture::new();
        assert_eq!(fixture.background.interval(false, false), None);
        assert_eq!(
            fixture.background.interval(true, false),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            fixture.background.interval(false, true),
            Some(Duration::from_secs(1))
        );
        let path = fixture.directory.join("saved.png");
        let mut command = fixture.command(ReviewKind::Screenshot, &path, 0, true);
        fixture
            .background
            .launch_review(ReviewKind::Screenshot, path, &mut command)
            .unwrap();
        assert_eq!(
            fixture.background.interval(false, false),
            Some(Duration::from_millis(100))
        );
        fixture.background.reviews[0].started -= Duration::from_secs(3);
        assert_eq!(
            fixture.background.interval(false, false),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn notification_timeout_reaps_only_helper_and_preserves_live_review() {
        let mut fixture = Fixture::new();
        let path = fixture.directory.join("saved.png");
        let mut command = fixture.command(ReviewKind::Screenshot, &path, 0, true);
        fixture
            .background
            .launch_review(ReviewKind::Screenshot, path, &mut command)
            .unwrap();
        let helper = command.spawn().unwrap();
        fixture
            .background
            .notifications
            .push((helper, Instant::now()));
        fixture.background.poll();
        assert!(fixture.background.notifications.is_empty());
        assert!(fixture.background.reviews[0]
            .child
            .try_wait()
            .unwrap()
            .is_none());
        fixture.background.notifications.push((
            command.spawn().unwrap(),
            Instant::now() + Duration::from_secs(2),
        ));
        fixture.background.stop_notifications();
        assert!(fixture.background.notifications.is_empty());
        assert!(fixture.background.reviews[0]
            .child
            .try_wait()
            .unwrap()
            .is_none());
    }

    #[test]
    fn recording_preview_requires_successful_decode_and_keeps_valid_images_for_history() {
        for (valid_png, exit_code) in [(true, 0), (false, 0), (true, 7)] {
            let mut fixture = Fixture::new();
            let image = fixture.directory.join("preview.png");
            if valid_png {
                tiny_skia::Pixmap::new(8, 4).unwrap().save_png(&image).unwrap();
            } else {
                std::fs::write(&image, b"incomplete PNG").unwrap();
            }
            let video = fixture.directory.join("saved.mp4");
            std::fs::write(&video, b"saved video").unwrap();
            let child = Command::new("/bin/sh")
                .args(["-c", &format!("exit {exit_code}")])
                .spawn().unwrap();
            let mut preview = RecordingPreview {
                child, video: video.clone(), image: image.clone(),
                deadline: Instant::now() + Duration::from_secs(3),
            };
            let mut ready = None;
            fixture.wait_until(|_| {
                ready = preview.poll(Instant::now());
                ready.is_some()
            });
            assert_eq!(ready, Some(valid_png && exit_code == 0));
            assert_eq!(image.exists(), ready.unwrap());
            assert_eq!(std::fs::read(video).unwrap(), b"saved video");
        }
    }

    #[test]
    fn recording_preview_timeout_and_shutdown_reap_decoder_and_remove_partial_image() {
        for shutdown in [false, true] {
            let mut fixture = Fixture::new();
            let image = fixture.directory.join("partial.png");
            std::fs::write(&image, b"incomplete PNG").unwrap();
            let child = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut preview = RecordingPreview {
                child, video: fixture.directory.join("saved.mp4"), image: image.clone(), deadline,
            };
            assert_eq!(preview.poll(deadline - Duration::from_secs(1)), None);
            assert!(image.exists());
            if shutdown {
                fixture.background.previews.push(preview);
                assert_eq!(fixture.background.interval(false, false), Some(Duration::from_millis(100)));
                fixture.background.stop_notifications();
                assert!(fixture.background.previews.is_empty());
                assert_eq!(fixture.background.interval(false, false), None);
            } else {
                assert_eq!(preview.poll(deadline), Some(false));
                assert!(preview.child.try_wait().unwrap().is_some());
            }
            assert!(!image.exists());
        }
    }
}
