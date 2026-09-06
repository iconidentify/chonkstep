//! All filesystem and child-process work stays away from the compositor loop.
// Every blocking method in this module runs on its single dedicated I/O worker;
// no renderer, input callback or shell tick waits for these children.
#![allow(clippy::disallowed_methods)]
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
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

pub(super) fn start() -> (
    SyncSender<Job>,
    Receiver<Update>,
    std::thread::JoinHandle<()>,
) {
    let (sender, jobs) = mpsc::sync_channel(2);
    let (updates, receiver) = mpsc::sync_channel(8);
    let worker = std::thread::spawn(move || {
        let mut recording: Option<Recording> = None;
        let mut clipboard: Option<Child> = None;
        loop {
            match jobs.recv_timeout(Duration::from_millis(100)) {
                Ok(Job::Screenshot(pixels)) => {
                    match screenshot(pixels, &mut clipboard) {
                        Ok((path, copied)) => notify(
                            if copied {
                                "Screenshot saved and copied"
                            } else {
                                "Screenshot saved (clipboard unavailable)"
                            },
                            &path.display().to_string(),
                        ),
                        Err(error) => notify("Screenshot failed", &error),
                    }
                    let _ = updates.try_send(Update::ScreenshotDone);
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
                                notify("Recording could not start", &error);
                                let _ = updates.try_send(Update::RecordingEnded);
                            }
                        }
                    }
                }
                Ok(Job::Stop) => {
                    if let Some(active) = recording.take() {
                        finish(active);
                    }
                    let _ = updates.try_send(Update::RecordingEnded);
                }
                Ok(Job::Error(error)) => notify("Capture", &error),
                Ok(Job::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(active) = recording.take() {
                        finish(active);
                    }
                    if let Some(mut child) = clipboard.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
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
                    notify(
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
            if clipboard
                .as_mut()
                .is_some_and(|c| !matches!(c.try_wait(), Ok(None)))
            {
                clipboard = None;
            }
        }
    });
    (sender, receiver, worker)
}

fn notify(title: &str, message: &str) {
    tracing::info!(title, message, "capture result");
    if let Ok(mut child) = Command::new("notify-send")
        .args([
            "--app-name=Chonkstep Capture",
            "--icon=camera-photo",
            title,
            message,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let _ = wait_child(&mut child, Duration::from_secs(2));
    }
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

fn screenshot(
    pixels: DecorationBuffer,
    clipboard: &mut Option<Child>,
) -> Result<(PathBuf, bool), String> {
    let size =
        tiny_skia::IntSize::from_wh(pixels.width, pixels.height).ok_or("Empty screenshot")?;
    let image = tiny_skia::Pixmap::from_vec(pixels.pixels, size)
        .ok_or("Invalid screenshot pixel length")?;
    // tiny-skia's PNG encoder unpremultiplies alpha; transparent window edges
    // must not be saved as dark halos.
    let png = image.encode_png().map_err(|e| e.to_string())?;
    let (partial, destination, mut file) = paths(false, "png")?;
    let result = (|| {
        file.write_all(&png)?;
        file.sync_all()?;
        std::fs::hard_link(&partial, &destination)
    })();
    let _ = std::fs::remove_file(&partial);
    result.map_err(|e| e.to_string())?;
    let input = File::open(&destination).map_err(|e| e.to_string())?;
    // Foreground lets us own and reap the clipboard provider, instead of
    // accumulating detached children. It serves the exact saved PNG bytes.
    let child = Command::new("wl-copy")
        .args(["--foreground", "--type", "image/png"])
        .stdin(input)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let copied = match child {
        Ok(mut child) => {
            std::thread::sleep(Duration::from_millis(100));
            let status = child.try_wait();
            if matches!(status, Ok(None)) {
                if let Some(mut old) = clipboard.take() {
                    let _ = old.kill();
                    let _ = old.wait();
                }
                *clipboard = Some(child);
                true
            } else {
                // A clipboard manager can take ownership immediately and let
                // wl-copy exit successfully; that is still a successful copy.
                if status.is_err() {
                    let _ = child.kill();
                }
                child.wait().is_ok_and(|status| status.success())
            }
        }
        Err(_) => false,
    };
    Ok((destination, copied))
}

fn record(output: &str, geometry: &str, filter: &str) -> Result<Recording, String> {
    let (partial, destination, file) = paths(true, "mp4")?;
    drop(file);
    let log = partial.with_extension("log");
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log)
        .map_err(|e| e.to_string())?;
    // Software H.264 is intentional: no VAAPI/NVIDIA assumption on Apple
    // Silicon / Asahi. Pad odd selections instead of dropping their last row.
    // calloop uses signalfd, so children inherit blocked INT/TERM/HUP. GNU env
    // resets their disposition AND mask before exec; otherwise Stop would hang
    // until the hard timeout despite delivering SIGINT to the correct process.
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

fn finish(mut active: Recording) {
    // Signal only the child we own, never pkill/killall. SIGINT gives the muxer
    // time to flush its trailer. A crashed encoder still leaves a recoverable MKV.
    let _ = Command::new("kill")
        .args(["-INT", "--", &active.child.id().to_string()])
        .status();
    if !wait_child(&mut active.child, Duration::from_secs(8)) {
        notify(
            "Recording needs recovery",
            &active.partial.display().to_string(),
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
    let converted = reserved
        && Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-i"])
            .arg(&active.partial)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&temporary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok_and(|mut child| wait_child(&mut child, Duration::from_secs(30)));
    if converted && std::fs::hard_link(&temporary, &active.destination).is_ok() {
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(&active.partial);
        let _ = std::fs::remove_file(&active.log);
        notify("Recording saved", &active.destination.display().to_string());
    } else {
        if reserved {
            let _ = std::fs::remove_file(&temporary);
        }
        notify(
            "Recording preserved as MKV",
            &format!(
                "{}\nInstall ffmpeg for MP4 export.",
                active.partial.display()
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
