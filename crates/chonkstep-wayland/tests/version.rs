#[cfg(target_os = "linux")]
// This is a test process waiting for the CLI it deliberately spawned,
// not the compositor's event/repaint thread.
#[allow(clippy::disallowed_methods)]
fn assert_version(flag: &str) {
    let binary = env!("CARGO_BIN_EXE_chonkstep-wayland");
    let output = std::process::Command::new(binary)
        .arg(flag)
        .output()
        .expect("run chonkstep-wayland");
    assert!(output.status.success(), "{flag}: {:?}", output.status);
    assert!(
        output.stderr.is_empty(),
        "{flag}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let build_id = chonk_build_info::elf_build_id(binary).unwrap();
    let expected = format!(
        "chonkstep-wayland {}\nsource: {}\nbuild id: {build_id}\n",
        env!("CARGO_PKG_VERSION"),
        chonk_build_info::SOURCE_ID,
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        expected,
        "{flag}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn long_version_names_the_source_and_executable_build_id() {
    assert_version("--version");
}

#[cfg(target_os = "linux")]
#[test]
fn short_version_is_the_same_report() {
    assert_version("-V");
}
