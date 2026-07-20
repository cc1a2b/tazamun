//! Window-level preferences that outlive a run.
//!
//! The window forgets everything on exit: text scale returns to 100%, the
//! Files view returns to name order, the tab bar returns to the first tab.
//! That is merely annoying for sort order and genuinely hostile for text
//! scale — the one setting a reader who needs it must re-apply every single
//! launch. This module owns that small set of values and the one file they
//! live in: `<config-base>/tazamun/gui.json`, a sibling of the session
//! registry's `sessions.json` and written the same atomic way.
//!
//! Preferences are advisory in the strongest sense. Nothing here is on the
//! sync path, nothing here is authoritative, and losing the file loses
//! nothing but convenience — so [`load`] has no error type at all. A missing,
//! truncated, unreadable, or hand-mangled file resolves to [`Prefs::default`]
//! and the window opens regardless. [`save`] answers the same way in reverse:
//! it returns a bool the caller is expected to ignore.
//!
//! The interesting half is [`Prefs::sanitize`], which is the trust boundary.
//! `gui.json` sits in a user-writable directory in plain JSON, so every field
//! that reaches the window is untrusted input, and a float in this file is
//! multiplied into type sizes and window geometry. `sanitize` forces each one
//! back into a range the window can survive; call it after every load, and
//! before every save, so nothing absurd can be read *or* stored. Like
//! [`super::grouping`], the whole of it is pure — no [`eframe::egui`], no I/O
//! — so every hostile value is exercised in unit tests.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::a11y;

// ─── bounds ──────────────────────────────────────────────────────────────────

/// Smallest and largest remembered window size, in logical points.
///
/// The floor mirrors the `with_min_inner_size` the viewport is built with:
/// a smaller stored size is not a size the window would ever adopt, so
/// restoring it just hands the platform something it has to correct. The
/// ceiling is deliberately far past any real display (a 16K wall spanning
/// several monitors still fits) — it exists only to stop a hand-typed `1e9`
/// from becoming a viewport request no compositor can honour.
const MIN_WINDOW: [f32; 2] = [820.0, 520.0];
const MAX_WINDOW: [f32; 2] = [16384.0, 16384.0];

/// Longest accepted [`Prefs::last_tab`]. Tab keys are short identifiers;
/// 32 bytes is room for several times the longest real one and far too little
/// to hide anything in.
const MAX_TAB_LEN: usize = 32;

/// Longest accepted [`Prefs::last_session`]. `PATH_MAX` is 4096 on Linux and
/// the practical ceiling elsewhere, so a longer string cannot name a folder
/// that exists — it can only waste the file and the sidebar it is drawn in.
const MAX_SESSION_LEN: usize = 4096;

/// Tab the window opens on when nothing valid is stored.
const DEFAULT_TAB: &str = "overview";

// ─── the preferences ─────────────────────────────────────────────────────────

/// Window-level preferences that outlive a run.
///
/// `#[serde(default)]` sits on the container, not on the individual fields,
/// and the difference matters: a field-level default fills a missing value
/// with `Default::default()` *of the field's type*, which would silently make
/// an absent `text_scale` `0.0` rather than 100%. The container form fills
/// every missing field from [`Prefs::default`] instead, so a `gui.json`
/// written by an older build — or one where a key was deleted by hand — loads
/// with real defaults rather than zeroes. Unknown keys are ignored (no
/// `deny_unknown_fields`), which is the other half of the same bargain: a file
/// written by a *newer* build still loads here instead of being discarded
/// wholesale for one field this version has never heard of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Text scale, in [`a11y`]'s range. Sanitized through
    /// [`a11y::clamp_scale`] rather than against numbers copied to here.
    pub text_scale: f32,
    /// Files view ordering: `true` is `SortMode::Size`, `false` is
    /// `SortMode::Name`. Stored as a bool because
    /// [`SortMode`](super::grouping::SortMode) is a pure view type with no
    /// serde derives, and a preferences file is a poor reason to give a
    /// sorting enum a wire format it otherwise does not need.
    pub sort_by_size: bool,
    /// Tab to reopen, as a lowercase key.
    ///
    /// Deliberately a `String` and not the window's `Tab` enum. An enum makes
    /// the *whole file* fail to parse the moment it meets a variant it does
    /// not know — a tab added in a later version, or one removed in this one —
    /// which would throw away the text scale alongside it. A string cannot
    /// fail that way: an unrecognised key simply fails to map and the window
    /// opens on its default tab, every other preference intact.
    ///
    /// `sanitize` therefore checks only the *shape* of this value (length and
    /// charset); its *meaning* is the integrator's lookup, which falls back to
    /// the default tab for anything it does not recognise.
    pub last_tab: String,
    /// Absolute path of the session selected when the window last closed.
    pub last_session: Option<String>,
    /// Last inner size in logical points; `None` asks for the built-in size.
    pub window: Option<[f32; 2]>,
    /// Whether the window was maximized when it last closed.
    pub maximized: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            text_scale: a11y::SCALE_DEFAULT,
            sort_by_size: false,
            last_tab: DEFAULT_TAB.to_string(),
            last_session: None,
            window: None,
            maximized: false,
        }
    }
}

