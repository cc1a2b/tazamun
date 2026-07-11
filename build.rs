//! Build script: embeds a Windows application manifest declaring
//! `longPathAware`, so paths past MAX_PATH work whenever the OS-side
//! `LongPathsEnabled` registry switch is on. Non-Windows targets are untouched
//! (the check is on the *target*, not the build host). The manifest is
//! belt-and-braces on top of `src/win_fs.rs`, which converts absolute paths to
//! `\\?\` extended-length form at the filesystem boundary and works even when
//! the registry switch is off.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Best-effort short build id for `--version`: the git short hash when this
    // is built from a checkout, otherwise just the crate version. Never fails
    // the build (a release tarball has no .git).
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let build_id = std::process::Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let full = match build_id {
        Some(id) => format!("{version} ({id})"),
        None => version,
    };
    println!("cargo:rustc-env=TAZAMUN_VERSION={full}");
    println!("cargo:rerun-if-changed=.git/HEAD");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use embed_manifest::{embed_manifest, manifest::Setting, new_manifest};
        embed_manifest(new_manifest("Tazamun.Tazamun").long_path_aware(Setting::Enabled))
            .expect("embedding the Windows application manifest");
    }
}
