//! Lifecycle and admission coverage for protocol objects whose retained
//! compositor state participates in per-frame or per-input walks.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const SETTLE: Duration = Duration::from_secs(5);

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn input_method_popups_are_capped_and_removed_on_disconnect() {
    let mut session = Session::boot("ime-popup-ledger", SessionOptions::default()).expect("session boots");
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    session
        .launch(&program, &["ImePopupFlood", "ime-popup-flood", "ime-popup-flood"])
        .expect("IME popup probe launches");
    session.wait_for_window("ImePopupFlood").expect("IME parent window maps");
    poll_until(SETTLE, "the input method to request its popup flood", || {
        session
            .client_log(&program)
            .contains("32 input-method popups requested")
            .then_some(())
    })
    .unwrap();
    poll_until(SETTLE, "the IME ledger to stop at its per-client ceiling", || {
        (session.door().protocol_ledgers().ok()?.ime == 16).then_some(())
    })
    .unwrap();

    session.kill_client(&program);
    poll_until(SETTLE, "client disconnect to empty the IME ledger", || {
        (session.door().protocol_ledgers().ok()?.ime == 0).then_some(())
    })
    .unwrap();
    assert!(session.compositor_alive(), "IME popup cleanup keeps the compositor alive");
}
