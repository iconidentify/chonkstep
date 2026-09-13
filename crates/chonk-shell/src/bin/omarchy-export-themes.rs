//! `omarchy-export-themes [TARGET]` — writes chonkstep's built-in
//! themes as Omarchy themes under `TARGET`, by default
//! `~/.config/omarchy/themes/`, so `omarchy-theme-set` (or Omarchy's
//! own theme menu) can dress the rest of the machine in Amber Phosphor.
//! Runs in a moment, touches only `TARGET`, and refreshes in place:
//! run it again after an update and the palettes follow.
//! Sample frames resolve each theme's default recipe through Auto;
//! the exporter does not read the running desktop's `decoration_style` setting.
//!
//! The work is `chonk_shell::omarchy_export::export`; this is the
//! doorstep. See `docs/appearance.md`, "Omarchy".

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1).peekable();
    let missing_only = args.peek().is_some_and(|arg| arg == "--missing");
    if missing_only { args.next(); }
    let target = match (args.next(), args.next()) {
        (None, _) => chonk_shell::omarchy_export::default_target(),
        (Some(arg), None) if arg != "-h" && arg != "--help" => Some(PathBuf::from(arg)),
        _ => {
            eprintln!(
                "usage: omarchy-export-themes [--missing] [TARGET]\n\n\
                 Writes chonkstep's built-in themes as Omarchy themes under TARGET\n\
                 (default: ~/.config/omarchy/themes), one directory per theme with a\n\
                 colors.toml, backgrounds/ and preview.png; modern and System 7 themes also provide\n\
                 shell.toml surface colors and a public Theme in chonkstep.toml.\n\
                 Existing exports are refreshed unless --missing is given.\n\
                 --missing adds only themes without an existing directory."
            );
            std::process::exit(2);
        }
    };
    let Some(target) = target else {
        eprintln!("omarchy-export-themes: no home directory to find ~/.config/omarchy/themes under; pass TARGET");
        std::process::exit(1);
    };
    let result = if missing_only {
        chonk_shell::omarchy_export::install_missing(&target)
    } else {
        chonk_shell::omarchy_export::export(&target)
    };
    match result {
        Ok(written) => {
            for dir in &written {
                println!("{}", dir.display());
            }
            println!("{} themes written under {}", written.len(), target.display());
        }
        Err(e) => {
            eprintln!("omarchy-export-themes: could not write under {}: {e}", target.display());
            std::process::exit(1);
        }
    }
}
