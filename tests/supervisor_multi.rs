//! P16: the multi-session supervisor hosts a real session, then pauses and
//! resumes it live over the device-global control socket.
//!
//! Linux-only: isolation relies on `XDG_CONFIG_HOME` (the registry) and
//! `XDG_RUNTIME_DIR` (the control socket) pointing at temp dirs, which is how
//! `config_base()` and the control-socket path resolve on Linux. The logic is
//! OS-independent; only this isolation mechanism is Linux-specific, so the
//! other platforms simply skip the test rather than risk touching a real
//! device registry.
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use common::{WAIT, wait_until};
use tazamun::cli::NetFlags;
use tazamun::registry::{Registry, SessionKind};
use tazamun::supervisor::{self, ControlRequest};

#[tokio::test(flavor = "multi_thread")]
async fn supervisor_hosts_pauses_and_resumes_a_session() {
    // Isolate the device-global registry and control socket into temp dirs so
    // the test never touches the developer's real registry or a live daemon.
    let cfg_home = tempfile::tempdir().expect("cfg tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    // SAFETY: set before any concurrent env access; the runtime's worker
    // threads are idle at this point and nothing else reads these vars.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", cfg_home.path());
        std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
    }

    // A real session in a temp folder, recorded in the (isolated) registry.
    let sdir = tempfile::tempdir().expect("session tempdir");
    tazamun::cli::init(sdir.path()).expect("init session");
    let session_path = std::path::absolute(sdir.path())
        .unwrap()
        .to_string_lossy()
        .to_string();
    let mut reg = Registry::default();
    reg.register(sdir.path(), SessionKind::Init, 1);
    reg.save().expect("save registry");

    // Run the supervisor in the background (airgap: no relay, no external
    // discovery — a self-contained host).
    let net = NetFlags {
        airgap: true,
        ..Default::default()
    };
    let sup = tokio::spawn(async move {
        let _ = supervisor::run(net).await;
    });

    // It brings the session up: the per-folder socket answers.
    assert!(
        wait_until(
            || async { tazamun::ipc::daemon_alive(sdir.path()).await },
            WAIT
        )
        .await,
        "supervisor never hosted the registered session"
    );

    // List reports it as hosted.
    let list = supervisor::request(&ControlRequest::List, Duration::from_secs(5))
        .await
        .expect("control list");
    assert!(list.ok, "list failed: {list:?}");
    let hosted = list
        .data
        .as_ref()
        .and_then(|d| d.get("hosted"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        hosted.iter().any(|p| p.as_str() == Some(&session_path)),
        "hosted set {hosted:?} missing {session_path}"
    );

    // Pause it live: the supervisor gracefully stops the hosted session.
    let resp = supervisor::request(
        &ControlRequest::Pause {
            path: session_path.clone(),
        },
        Duration::from_secs(30),
    )
    .await
    .expect("control pause");
    assert!(resp.ok, "pause failed: {resp:?}");

    // The per-folder socket goes quiet, and the registry records the pause.
    assert!(
        wait_until(
            || async { !tazamun::ipc::daemon_alive(sdir.path()).await },
            WAIT
        )
        .await,
        "paused session is still answering IPC"
    );
    assert!(
        Registry::load().is_paused(sdir.path()),
        "registry did not record the pause"
    );

    // Resume it: the supervisor hosts it again.
    let resp = supervisor::request(
        &ControlRequest::Resume {
            path: session_path.clone(),
        },
        Duration::from_secs(30),
    )
    .await
    .expect("control resume");
    assert!(resp.ok, "resume failed: {resp:?}");
    assert!(
        wait_until(
            || async { tazamun::ipc::daemon_alive(sdir.path()).await },
            WAIT
        )
        .await,
        "resume did not re-host the session"
    );
    assert!(
        !Registry::load().is_paused(sdir.path()),
        "resume did not clear the pause flag"
    );

    // A second supervisor must refuse to start (single control socket).
    let net2 = NetFlags {
        airgap: true,
        ..Default::default()
    };
    let second = supervisor::run(net2).await;
    assert!(
        second.is_err(),
        "a second supervisor should fail: control socket already bound"
    );

    sup.abort();
}
