//! Following Omarchy's theme, end to end: a session configured with
//! `theme = "omarchy"` dresses in the palette under Omarchy's state
//! directory at boot, and re-dresses — appearance included — when
//! that palette is swapped underneath it, with nobody telling it to.
//!
//! Omarchy is *simulated*: the harness plants
//! `$XDG_STATE_HOME/omarchy/current/theme/colors.toml` and
//! `current/theme.name` exactly where `omarchy-theme-set` writes them,
//! and swaps them the way it does (palette first, name last); the
//! background is a `current/background` link into the theme's
//! `backgrounds/`, repointed the way `omarchy-theme-bg-next` does it.
//! The real Omarchy is never touched — this runs against the isolated
//! state root every testkit session gets.
//!
//! `#[ignore]`d like the rest of the suite (needs a Wayland session
//! to nest in); `cargo test -p chonk-testkit --test omarchy -- --ignored`.

use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

/// Tokyo Night, as Omarchy ships it.
const TOKYO_NIGHT: &str = r##"mode = "dark"
accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"
background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"
foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"
red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"
bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

/// Catppuccin Latte — a *light* palette, so the swap is observable
/// as an appearance flip and not only as a change of hue.
const CATPPUCCIN_LATTE: &str = r##"mode = "light"
accent = "#1e66f5"
selection = "#ccd0da"
muted = "#acb0be"
background = "#eff1f5"
dark_background = "#e3e4e8"
darker_background = "#d7d8dc"
lighter_background = "#dce0e8"
foreground = "#4c4f69"
dark_foreground = "#9ca0b0"
light_foreground = "#5c5f77"
bright_foreground = "#4c4f69"
red = "#d20f39"
yellow = "#df8e1d"
orange = "#d84e2b"
green = "#40a02b"
cyan = "#179299"
blue = "#1e66f5"
magenta = "#ea76cb"
brown = "#6c2715"
bright_red = "#d20f39"
bright_yellow = "#df8e1d"
bright_green = "#40a02b"
bright_cyan = "#179299"
bright_blue = "#1e66f5"
bright_magenta = "#ea76cb"
"##;

fn following_options() -> SessionOptions {
    SessionOptions {
        scale: Some(1.0),
        config_extra: "theme = \"omarchy\"\n".to_string(),
        state_root_files: vec![
            ("omarchy/current/theme/colors.toml".to_string(), TOKYO_NIGHT.into()),
            ("omarchy/current/theme.name".to_string(), "tokyo-night\n".into()),
        ],
        ..SessionOptions::default()
    }
}

/// What `omarchy-theme-set` does to the state directory, in its order:
/// the palette lands first, the name is written last.
fn omarchy_sets_theme(session: &Session, dir_name: &str, colors: &str) {
    let current = session.dir.join("state/omarchy/current");
    std::fs::write(current.join("theme/colors.toml"), colors).unwrap();
    std::fs::write(current.join("theme.name"), format!("{dir_name}\n")).unwrap();
}

/// A solid-colour PNG, the smallest picture that can stand in for one
/// of Omarchy's backgrounds: cover-scaled to the screen it is the same
/// colour everywhere, so one sample anywhere on bare desk reads it.
fn solid_png(rgb: [u8; 3]) -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgb.repeat((w * h) as usize)).unwrap();
    }
    bytes
}

/// What `omarchy-theme-bg-set` does: `ln -nsf <image> current/background`.
fn omarchy_sets_background(session: &Session, image: &str) {
    let current = session.dir.join("state/omarchy/current");
    let link = current.join("background");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(current.join("theme/backgrounds").join(image), link).unwrap();
}

/// The colour of bare desk at the centre of the screen — under no
/// chrome, no window — as a 40×40 mean.
fn desk_colour(shot: &chonk_testkit::Screenshot) -> [f64; 3] {
    shot.mean_rgb(shot.width / 2 - 20, shot.height / 2 - 20, 40, 40)
}

fn near(actual: [f64; 3], expected: [u8; 3]) -> bool {
    actual.iter().zip(expected).all(|(a, e)| (a - e as f64).abs() < 12.0)
}

