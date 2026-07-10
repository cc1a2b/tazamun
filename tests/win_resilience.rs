//! Windows file-op resilience: real sharing violations against the retry
//! wrappers. Windows-only by nature (Unix has no mandatory share modes); the
//! whole file is a no-op elsewhere.

#![cfg(windows)]

use std::os::windows::fs::OpenOptionsExt;
use std::time::Duration;

/// FILE_SHARE_READ only — the holder denies concurrent delete/rename.
const FILE_SHARE_READ: u32 = 0x1;

/// Opens `path` with a share mode that blocks delete/rename, holds it for
/// `hold`, then drops it — the contention every scanner/editor creates.
fn hold_handle_for(path: std::path::PathBuf, hold: Duration) -> std::thread::JoinHandle<()> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .expect("open with restrictive share mode");
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(f);
    })
}

#[test]
fn remove_file_retries_through_a_sharing_violation() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("held.bin");
    std::fs::write(&target, b"held bytes").unwrap();

    // Sanity: with the handle held, a plain remove fails with error 32/5.
    let holder = hold_handle_for(target.clone(), Duration::from_millis(600));
    let plain = std::fs::remove_file(&target);
    assert!(
        plain.is_err(),
        "plain remove should fail while the handle is held"
    );

    // The wrapper retries past the ~600ms hold and succeeds.
    tazamun::win_fs::remove_file(&target).expect("retry wrapper should outlast the holder");
    assert!(!target.exists(), "file gone after retries");
    holder.join().unwrap();
}

#[test]
fn persist_temp_retries_through_a_sharing_violation() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dest.bin");
    std::fs::write(&target, b"old").unwrap();

    let named = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    std::fs::write(named.path(), b"new-staged").unwrap();

    let holder = hold_handle_for(target.clone(), Duration::from_millis(600));
    tazamun::win_fs::persist_temp(named.into_temp_path(), &target)
        .expect("rename-over should retry past the held handle");
    assert_eq!(std::fs::read(&target).unwrap(), b"new-staged");
    holder.join().unwrap();
}

#[test]
fn set_attributes_retries_through_contention() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("attr.bin");
    std::fs::write(&target, b"x").unwrap();

    // Attribute changes contend with open handles less often, but the wrapper
    // must at minimum stay correct under a concurrent holder.
    let holder = hold_handle_for(target.clone(), Duration::from_millis(300));
    tazamun::guard::set_readonly(&target).expect("set read-only under contention");
    assert!(std::fs::metadata(&target).unwrap().permissions().readonly());
    tazamun::guard::set_writable(&target).expect("clear read-only under contention");
    holder.join().unwrap();
}

#[test]
fn readonly_file_delete_follows_clear_then_delete_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("ro.bin");
    std::fs::write(&target, b"x").unwrap();
    tazamun::guard::set_readonly(&target).unwrap();

    // Windows refuses to delete a read-only file; the documented ordering is
    // clear-attribute → delete-with-retry, which the sites implement via
    // set_writable + win_fs::remove_file.
    assert!(
        std::fs::remove_file(&target).is_err(),
        "plain delete of a read-only file must fail on Windows"
    );
    tazamun::guard::set_writable(&target).unwrap();
    tazamun::win_fs::remove_file(&target).unwrap();
    assert!(!target.exists());
}
