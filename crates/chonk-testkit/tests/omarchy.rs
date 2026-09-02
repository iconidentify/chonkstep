//! Following Omarchy's theme, end to end: a session configured with
//! `theme = "omarchy"` dresses in the palette under Omarchy's state
//! directory at boot, and re-dresses — appearance included — when
//! that palette is swapped underneath it, with nobody telling it to.
//!
//! Omarchy is *simulated*: the harness plants
//! `$XDG_STATE_HOME/omarchy/current/theme/colors.toml` and
//! `current/theme.name` exactly where `omarchy-theme-set` writes them,
//! and swaps them the way it does (palette first, name last). The
//! real Omarchy is never touched — this runs against the isolated
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
            ("omarchy/current/theme/colors.toml".to_string(), TOKYO_NIGHT.to_string()),
            ("omarchy/current/theme.name".to_string(), "tokyo-night\n".to_string()),
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