impl Prefs {
    /// Forces every field into a sane range. Always call this after loading:
    /// the file is user-editable and may be hand-edited, truncated, or
    /// corrupted, and a hostile or absurd value must never reach the window.
    pub fn sanitize(&mut self) {
        // `clamp_scale` screens non-finite input *before* it reaches
        // `f32::clamp` — which panics on a NaN bound and returns NaN for a NaN
        // input — and is the single owner of the scale range, so the numbers
        // are never copied to here where they could drift apart.
        self.text_scale = a11y::clamp_scale(self.text_scale);

        self.window = self.window.and_then(sane_window);

        if !is_tab_key(&self.last_tab) {
            self.last_tab = String::from(DEFAULT_TAB);
        }

        // `take` then restore: an invalid path is dropped rather than repaired,
        // since half a path is not a path.
        self.last_session = self.last_session.take().filter(|s| is_session_path(s));
    }
}

/// Both dimensions must be real numbers describing a real window. Anything
/// non-finite or non-positive carries no size information at all and is
/// dropped, so the window falls back to its built-in default; a finite,
/// positive but extreme size is real information stated badly, and is clamped
/// into range instead.
fn sane_window(size: [f32; 2]) -> Option<[f32; 2]> {
    let [w, h] = size;
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some([
        w.clamp(MIN_WINDOW[0], MAX_WINDOW[0]),
        h.clamp(MIN_WINDOW[1], MAX_WINDOW[1]),
    ])
}

/// A tab key is a short, non-empty, identifier-shaped ASCII string. The tight
/// charset is doing real work: it rejects control characters, bidi overrides
/// and anything else that would misrender the moment the value were echoed
/// into a painted label or a log line, and it does so without needing to know
/// which tabs exist.
fn is_tab_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_TAB_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A remembered session path must be non-blank, of plausible length, and free
/// of control characters — a NUL cannot occur in a path on any supported
/// platform, and the rest would corrupt the sidebar row this is drawn into.
fn is_session_path(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_SESSION_LEN && !s.chars().any(char::is_control)
}

// ─── persistence ─────────────────────────────────────────────────────────────

/// `<config-base>/tazamun/gui.json`.
pub fn path() -> PathBuf {
    crate::registry::config_base()
        .join("tazamun")
        .join("gui.json")
}

/// Loads and sanitizes. Any failure — missing file, unreadable, malformed
/// JSON, wrong types — yields `Prefs::default()`, never an error: preferences
/// are a convenience and must never block the window from opening.
pub fn load() -> Prefs {
    match std::fs::read_to_string(path()) {
        Ok(text) => parse(&text),
        Err(_) => Prefs::default(),
    }
}

/// Atomically persists. Returns false when it could not be written; the caller
/// treats that as unimportant.
pub fn save(p: &Prefs) -> bool {
    // Sanitized on the way out too, so this process can never be the one that
    // writes a value the next load would have to reject.
    let mut clean = p.clone();
    clean.sanitize();
    save_to(&path(), &clean).is_ok()
}

