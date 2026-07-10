//! Build script: embeds a Windows application manifest declaring
//! `longPathAware`, so paths past MAX_PATH work whenever the OS-side
//! `LongPathsEnabled` registry switch is on. Non-Windows targets are untouched
//! (the check is on the *target*, not the build host). The manifest is
//! belt-and-braces on top of `src/win_fs.rs`, which converts absolute paths to
//! `\\?\` extended-length form at the filesystem boundary and works even when
//! the registry switch is off.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        use embed_manifest::{embed_manifest, manifest::Setting, new_manifest};
        embed_manifest(new_manifest("Tazamun.Tazamun").long_path_aware(Setting::Enabled))
            .expect("embedding the Windows application manifest");
    }
}