fn mean_brightness(shot: &chonk_testkit::Screenshot) -> f64 {
    let m = shot.mean_rgb(0, 0, shot.width, shot.height);
    (m[0] + m[1] + m[2]) / 3.0
}

#[test]
#[ignore]
fn a_session_told_to_follow_omarchy_wears_its_palette_and_re_dresses_when_it_changes() {
    let mut session = Session::boot("omarchy-follow", following_options()).unwrap();
    session.door().barrier().unwrap();

    // Boot: the shell says, in its own words, that it wears Omarchy's
    // theme — the follow id, Omarchy's display name, the palette's mode.
    let world = session.door().windows().unwrap();
    assert_eq!(world.theme.id, "omarchy", "{}", session.log());
    assert_eq!(world.theme.name, "Omarchy (Tokyo Night)");
    assert_eq!(world.theme.appearance, "dark");
    assert_eq!(world.theme.following, "omarchy");
    let published = session.state_file("appearance");
    let read_mode = || std::fs::read_to_string(&published).map(|s| s.trim().to_string()).unwrap_or_default();
    assert_eq!(read_mode(), "dark", "the palette's mode is the published appearance");
    let dark = session.screenshot("tokyo-night").unwrap();

    // Omarchy switches to a light theme. Nobody tells the session; the
    // one-hertz watch has to notice on its own.
    omarchy_sets_theme(&session, "catppuccin-latte", CATPPUCCIN_LATTE);
    poll_until(Duration::from_secs(30), "the session to re-dress in the new Omarchy theme", || {
        let world = session.door().windows().ok()?;
        (world.theme.name == "Omarchy (Catppuccin Latte)").then_some(world)
    })
    .map(|world| {
        assert_eq!(world.theme.id, "omarchy");
        assert_eq!(world.theme.following, "omarchy");
        assert_eq!(world.theme.appearance, "light", "a light Omarchy theme is a light desk");
    })
    .expect("the Omarchy theme change was never picked up");
    assert!(session.compositor_alive(), "re-dressing killed the compositor");
    poll_until(Duration::from_secs(10), "the appearance to be republished", || (read_mode() == "light").then_some(()))
        .expect("the published appearance did not follow the palette's mode");

    session.door().barrier().unwrap();
    let light = session.screenshot("catppuccin-latte").unwrap();
    assert!(
        mean_brightness(&light) > mean_brightness(&dark) + 25.0,
        "the desk should be visibly lighter in Latte: {:.1} vs {:.1} ({}, {})",
        mean_brightness(&dark),
        mean_brightness(&light),
        dark.path.display(),
        light.path.display()
    );
}

#[test]
#[ignore]
fn an_appearance_request_is_declined_while_following_omarchy() {
    let mut session = Session::boot("omarchy-appearance", following_options()).unwrap();
    session.door().barrier().unwrap();
    assert_eq!(session.door().windows().unwrap().theme.appearance, "dark");

    // The request is consumed (a marker left lying around would fire
    // forever) but not honoured: Omarchy's theme decides the mode.
    let request = session.state_file("appearance-request");
    std::fs::write(&request, "light").unwrap();
    poll_until(Duration::from_secs(30), "the appearance request to be consumed", || (!request.exists()).then_some(()))
        .expect("the request marker was never consumed");
    session.door().barrier().unwrap();
    assert!(session.compositor_alive(), "the request killed the compositor");
    let world = session.door().windows().unwrap();
    assert_eq!(world.theme.appearance, "dark", "the mode did not flip");
    assert_eq!(world.theme.name, "Omarchy (Tokyo Night)", "and the dress did not change");
    assert_eq!(std::fs::read_to_string(session.state_file("appearance")).unwrap().trim(), "dark");
    assert!(session.log().contains("follows Omarchy, whose theme decides the mode"), "declined out loud:\n{}", session.log());
}

