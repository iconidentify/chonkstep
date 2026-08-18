use wm_theme_api::ButtonKind;

use crate::model::{
    Bevel, BevelStyle, BorderStyle, ButtonStyle, Color, Fill, FontSpec, FontStyle, FontWeight,
    Gradient, GradientDirection, MenuStyle, ResizeBarStyle, TextAlign, Theme, TitlebarStyle,
};

/// The flagship built-in theme. Colors and typography are sampled/
/// derived from a reference NeXTSTEP desktop screenshot at the pixel
/// level (not eyeballed):
/// - A cool lavender-gray desktop (`rgb(128,129,159)`, set by
///   `chonkstep`'s desktop shell) with no separate dock panel — icons
///   sit directly on it.
/// - **Active title bars are black with white bold text** — both the
///   root menu's and an application window's, confirmed by sampling
///   both independently. This is a correction from an earlier, less
///   accurate light-gray guess. Inactive title bars stay a light,
///   receding gray with dark text, per the usual focused/unfocused
///   convention (the reference only shows one, focused, window).
/// - The root menu's items sit on a light gray (`#C0C0C0`) list, with
///   the active/open item highlighted near-white — not an inverted
///   color block.
/// - Font: NeXTSTEP's real system font was Helvetica. This system's own
///   fontconfig resolves "Helvetica" to Nimbus Sans (a metric-compatible
///   Helvetica clone bundled with Ghostscript) rather than Liberation
///   Sans (an Arial clone) — `fc-match Helvetica` confirms this — so
///   Nimbus Sans is the more historically accurate choice here.
pub fn nextstep_classic() -> Theme {
    const FONT_FAMILY: &str = "Nimbus Sans";

    let bevel_raised = Bevel {
        style: BevelStyle::Raised,
        width: 1,
        // Not pure white: a hard-edged 1px line (which grows with
        // `CHONKSTEP_SCALE`, same as everything else) at full 0xFF
        // brightness reads as a harsh, oddly intense stripe rather than
        // a subtle "catching the light" highlight — most noticeable on
        // the buttons' own top/left edges, confirmed live. Softened
        // without losing the raised-bevel effect entirely.
        light: Color::rgb(0xD8, 0xD8, 0xDC),
        dark: Color::rgb(0x30, 0x30, 0x30),
    };

    Theme {
        name: "NeXTSTEP Classic".to_string(),
        titlebar: TitlebarStyle {
            height: 20,
            // Very dark, not flat pure black — keeps the diagonal
            // gradient perceptible up close while reading as "black" at
            // a glance, same as the reference.
            active: Fill::Gradient(Gradient {
                direction: GradientDirection::Diagonal,
                from: Color::rgb(0x28, 0x28, 0x2C),
                to: Color::rgb(0x06, 0x06, 0x08),
            }),
            inactive: Fill::Gradient(Gradient {
                direction: GradientDirection::Diagonal,
                from: Color::rgb(0xB4, 0xB4, 0xBC),
                to: Color::rgb(0x94, 0x94, 0x9E),
            }),
            font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Bold,
                style: FontStyle::Normal,
            },
            text_color_active: Color::rgb(0xFF, 0xFF, 0xFF),
            text_color_inactive: Color::rgb(0x28, 0x28, 0x2C),
            text_align: TextAlign::Center,
            bevel: bevel_raised,
            // Real WindowMaker's default: Miniaturize left-anchored,
            // Close right-anchored — confirmed by reading actual
            // screenshots (windowmaker.org's own "Info" dialog and a
            // themed desktop, both showing miniaturize-left/close-right)
            // rather than assumed. No Maximize: real WindowMaker has no
            // maximize button at all (zoom is menu/keybinding-driven) —
            // `ButtonKind::Maximize` still exists as a WM-core primitive
            // (reachable via Ctrl+Shift+double-click, see `manager.rs`),
            // a theme is just free to not expose it as a titlebar button,
            // same as this one now doesn't.
            //
            // Size and inset are read straight from real WindowMaker's
            // own `TS_NEXT` branch in `wFrameWindowUpdateBorders`
            // (`src/framewin.c`) — the style that actually reproduces
            // NeXTSTEP, as opposed to `TS_NEW`, WindowMaker's own newer
            // default look (which this used to copy instead, by
            // mistake): `bsize = theight - 8`, buttons inset `3`px from
            // the titlebar's edge and vertically centered in the
            // remaining `(theight - bsize) / 2` gap — smaller, inset
            // buttons, not ones stretched flush to the titlebar's full
            // height.
            buttons: vec![
                ButtonStyle { kind: ButtonKind::Miniaturize, size: 12, bevel: bevel_raised },
                ButtonStyle { kind: ButtonKind::Close, size: 12, bevel: bevel_raised },
            ],
            button_margin: 3,
        },
        // Notably thinner than most other WMs' resize borders — an easy
        // detail to get wrong when chasing parity.
        resize_bar: ResizeBarStyle {
            height: 3,
            fill: Fill::Solid(Color::rgb(0xA2, 0xA2, 0xAA)),
            bevel: bevel_raised,
        },
        // Real WindowMaker's own defaults.c: both "FrameBorderColor"
        // (unfocused) and "FrameFocusedBorderColor" default to plain
        // "black" — identical. There's a separate, brighter
        // "FrameSelectedBorderColor" ("white"), but that's for
        // rubber-band multi-window *selection*, a different state
        // entirely, not everyday focus/unfocus. An unfocused window's
        // border sitting adjacent to a focused one used to read as a
        // conspicuous light-gray stripe running the unfocused window's
        // full height — confirmed live, sitting right behind/beside a
        // focused window it looked like a rendering artifact rather
        // than "this other window is just unfocused."
        border: BorderStyle {
            width: 1,
            color_active: Color::rgb(0x08, 0x08, 0x08),
            color_inactive: Color::rgb(0x08, 0x08, 0x08),
        },
        menu: MenuStyle {
            title_font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Bold,
                style: FontStyle::Normal,
            },
            item_font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Normal,
                style: FontStyle::Normal,
            },
            // Same treatment as the window titlebar's active state —
            // black bar, white text — confirmed by directly sampling
            // the reference's root-menu title bar.
            title_bar: Fill::Gradient(Gradient {
                direction: GradientDirection::Diagonal,
                from: Color::rgb(0x28, 0x28, 0x2C),
                to: Color::rgb(0x06, 0x06, 0x08),
            }),
            title_text_color: Color::rgb(0xFF, 0xFF, 0xFF),
            background: Fill::Solid(Color::rgb(0xC0, 0xC0, 0xC0)),
            text_color: Color::rgb(0x10, 0x10, 0x10),
            // The reference highlights the active/open item near-white,
            // not an inverted color block.
            highlight_background: Fill::Solid(Color::rgb(0xF2, 0xF2, 0xF2)),
            highlight_text_color: Color::rgb(0x10, 0x10, 0x10),
            bevel: bevel_raised,
            item_height: 20,
            min_width: 140,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flagship_theme_has_miniaturize_left_and_close_right_only() {
        let theme = nextstep_classic();
        let kinds: Vec<_> = theme.titlebar.buttons.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![ButtonKind::Miniaturize, ButtonKind::Close], "matches real WindowMaker: no maximize button");
    }

    #[test]
    fn flagship_theme_uses_diagonal_gradients() {
        let theme = nextstep_classic();
        match theme.titlebar.active {
            Fill::Gradient(g) => assert_eq!(g.direction, GradientDirection::Diagonal),
            Fill::Solid(_) => panic!("expected a gradient titlebar fill"),
        }
    }
}
