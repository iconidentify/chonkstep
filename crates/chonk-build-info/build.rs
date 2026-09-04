use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CHONKSTEP_GIT_DESCRIBE");

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    track_git_inputs(&workspace);

    let source_id = std::env::var("CHONKSTEP_GIT_DESCRIBE")
        .ok()
        .and_then(one_line)
        .or_else(|| {
            git(&workspace, &["describe", "--tags", "--always", "--dirty"]).and_then(one_line)
        })
        .unwrap_or_else(|| format!("v{}+unknown-source", env!("CARGO_PKG_VERSION")));
    println!("cargo:rustc-env=CHONKSTEP_SOURCE_ID={source_id}");
}

fn one_line(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_owned())
}

// This is a Cargo build script, not the compositor's event/repaint thread;
// source identity must be known before rustc compiles the binary.
#[allow(clippy::disallowed_methods)]
fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Make a checkout build follow branch switches, new commits, staged
/// changes, and tags. Source archives have none of these paths; their
/// identity comes from `CHONKSTEP_GIT_DESCRIBE` instead.
fn track_git_inputs(workspace: &Path) {
    for name in ["HEAD", "index"] {
        if let Some(path) = git_path(workspace, name) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if let Some(common) = git(workspace, &["rev-parse", "--git-common-dir"]).and_then(one_line) {
        let common = absolute(workspace, PathBuf::from(common));
        println!(
            "cargo:rerun-if-changed={}",
            common.join("packed-refs").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            common.join("refs/tags").display()
        );
    }
}

fn git_path(workspace: &Path, name: &str) -> Option<PathBuf> {
    git(workspace, &["rev-parse", "--git-path", name])
        .and_then(one_line)
        .map(PathBuf::from)
        .map(|path| absolute(workspace, path))
}

fn absolute(workspace: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}