/// The whole of [`load`] except the read, so the fallback behaviour is
/// testable without a config directory.
fn parse(text: &str) -> Prefs {
    let mut p: Prefs = serde_json::from_str(text).unwrap_or_default();
    p.sanitize();
    p
}

/// Atomic write, the same shape as `AppState::save`: temp file in the *target
/// directory* (so the rename never crosses a filesystem), flushed and synced
/// before it is named, and owner-only on Unix. A crash mid-write leaves the
/// previous `gui.json` untouched rather than a truncated one.
fn save_to(path: &Path, p: &Prefs) -> io::Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "prefs path has no parent"))?;
    std::fs::create_dir_all(dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), p)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    tmp.as_file_mut().write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    set_owner_only(tmp.path())?;
    tmp.persist(path).map_err(|e| e.error)?;

    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// 0600, matching everything else this crate writes. Preferences are not
/// secret; the consistency is the point — nothing tazamun creates is
/// world-readable by default.
fn set_owner_only(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui_native::a11y::{SCALE_DEFAULT, SCALE_MAX, SCALE_MIN};

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Every hostile value this module is expected to survive, in one place,
    /// so the idempotence test cannot drift from the individual ones.
    fn hostile() -> Vec<Prefs> {
        let scales = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -1.0,
            1e30,
            SCALE_MIN,
            SCALE_MAX,
            1.25,
        ];
        let windows = [
            None,
            Some([f32::NAN, 600.0]),
            Some([900.0, f32::INFINITY]),
            Some([0.0, 0.0]),
            Some([-1200.0, -800.0]),
            Some([1e9, 1e9]),
            Some([10.0, 10.0]),
            Some([1180.0, 760.0]),
        ];
        let long_tab = "x".repeat(4096);
        let tabs: [&str; 5] = ["", "files", "\u{7}bell", "tab\nname", &long_tab];
        let sessions = [
            None,
            Some(String::new()),
            Some("   ".to_string()),
            Some("/home/u/p\u{0}roj".to_string()),
            Some("x".repeat(MAX_SESSION_LEN + 1)),
            Some("/home/u/proj".to_string()),
        ];

        let mut out = Vec::new();
        for (i, scale) in scales.iter().enumerate() {
            for (j, window) in windows.iter().enumerate() {
                out.push(Prefs {
                    text_scale: *scale,
                    sort_by_size: i % 2 == 0,
                    last_tab: tabs[(i + j) % tabs.len()].to_string(),
                    last_session: sessions[(i + j) % sessions.len()].clone(),
                    window: *window,
                    maximized: j % 2 == 0,
                });
            }
        }
        out
    }

    fn sanitized(p: &Prefs) -> Prefs {
        let mut c = p.clone();
        c.sanitize();
        c
    }

    // ─── baseline ────────────────────────────────────────────────────────────

    #[test]
    fn default_is_already_sane() {
        let d = Prefs::default();
        assert_eq!(sanitized(&d), d);
        assert!(approx(d.text_scale, SCALE_DEFAULT));
        assert_eq!(d.last_tab, DEFAULT_TAB);
        assert_eq!(d.last_session, None);
        assert_eq!(d.window, None);
        assert!(!d.sort_by_size);
        assert!(!d.maximized);
    }

    #[test]
    fn sanitize_is_idempotent() {
        for p in hostile() {
            let once = sanitized(&p);
            let twice = sanitized(&once);
            assert_eq!(once, twice, "not idempotent for {p:?}");
        }
    }

    #[test]
    fn sanitize_leaves_the_booleans_alone() {
        // Nothing about a bool can be out of range; the invariant is that
        // sanitizing never quietly flips a user's choice.
        for (sort, max) in [(true, true), (true, false), (false, true), (false, false)] {
            let p = Prefs {
                sort_by_size: sort,
                maximized: max,
                text_scale: f32::NAN,
                ..Prefs::default()
            };
            let c = sanitized(&p);
            assert_eq!(c.sort_by_size, sort);
            assert_eq!(c.maximized, max);
        }
    }

    // ─── text_scale ──────────────────────────────────────────────────────────

    #[test]
    fn text_scale_non_finite_falls_back_to_default() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let c = sanitized(&Prefs {
                text_scale: bad,
                ..Prefs::default()
            });
            assert!(
                approx(c.text_scale, SCALE_DEFAULT),
                "{bad} did not fall back"
            );
            assert!(c.text_scale.is_finite());
        }
    }

    #[test]
    fn text_scale_zero_and_negative_clamp_to_min() {
        for bad in [0.0, -0.0, -1.0, -1e30] {
            let c = sanitized(&Prefs {
                text_scale: bad,
                ..Prefs::default()
            });
            assert!(approx(c.text_scale, SCALE_MIN), "{bad} did not clamp up");
        }
    }

    #[test]
    fn text_scale_absurdly_large_clamps_to_max() {
        for bad in [2.0, 1e6, 1e30, f32::MAX] {
            let c = sanitized(&Prefs {
                text_scale: bad,
                ..Prefs::default()
            });
            assert!(approx(c.text_scale, SCALE_MAX), "{bad} did not clamp down");
        }
    }

    #[test]
    fn text_scale_in_range_survives_untouched() {
        for good in [SCALE_MIN, 1.0, 1.25, SCALE_MAX] {
            let c = sanitized(&Prefs {
                text_scale: good,
                ..Prefs::default()
            });
            assert!(approx(c.text_scale, good));
        }
    }

    // ─── window ──────────────────────────────────────────────────────────────

    #[test]
    fn window_none_stays_none() {
        assert_eq!(sanitized(&Prefs::default()).window, None);
    }

    #[test]
    fn window_non_finite_in_either_axis_is_dropped() {
        for bad in [
            [f32::NAN, 600.0],
            [900.0, f32::NAN],
            [f32::INFINITY, 600.0],
            [900.0, f32::NEG_INFINITY],
            [f32::NAN, f32::NAN],
        ] {
            let c = sanitized(&Prefs {
                window: Some(bad),
                ..Prefs::default()
            });
            assert_eq!(c.window, None, "{bad:?} survived");
        }
    }

    #[test]
    fn window_zero_or_negative_is_dropped() {
        for bad in [[0.0, 0.0], [0.0, 600.0], [900.0, 0.0], [-1200.0, -800.0]] {
            let c = sanitized(&Prefs {
                window: Some(bad),
                ..Prefs::default()
            });
            assert_eq!(c.window, None, "{bad:?} survived");
        }
    }

    #[test]
    fn window_below_the_floor_clamps_up() {
        let c = sanitized(&Prefs {
            window: Some([10.0, 4.0]),
            ..Prefs::default()
        });
        assert_eq!(c.window, Some(MIN_WINDOW));
    }

    #[test]
    fn window_larger_than_any_display_clamps_down() {
        for bad in [[1e9, 1e9], [f32::MAX, f32::MAX], [99999.0, 99999.0]] {
            let c = sanitized(&Prefs {
                window: Some(bad),
                ..Prefs::default()
            });
            assert_eq!(c.window, Some(MAX_WINDOW), "{bad:?} was not clamped");
        }
    }

    #[test]
    fn window_in_range_survives_untouched() {
        let size = [1180.0, 760.0];
        let c = sanitized(&Prefs {
            window: Some(size),
            ..Prefs::default()
        });
        assert_eq!(c.window, Some(size));
    }

    // ─── last_tab ────────────────────────────────────────────────────────────

    #[test]
    fn last_tab_empty_falls_back_to_default() {
        let c = sanitized(&Prefs {
            last_tab: String::new(),
            ..Prefs::default()
        });
        assert_eq!(c.last_tab, DEFAULT_TAB);
    }

    #[test]
    fn last_tab_overlong_falls_back_to_default() {
        for len in [MAX_TAB_LEN + 1, 1024, 1_000_000] {
            let c = sanitized(&Prefs {
                last_tab: "a".repeat(len),
                ..Prefs::default()
            });
            assert_eq!(c.last_tab, DEFAULT_TAB, "length {len} survived");
        }
        // Exactly at the bound is still accepted.
        let edge = "a".repeat(MAX_TAB_LEN);
        let c = sanitized(&Prefs {
            last_tab: edge.clone(),
            ..Prefs::default()
        });
        assert_eq!(c.last_tab, edge);
    }

    #[test]
    fn last_tab_with_control_characters_falls_back_to_default() {
        for bad in [
            "files\n",
            "\u{0}files",
            "fi\tles",
            "\u{1b}[31mfiles",
            "files\u{7f}",
            "\u{202e}selif",
        ] {
            let c = sanitized(&Prefs {
                last_tab: bad.to_string(),
                ..Prefs::default()
            });
            assert_eq!(c.last_tab, DEFAULT_TAB, "{bad:?} survived");
        }
    }

    #[test]
    fn last_tab_with_odd_but_harmless_shape_falls_back() {
        // Spaces, dots and slashes are not tab keys either.
        for bad in ["two words", "../files", "files.tab", "tab:1"] {
            let c = sanitized(&Prefs {
                last_tab: bad.to_string(),
                ..Prefs::default()
            });
            assert_eq!(c.last_tab, DEFAULT_TAB, "{bad:?} survived");
        }
    }

    #[test]
    fn last_tab_unknown_but_well_formed_is_kept() {
        // The point of the `String`: a tab this version has never heard of
        // rides through sanitize untouched, and the integrator's lookup is
        // what falls back to the default tab.
        for good in ["files", "conflicts", "some_future_tab", "tab-9"] {
            let c = sanitized(&Prefs {
                last_tab: good.to_string(),
                ..Prefs::default()
            });
            assert_eq!(c.last_tab, good);
        }
    }

    // ─── last_session ────────────────────────────────────────────────────────

    #[test]
    fn last_session_blank_is_dropped() {
        for bad in ["", "   ", "\t\n"] {
            let c = sanitized(&Prefs {
                last_session: Some(bad.to_string()),
                ..Prefs::default()
            });
            assert_eq!(c.last_session, None, "{bad:?} survived");
        }
    }

    #[test]
    fn last_session_overlong_is_dropped() {
        for len in [MAX_SESSION_LEN + 1, 100_000] {
            let c = sanitized(&Prefs {
                last_session: Some("/".to_string() + &"a".repeat(len)),
                ..Prefs::default()
            });
            assert_eq!(c.last_session, None, "length {len} survived");
        }
    }

    #[test]
    fn last_session_with_control_characters_is_dropped() {
        for bad in [
            "/home/u/pr\u{0}oj",
            "/home/u/proj\n",
            "/home\u{1b}[2Ju/proj",
        ] {
            let c = sanitized(&Prefs {
                last_session: Some(bad.to_string()),
                ..Prefs::default()
            });
            assert_eq!(c.last_session, None, "{bad:?} survived");
        }
    }

    #[test]
    fn last_session_ordinary_path_is_kept() {
        for good in [
            "/home/u/proj",
            "C:\\Users\\u\\proj",
            "/home/u/مشروع",
            "/home/u/with space",
        ] {
            let c = sanitized(&Prefs {
                last_session: Some(good.to_string()),
                ..Prefs::default()
            });
            assert_eq!(c.last_session.as_deref(), Some(good));
        }
    }

    // ─── serde ───────────────────────────────────────────────────────────────

    #[test]
    fn empty_object_deserializes_to_default() {
        assert_eq!(parse("{}"), Prefs::default());
    }

    #[test]
    fn json_missing_every_field_keeps_real_defaults_not_zeroes() {
        // The container-level `#[serde(default)]` is what makes this a 1.0 and
        // not a 0.0; a field-level default would zero the scale.
        let p = parse(r#"{"maximized":true}"#);
        assert!(approx(p.text_scale, SCALE_DEFAULT));
        assert_eq!(p.last_tab, DEFAULT_TAB);
        assert!(p.maximized);
    }

    #[test]
    fn partial_json_from_an_older_build_deserializes() {
        // Written before `window`/`maximized`/`last_session` existed.
        let p = parse(r#"{"text_scale":1.25,"sort_by_size":true,"last_tab":"files"}"#);
        assert!(approx(p.text_scale, 1.25));
        assert!(p.sort_by_size);
        assert_eq!(p.last_tab, "files");
        assert_eq!(p.window, None);
        assert_eq!(p.last_session, None);
        assert!(!p.maximized);
    }

    #[test]
    fn unknown_extra_field_is_ignored() {
        // A file from a newer build must not cost this one every other setting.
        let p = parse(r#"{"text_scale":1.15,"theme_variant":"midnight","future":{"a":[1,2]}}"#);
        assert!(approx(p.text_scale, 1.15));
        assert_eq!(p.last_tab, DEFAULT_TAB);
    }

    #[test]
    fn null_optionals_deserialize_as_none() {
        let p = parse(r#"{"last_session":null,"window":null}"#);
        assert_eq!(p.last_session, None);
        assert_eq!(p.window, None);
    }

    #[test]
    fn wrong_types_fall_back_to_the_whole_default() {
        for bad in [
            r#"{"text_scale":"huge"}"#,
            r#"{"window":"1180x760"}"#,
            r#"{"window":[1180.0]}"#,
            r#"{"last_tab":42}"#,
            r#"{"sort_by_size":"yes"}"#,
        ] {
            assert_eq!(parse(bad), Prefs::default(), "{bad} did not fall back");
        }
    }

    #[test]
    fn malformed_json_falls_back_without_panicking() {
        let deep = "{".repeat(64);
        let cases: [&str; 6] = ["", "{ not json", "[]", "null", "\u{0}", &deep];
        for bad in cases {
            assert_eq!(parse(bad), Prefs::default(), "{bad:?} did not fall back");
        }
    }

    #[test]
    fn parse_sanitizes_hostile_values_out_of_the_file() {
        // Serde will happily produce these; sanitize is what stops them.
        let p = parse(r#"{"text_scale":1e30,"window":[1e9,-4.0],"last_tab":"a\nb"}"#);
        assert!(approx(p.text_scale, SCALE_MAX));
        assert_eq!(p.window, None);
        assert_eq!(p.last_tab, DEFAULT_TAB);
    }

    #[test]
    fn valid_values_round_trip_through_json() {
        let p = Prefs {
            text_scale: 1.25,
            sort_by_size: true,
            last_tab: "conflicts".to_string(),
            last_session: Some("/home/u/proj".to_string()),
            window: Some([1400.0, 900.0]),
            maximized: true,
        };
        let text = serde_json::to_string(&p).expect("serializes");
        assert_eq!(parse(&text), p);
    }

    // ─── persistence (temp directories only; never the real config dir) ──────

    #[test]
    fn path_is_a_sibling_of_the_session_registry() {
        let p = path();
        assert_eq!(p.file_name(), Some(std::ffi::OsStr::new("gui.json")));
        assert_eq!(p.parent(), crate::registry::registry_path().parent());
    }

    #[test]
    fn save_to_round_trips_through_a_temp_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gui.json");
        let p = Prefs {
            text_scale: 1.35,
            sort_by_size: true,
            last_tab: "history".to_string(),
            last_session: Some("/tmp/proj".to_string()),
            window: Some([1024.0, 768.0]),
            maximized: false,
        };
        save_to(&file, &p).expect("writes");
        let text = std::fs::read_to_string(&file).expect("reads");
        assert_eq!(parse(&text), p);
    }

    #[test]
    fn save_to_creates_a_missing_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("nested").join("tazamun").join("gui.json");
        save_to(&file, &Prefs::default()).expect("writes");
        assert!(file.is_file());
    }

    #[test]
    fn save_to_replaces_an_existing_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gui.json");
        save_to(&file, &Prefs::default()).expect("first write");
        let second = Prefs {
            last_tab: "peers".to_string(),
            ..Prefs::default()
        };
        save_to(&file, &second).expect("second write");
        assert_eq!(
            parse(&std::fs::read_to_string(&file).expect("reads")),
            second
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries.len(), 1, "temp file left behind: {entries:?}");
    }

    #[test]
    fn save_to_rejects_a_path_with_no_parent() {
        assert!(save_to(Path::new("/"), &Prefs::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gui.json");
        save_to(&file, &Prefs::default()).expect("writes");
        let mode = std::fs::metadata(&file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }
}
