use chonk_dock::{apps, client::Platform, runtime, startup};
use std::time::{Duration, Instant};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))
        .init();
    if let Err(error) = run() {
        eprintln!("chonk-dock: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = runtime::Options::default();
    let mut operation = "start";
    let mut pin = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("Chonk Dock — standalone NeXTSTEP dock\n\nchonk-dock [--x11 | --wayland] [--output NAME] [--theme ID] [--scale FACTOR]\nchonk-dock --stop | --toggle | --status\nchonk-dock --list-apps | --pin DESKTOP-ID\n\nRight-click instruments for panels; middle-drag to reorder.\nClick the identity tile for applications, pins, and Quit.\nThe dock is opt-in and is never started by the Omarchy preset.");
                return Ok(());
            }
            "--version" => {
                println!("chonk-dock {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--x11" => options.platform = Platform::X11,
            "--wayland" => options.platform = Platform::Wayland,
            "--stop" => operation = "stop",
            "--toggle" => operation = "toggle",
            "--status" => operation = "status",
            "--output" => options.output = Some(args.next().ok_or("--output needs a name")?),
            "--theme" => options.theme = Some(args.next().ok_or("--theme needs an id")?),
            "--scale" => {
                options.scale = args.next().ok_or("--scale needs a number")?.parse()?;
                if !options.scale.is_finite() || !(0.5..=4.0).contains(&options.scale) {
                    return Err("scale must be between 0.5 and 4".into());
                }
            }
            "--list-apps" => {
                for app in apps::scan_applications() {
                    println!("{}\t{}", app.id, app.name);
                }
                return Ok(());
            }
            "--pin" => pin = Some(args.next().ok_or("--pin needs a desktop id")?),
            _ => return Err(format!("unknown option {arg}; use --help").into()),
        }
    }
    if let Some(theme) = &options.theme {
        if theme != "omarchy" && wm_theme::default_theme::theme_by_id(theme).is_none() {
            return Err(format!("unknown theme {theme}").into());
        }
    }
    let path = runtime::socket_path(options.platform)?;
    if let Some(pin) = pin {
        let apps = apps::scan_applications();
        if !apps.iter().any(|app| app.id == pin) {
            return Err(format!("unknown desktop id {pin}; use --list-apps").into());
        }
        let state = startup::state_file("dock").ok_or("no state directory")?;
        let mut pins: Vec<String> = std::fs::read_to_string(&state)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        if !pins.contains(&pin) {
            pins.push(pin);
        }
        std::fs::create_dir_all(state.parent().unwrap())?;
        std::fs::write(state, format!("{}\n", pins.join("\n")))?;
        let _ = request(&path, "reload");
        return Ok(());
    }
    match request(
        &path,
        if matches!(operation, "stop" | "toggle") {
            "stop"
        } else {
            "status"
        },
    ) {
        Ok(reply) => {
            println!("{reply}");
            return Ok(());
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e.into()),
    }
    if matches!(operation, "stop" | "status") {
        println!("stopped");
        return Ok(());
    }
    runtime::run(options)
}

fn request(path: &std::path::Path, command: &str) -> std::io::Result<String> {
    let stream = chonk_ipc::Stream::connect(path)?;
    let bytes = format!("{command}\n");
    if stream.send(bytes.as_bytes())? != bytes.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "incomplete dock command",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut reply = Vec::new();
    let mut bytes = [0u8; 256];
    while reply.len() < 256 {
        match stream.recv_until(&mut bytes, deadline)? {
            Some(0) => break,
            Some(n) => {
                reply.extend_from_slice(&bytes[..n]);
                if reply.contains(&b'\n') {
                    break;
                }
            }
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "dock did not respond",
                ))
            }
        }
    }
    Ok(String::from_utf8_lossy(&reply).trim().to_string())
}
