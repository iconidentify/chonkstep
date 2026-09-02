//! `omarchy-export-themes [TARGET]` — writes chonkstep's built-in
//! themes as Omarchy themes under `TARGET`, by default
//! `~/.config/omarchy/themes/`, so `omarchy-theme-set` (or Omarchy's
//! own theme menu) can dress the rest of the machine in Amber Phosphor.
//! Runs in a moment, touches only `TARGET`, and refreshes in place:
//! run it again after an update and the palettes follow.
//!
//! The work is `chonk_shell::omarchy_export::export`; this is the
//! doorstep. See `docs/appearance.md`, "Omarchy".

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let target = match (args.next(), args.next()) {
        (None, _) => default_target(),
        (Some(arg), None) if arg != "-h" && arg != "--help" => Some(PathBuf::from(arg)),
        _ => {
            eprintln!(
                "usage: omarchy-export-themes [TARGET]\n\n\
                 Writes chonkstep's built-in themes as Omarchy themes under TARGET\n\
                 (default: ~/.config/omarchy/themes), one directory per theme with a\n\
                 colors.toml and a backgrounds/ folder. Existing exports are refreshed."
            );
            std::process::exit(2);
        }
    };
    let Some(target) = target else {
        eprintln!("omarchy-export-themes: no home directory to find ~/.config/omarchy/themes under; pass TARGET");
        std::process::exit(1);
    };
    match chonk_shell::omarchy_export::export(&target) {
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

/// Where Omarchy looks for user themes: `$XDG_CONFIG_HOME/omarchy/themes`.
fn default_target() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("omarchy").join("themes"))
}