#[test]
#[ignore]
fn following_with_no_omarchy_palette_wears_the_default_until_one_appears() {
    let mut session = Session::boot(
        "omarchy-absent",
        SessionOptions { scale: Some(1.0), config_extra: "theme = \"omarchy\"\n".to_string(), ..SessionOptions::default() },
    )
    .unwrap();
    session.door().barrier().unwrap();
    let world = session.door().windows().unwrap();
    assert_eq!(world.theme.id, "nextstep-classic", "no palette: the flagship stands in");
    assert_eq!(world.theme.following, "omarchy", "but the choice to follow stands");
    assert!(session.log().contains("no readable colors.toml"), "said once in the log:\n{}", session.log());

    // Omarchy gets installed and sets a theme: the session notices.
    std::fs::create_dir_all(session.dir.join("state/omarchy/current/theme")).unwrap();
    omarchy_sets_theme(&session, "tokyo-night", TOKYO_NIGHT);
    poll_until(Duration::from_secs(30), "the session to pick up the palette that appeared", || {
        let world = session.door().windows().ok()?;
        (world.theme.id == "omarchy").then_some(world)
    })
    .map(|world| assert_eq!(world.theme.name, "Omarchy (Tokyo Night)"))
    .expect("a palette appearing after boot was never picked up");
}

/// A follow desk wears Omarchy's own background: the picture behind
/// `current/background` is the wallpaper from the first frame, with
/// the Wallpaper menu never touched; a cycle to the next picture —
/// which moves the link and nothing under `theme/`, so the palette
/// and the resolved theme are exactly as they were — repaints it
/// within the watch's second; and a theme change that lands with the
/// palette repaints palette and picture together.
#[test]
#[ignore]
fn a_follow_desk_wears_omarchys_background_and_repaints_when_it_is_cycled() {
    const GREEN: [u8; 3] = [0x20, 0xA0, 0x40];
    const PURPLE: [u8; 3] = [0x80, 0x20, 0xA0];
    let mut options = following_options();
    options.state_root_files.push(("omarchy/current/theme/backgrounds/1-green.png".to_string(), solid_png(GREEN)));
    options.state_root_files.push(("omarchy/current/theme/backgrounds/2-purple.png".to_string(), solid_png(PURPLE)));
    options.state_root_links.push(("omarchy/current/background".to_string(), "omarchy/current/theme/backgrounds/1-green.png".to_string()));
    let mut session = Session::boot("omarchy-background", options).unwrap();
    session.door().barrier().unwrap();

    // Boot: nothing persisted about the wallpaper, so the theme's own
    // — Omarchy's picture — is what the desk shows.
    let green = session.screenshot("green").unwrap();
    assert!(near(desk_colour(&green), GREEN), "the desk should wear Omarchy's background at boot: {:?} ({})", desk_colour(&green), green.path.display());

    // `omarchy-theme-bg-next`: the link moves; the theme does not.
    omarchy_sets_background(&session, "2-purple.png");
    let purple = poll_until(Duration::from_secs(30), "the desk to repaint in the next background", || {
        let shot = session.screenshot("purple").ok()?;
        near(desk_colour(&shot), PURPLE).then_some(shot)
    })
    .expect("cycling Omarchy's background was never picked up");
    let world = session.door().windows().unwrap();
    assert_eq!(world.theme.name, "Omarchy (Tokyo Night)", "a background swap alone leaves the theme as it was");
    assert!(session.compositor_alive(), "repainting killed the compositor: {}", session.log());
    drop(purple);

    // A new theme arrives with its own picture — as `omarchy-theme-set`
    // does it, the background link last of all.
    omarchy_sets_theme(&session, "catppuccin-latte", CATPPUCCIN_LATTE);
    omarchy_sets_background(&session, "1-green.png");
    poll_until(Duration::from_secs(30), "palette and picture to change together", || {
        let world = session.door().windows().ok()?;
        (world.theme.name == "Omarchy (Catppuccin Latte)").then_some(())?;
        let shot = session.screenshot("latte-green").ok()?;
        near(desk_colour(&shot), GREEN).then_some(shot)
    })
    .expect("the theme change did not carry its background with it");
}
