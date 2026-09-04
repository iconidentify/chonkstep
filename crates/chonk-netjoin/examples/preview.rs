//! Renders every designed state of the join dialog to PNGs, in both
//! appearances, so the look can be reviewed without a wifi radio, a
//! secured network in range, or a wrong passphrase to type at it.
//!
//! `cargo run -p chonk-netjoin --example preview-netjoin -- <outdir>`
//!
//! The twin of `wm-theme`'s own `preview_*` examples, and the reason
//! this crate carries `wm-theme` as a dev-dependency: an app needs one
//! theme at runtime, but a design pass needs all of them.

use chonk_netjoin::dialog::{JoinDialog, Target};
use chonk_netjoin::keys::Key;
use chonk_netjoin::render::{layout, render_join_dialog};
use wm_theme::default_theme::theme_variant;
use wm_theme::model::Appearance;

/// One named state, built by driving the real state machine to it.
type State = (&'static str, Box<dyn Fn(&mut JoinDialog)>);

fn typed(dialog: &mut JoinDialog, text: &str) {
    for c in text.chars() {
        dialog.on_key(Key::Char(c));
    }
}

fn press(dialog: &mut JoinDialog, target: Target) {
    let l = layout(1.0);
    let (x, y) = l.center(target).expect("placed");
    dialog.on_press(x, y, &l);
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();

    let states: Vec<State> = vec![
        ("01-empty", Box::new(|_: &mut JoinDialog| {})),
        ("02-typed", Box::new(|d: &mut JoinDialog| typed(d, "correct horse battery"))),
        (
            "03-revealed",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "correct horse battery");
                d.on_key(Key::ToggleReveal);
            }),
        ),
        (
            "04-focus-join",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
                d.on_key(Key::Tab { back: false });
                d.on_key(Key::Tab { back: false });
            }),
        ),
        (
            "05-pressed-cancel",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
                press(d, Target::Cancel);
            }),
        ),
        (
            "06-joining",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
                d.on_key(Key::Enter);
            }),
        ),
        (
            "07-failed",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
                d.on_key(Key::Enter);
                d.finished(false, "Error: Connection activation failed: (7) Secrets were required, but not provided.\n");
            }),
        ),
        (
            "08-joined",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
                d.on_key(Key::Enter);
                d.finished(true, "");
            }),
        ),
        (
            "09-long-ssid",
            Box::new(|d: &mut JoinDialog| {
                typed(d, "hunter2");
            }),
        ),
    ];

    for (appearance, tag) in [(Appearance::Dark, "dark"), (Appearance::Light, "light")] {
        let theme = theme_variant("nextstep-classic", appearance).expect("the flagship theme has both appearances");
        for (name, build) in &states {
            let ssid = if name.contains("long-ssid") { "Guest Network — Please Ask At Reception 5GHz" } else { "Cafe Wifi" };
            let mut dialog = JoinDialog::new(ssid);
            build(&mut dialog);
            let pixmap = render_join_dialog(&theme, &mut fonts, &mut swash, 1.0, &dialog.view());
            let path = format!("{out}/{tag}-{name}.png");
            pixmap.save_png(&path).expect("write png");
            println!("{path}  {}x{}", pixmap.width(), pixmap.height());
        }
    }

    // Every theme, one representative state, to catch a palette where
    // the sunken well or the focus ring disappears.
    for theme in wm_theme::default_theme::all_themes() {
        let mut dialog = JoinDialog::new("Cafe Wifi");
        typed(&mut dialog, "hunter2");
        dialog.on_key(Key::ToggleReveal);
        let pixmap = render_join_dialog(&theme, &mut fonts, &mut swash, 1.0, &dialog.view());
        let path = format!("{out}/theme-{}.png", theme.id);
        pixmap.save_png(&path).expect("write png");
        println!("{path}");
    }

    // And one at 2x, since every metric here is scaled by hand.
    let theme = theme_variant("nextstep-classic", Appearance::Dark).expect("theme");
    let mut dialog = JoinDialog::new("Cafe Wifi");
    typed(&mut dialog, "hunter2");
    let pixmap = render_join_dialog(&theme, &mut fonts, &mut swash, 2.0, &dialog.view());
    let path = format!("{out}/dark-10-scale2x.png");
    pixmap.save_png(&path).expect("write png");
    println!("{path}  {}x{}", pixmap.width(), pixmap.height());
}
