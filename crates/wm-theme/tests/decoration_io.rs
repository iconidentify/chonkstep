//! Prove live engine reconstruction uses the resident font state. The child
//! process is killed by the kernel if the measured phase touches files, reads
//! descriptors or starts another process; the negative control proves the gate.
#![cfg(target_os = "linux")]

use std::os::unix::process::ExitStatusExt;
use wm_theme::{FontState, RasterThemeEngine};
use wm_theme_api::{DecorationRequest, Size, ThemeEngine};

#[test]
fn resident_engine_rebuild_does_no_file_or_process_io() {
    if let Ok(mode) = std::env::var("CHONK_DECORATION_IO_CHILD") {
        let fonts = FontState::new();
        let theme = wm_theme::default_theme::nextstep_classic();
        let request = DecorationRequest {
            content_size: Size::new(800, 600), title: "Resident title".into(),
            focused: true, resizable: true, buttons: Vec::new(),
        };
        // Font discovery/loading belongs to initial session setup. Glyphs
        // used here are resident before the simulated live-switch boundary.
        let first = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone());
        for scale in [1.0, 1.5, 2.0] {
            let layout = first.layout_at(&request, scale);
            std::hint::black_box(first.render_surface_at(&request, &layout, scale));
        }
        deny_io();
        if mode == "negative" {
            // This must terminate with SIGSYS, even though /dev/null exists.
            let _ = std::fs::read("/dev/null");
            std::process::exit(99);
        }
        for &style in wm_theme::SUPPORTED_DECORATION_STYLES {
            for scale in [1.0, 1.5, 2.0] {
                let engine = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone()).with_style(style).unwrap();
                let layout = engine.layout_at(&request, scale);
                let rendered = engine.render_surface_at(&request, &layout, scale);
                assert!(rendered.retained_bytes() > 0);
                if style == wm_theme::DecorationStyle::System7 {
                    // These glyphs were never rendered before installing the
                    // filter. Even a cold Unicode title must use resident data.
                    let unicode = DecorationRequest { title: "Terminal — Живет 中文 日本語 العربية 🦀 \u{10ffff}".into(), ..request.clone() };
                    let layout = engine.layout_at(&unicode, scale);
                    assert!(engine.render_surface_at(&unicode, &layout, scale).retained_bytes() > 0);
                    let chrome = wm_theme::UiChrome::new(&theme, fonts.clone(), style, scale);
                    let items = [wm_theme::menu::MenuItem::Action { label: "Κόσμος עברית · 端末".into(), action: 7 }];
                    let menu = chrome.menu(&theme, &mut fonts.system(), "Applications", &items, Some(0), true);
                    assert!(!menu.buffer.pixels.is_empty());
                    let caption = chrome.label(&theme, &mut fonts.system(), &mut fonts.swash(),
                        "E\u{301}ditor 👩\u{200d}💻", 180, 28, true);
                    assert!(!caption.pixels.is_empty());
                }
            }
        }
        std::process::exit(0);
    }
    for (mode, signal) in [("negative", Some(libc::SIGSYS)), ("resident", None)] {
        // Runs on the integration test harness thread; this binary has no
        // compositor/event loop, and must collect the disposable child's result.
        #[allow(clippy::disallowed_methods)]
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "resident_engine_rebuild_does_no_file_or_process_io", "--nocapture"])
            // libtest runs this body on a helper thread. Glibc's secondary
            // arena can lazily open /proc/sys/vm/overcommit_memory when first
            // shrinking, unrelated to font I/O (reproduced in Arch CI).
            // Give this disposable process one arena, as a main-thread-only
            // compositor has. Keep every syscall denial and negative control.
            .env("GLIBC_TUNABLES", "glibc.malloc.arena_max=1")
            .env("CHONK_DECORATION_IO_CHILD", mode)
            .output().unwrap();
        assert_eq!(output.status.signal(), signal,
            "{mode}: {:?}\n{}", output.status, String::from_utf8_lossy(&output.stderr));
        if signal.is_none() {
            assert!(output.status.success(), "{mode}: {:?}", output.status);
        }
    }
}

fn deny_io() {
    // Test-only syscall filter, installed after all setup in a disposable
    // child. This is a correctness tripwire, not a production security sandbox.
    let mut filter = vec![libc::sock_filter {
        code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        jt: 0, jf: 0, k: 0, // seccomp_data.nr
    }];
    for syscall in [
        libc::SYS_read, libc::SYS_readv, libc::SYS_pread64, libc::SYS_preadv,
        libc::SYS_openat, libc::SYS_openat2, libc::SYS_getdents64,
        libc::SYS_newfstatat, libc::SYS_statx, libc::SYS_execve, libc::SYS_execveat,
        libc::SYS_clone, libc::SYS_clone3,
    ] {
        filter.push(libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0, jf: 1, k: syscall as u32,
        });
        filter.push(libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0, jf: 0, k: libc::SECCOMP_RET_TRAP,
        });
    }
    filter.push(libc::sock_filter {
        code: (libc::BPF_RET | libc::BPF_K) as u16,
        jt: 0, jf: 0, k: libc::SECCOMP_RET_ALLOW,
    });
    let program = libc::sock_fprog { len: filter.len() as u16, filter: filter.as_mut_ptr() };
    // SAFETY: prctl receives only constant scalar flags in the first call.
    // The second call points to a live sock_fprog and its live instruction
    // vector for the duration of the call; the kernel copies both before return.
    // This affects only the disposable child test thread.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0), 0);
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program, 0, 0), 0);
    }
}
