//! Build script. Two jobs:
//!  1. Export `TAZAMUN_VERSION` (crate version + short git hash) for `--version`.
//!  2. On a Windows *target*: embed the app icon, VERSIONINFO (the Properties →
//!     Details identity fields), and the `longPathAware` manifest via one
//!     resource compiler (`winresource`). The manifest lets paths past MAX_PATH
//!     work when the OS `LongPathsEnabled` switch is on; it is belt-and-braces
//!     on top of `src/win_fs.rs` (`\\?\` extended paths). Non-Windows targets
//!     are untouched (the check is on the target, not the build host).

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
    // `.git/HEAD` alone is not enough: on a branch it holds `ref: refs/heads/x`
    // and a commit rewrites the *branch* file, leaving HEAD untouched — so
    // watching only HEAD left every build after the first reporting a stale
    // commit. Watch the ref HEAD points at as well, plus packed-refs for when
    // the loose ref file has been packed away.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD")
        && let Some(git_ref) = head.strip_prefix("ref: ").map(str::trim)
        && !git_ref.is_empty()
    {
        println!("cargo:rerun-if-changed=.git/{git_ref}");
    }

    // Windows only: embed the app icon, the VERSIONINFO (file Properties →
    // Details tab), and the longPathAware manifest — all through ONE resource
    // compiler (winresource) so there is no duplicate-manifest link error.
    // cfg-gated on the *target*, so Linux/macOS builds are untouched: the crate
    // is a build-dependency only, and `.compile()` is never reached for them.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        const LONG_PATH_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings xmlns:ws2="http://schemas.microsoft.com/SMI/2016/WindowsSettings">
      <ws2:longPathAware>true</ws2:longPathAware>
    </windowsSettings>
  </application>
</assembly>"#;
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/branding/tazamun.ico");
        res.set_manifest(LONG_PATH_MANIFEST);
        res.set("ProductName", "tazamun");
        res.set(
            "FileDescription",
            "tazamun — strict-checkout P2P folder sync",
        );
        res.set("CompanyName", "@CC1A2B");
        res.set("LegalCopyright", "\u{00A9} 2026 @CC1A2B");
        res.set("OriginalFilename", "tazamun.exe");
        res.set("FileVersion", full.as_str());
        res.set("ProductVersion", full.as_str());
        res.compile()
            .expect("embedding Windows resources (icon + versioninfo + manifest)");
        println!("cargo:rerun-if-changed=assets/branding/tazamun.ico");
    }
}
