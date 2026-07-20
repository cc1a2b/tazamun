//! Ledger alignment and relative time for the native GUI's number columns.
//!
//! The views render a great many quantities — sizes, counts, durations, ages —
//! and each one arrives as a finished string from the crate's formatters
//! (`human_bytes`, `human_dur`, `fmt_rate`). Left-aligned, a stack of those
//! strings reads as a wall of text: the units sit wherever the digits happen to
//! end. [`split`] recovers the number/unit boundary from an already-formatted
//! string, [`value_width`] measures a column's worth of them, and [`align`]
//! right-pads each one so the units line up down the column and the eye can
//! compare magnitudes without reading.
//!
//! [`ago`] and [`count`] are the other half: one honest relative-time voice and
//! one pluralised count, so "5 minutes ago" and "3 conflicts" are spelled the
//! same way everywhere rather than re-invented per call site.
//!
//! Pure `std` — no egui, no I/O, and no clock: `now` is always passed in, which
//! is what makes every threshold here exhaustively testable. The module *reads*
//! formatted strings and never produces byte or duration scaling of its own, so
//! it cannot drift from the crate's existing conventions.

/// A number split from its unit so a column can align on the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    /// The numeric prefix, e.g. `"12.4"`. Empty when the input had no number.
    pub value: String,
    /// Everything after the number, whitespace-trimmed, e.g. `"MB"` or `"MB/s"`.
    pub unit: String,
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// Splits a formatted quantity such as `"12.4 MB"`, `"900 ms"`, or `"7"` into
/// its numeric part and its unit. A string with no unit yields an empty unit;
/// a string with no numeric part yields an empty value and the whole input as
/// the unit.
///
/// The exact rules, so callers can predict every case:
///
/// - Surrounding whitespace is trimmed first, and the unit is trimmed again
///   after the split. Any Unicode whitespace counts, so a thin space or a
///   non-breaking space between number and unit works like a plain one, and
///   `"1.5KB"` with no space at all splits identically to `"1.5 KB"` — needed
///   because `human_bytes` spaces its units and `human_dur` does not.
/// - Only ASCII digits form a number. Arabic-Indic digits and other Unicode
///   numerals are *not* numbers here: the crate formats with `format!`, which
///   only ever emits ASCII, so treating `"٣"` as a number would be inventing a
///   case that cannot occur while risking a wrong split on real text.
/// - A leading `-` joins the value only as the very first character and only
///   when an ASCII digit follows: `"-3 KB"` splits as `("-3", "KB")`, while the
///   em-dash placeholder `"—"` and a bare `"-"` yield an empty value and keep
///   the whole string as the unit. No `+` sign is recognised — nothing emits one.
/// - `.` and `,` extend the value only when a digit already precedes them *and*
///   a digit follows, which keeps `"12.4"` and `"1,024"` whole while stopping
///   `"3. seconds"` at `"3"` (its unit becomes `". seconds"`, kept verbatim
///   rather than silently discarded).
/// - Internal whitespace inside the unit survives: `"3 files here"` yields the
///   unit `"files here"`. Only the ends are trimmed.
/// - The empty string yields an empty value and an empty unit.
///
/// Operates entirely on `char`s and never slices the input, so no input can
/// produce a byte-boundary panic.
pub fn split(text: &str) -> Figure {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut cut = 0;
    let mut digits = 0usize;

    if chars.first() == Some(&'-') && is_digit(chars.get(1)) {
        cut = 1;
    }
    while let Some(&c) = chars.get(cut) {
        if c.is_ascii_digit() {
            digits += 1;
            cut += 1;
        } else if (c == '.' || c == ',') && digits > 0 && is_digit(chars.get(cut + 1)) {
            cut += 1;
        } else {
            break;
        }
    }
    // A sign with nothing numeric behind it is not a value; give the whole
    // string back as the unit instead.
    if digits == 0 {
        cut = 0;
    }

    Figure {
        value: chars.iter().take(cut).collect(),
        unit: chars
            .iter()
            .skip(cut)
            .collect::<String>()
            .trim()
            .to_string(),
    }
}

/// The width, in characters, that the value column needs so every figure in
/// `figures` aligns on the number/unit boundary.
///
/// Characters, not bytes: a multi-byte digit or separator must not inflate the
/// column. An empty slice needs no column, so the width is 0.
pub fn value_width(figures: &[Figure]) -> usize {
    figures
        .iter()
        .fold(0, |w, f| w.max(f.value.chars().count()))
}

/// Renders one figure padded to `width`, so a column of these aligns.
///
/// The value is padded on the *left* — right-aligning the number is what makes
/// a column line up on its units — and the unit follows after a single space.
/// The unit therefore always begins at character `width + 1`, including for a
/// figure with no value at all (the `"—"` placeholder lands in the unit column
/// rather than at the left margin). A figure with no unit gets no trailing
/// space.
///
/// Padding never truncates: a value wider than `width` is returned in full.
/// Dropping a digit from a size to fit a column would turn a cosmetic concern
/// into a wrong number on screen, so the column widens instead.
pub fn align(figure: &Figure, width: usize) -> String {
    let pad = width.saturating_sub(figure.value.chars().count());
    let mut out = " ".repeat(pad);
    out.push_str(&figure.value);
    if !figure.unit.is_empty() {
        out.push(' ');
        out.push_str(&figure.unit);
    }
    out
}

/// How long ago `then` was, relative to `now`, both in seconds since the Unix
/// epoch. House voice, lowercase: "just now", "5 minutes ago", "3 hours ago",
/// "2 days ago". A `then` in the future reads "just now" rather than a negative
/// duration, since a clock skew is not worth alarming the user about.
///
/// Thresholds, all floored — never rounded up, so the text never claims more
/// elapsed time than actually passed, matching how `human_dur` truncates:
///
/// | elapsed | reads |
/// | --- | --- |
/// | 0..59s | "just now" |
/// | 60s..59m 59s | "N minutes ago" |
/// | 1h..23h 59m 59s | "N hours ago" |
/// | 24h and beyond | "N days ago" |
///
/// So 59s is still "just now", 60s is "1 minute ago", 119s is "1 minute ago",
/// 3599s is "59 minutes ago" and 3600s is "1 hour ago".
///
/// Days are the terminal unit: they do not roll over into weeks, months or
/// years. A two-year-old version reads "730 days ago", which is unambiguous
/// where a vaguer phrase would not be, and there is no calendar arithmetic here
/// to get wrong. The gap is computed with `saturating_sub`, so `now < then` and
/// a `u64::MAX` timestamp are both ordinary inputs rather than overflow.
pub fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    if secs < MINUTE {
        "just now".to_string()
    } else if secs < HOUR {
        format!("{} ago", plural(secs / MINUTE, "minute"))
    } else if secs < DAY {
        format!("{} ago", plural(secs / HOUR, "hour"))
    } else {
        format!("{} ago", plural(secs / DAY, "day"))
    }
}

/// A count with its noun, pluralised: `(1, "file")` is "1 file", `(0, "file")`
/// is "0 files", `(3, "conflict")` is "3 conflicts".
///
/// Pluralisation is deliberately naive — it appends `"s"` for every count other
/// than 1 — because the GUI only ever counts a closed set of regular nouns:
/// `file`, `folder`, `peer`, `version`, `conflict`, `change`, `lease`,
/// `session`, `minute`, `hour`, `day`. Every one of those takes a plain `-s`.
/// Do not pass an irregular noun ("entry", "patch", "person"); add the correct
/// plural at the call site instead. This is not, and should not become, an
/// English pluralisation engine.
pub fn count(n: usize, noun: &str) -> String {
    plural(n as u64, noun)
}

/// The shared naive pluralisation, over `u64` so [`ago`]'s day counts cannot
/// truncate through `usize` on a 32-bit host.
fn plural(n: u64, noun: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{n} {noun}{s}")
}

fn is_digit(c: Option<&char>) -> bool {
    matches!(c, Some(c) if c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fig(value: &str, unit: &str) -> Figure {
        Figure {
            value: value.to_string(),
            unit: unit.to_string(),
        }
    }

    // --- split ---------------------------------------------------------------

    #[test]
    fn split_size_with_space() {
        assert_eq!(split("12.4 MB"), fig("12.4", "MB"));
        assert_eq!(split("1.5 KB"), fig("1.5", "KB"));
        assert_eq!(split("512 B"), fig("512", "B"));
    }

    #[test]
    fn split_handles_spaced_and_unspaced_units() {
        // human_bytes spaces its unit, human_dur does not; both must split.
        assert_eq!(split("900 ms"), fig("900", "ms"));
        assert_eq!(split("900ms"), fig("900", "ms"));
        assert_eq!(split("5s"), fig("5", "s"));
        assert_eq!(split("42m"), fig("42", "m"));
        assert_eq!(split("3h"), fig("3", "h"));
    }

    #[test]
    fn split_bare_number_has_no_unit() {
        assert_eq!(split("7"), fig("7", ""));
        assert_eq!(split("0"), fig("0", ""));
        assert_eq!(split("1024"), fig("1024", ""));
    }

    #[test]
    fn split_empty_string_is_empty_both_sides() {
        assert_eq!(split(""), fig("", ""));
        assert_eq!(split("   "), fig("", ""));
    }

    #[test]
    fn split_unit_only_keeps_whole_input_as_unit() {
        assert_eq!(split("MB"), fig("", "MB"));
        assert_eq!(split("files"), fig("", "files"));
        assert_eq!(split("unknown"), fig("", "unknown"));
    }

    #[test]
    fn split_leading_minus_joins_the_value() {
        assert_eq!(split("-3 KB"), fig("-3", "KB"));
        assert_eq!(split("-12.5%"), fig("-12.5", "%"));
        assert_eq!(split("-7"), fig("-7", ""));
    }

    #[test]
    fn split_sign_without_digits_is_not_a_value() {
        // The em-dash placeholder fmt_rate emits for zero, and a bare hyphen.
        assert_eq!(split("—"), fig("", "—"));
        assert_eq!(split("-"), fig("", "-"));
        assert_eq!(split("-abc"), fig("", "-abc"));
        assert_eq!(split("- 5"), fig("", "- 5"));
    }

    #[test]
    fn split_keeps_thousands_separators_in_the_value() {
        assert_eq!(split("1,024 files"), fig("1,024", "files"));
        assert_eq!(split("1,234,567 B"), fig("1,234,567", "B"));
    }

    #[test]
    fn split_separator_needs_a_digit_on_both_sides() {
        // A trailing separator ends the value and stays visible in the unit.
        assert_eq!(split("3. seconds"), fig("3", ". seconds"));
        assert_eq!(split("12."), fig("12", "."));
        assert_eq!(split("5,"), fig("5", ","));
        assert_eq!(split(".5 MB"), fig("", ".5 MB"));
        assert_eq!(split(",5"), fig("", ",5"));
    }

    #[test]
    fn split_multibyte_unit_is_intact() {
        // Arabic unit text: 2 bytes per char, so a naive byte split would panic
        // or corrupt it.
        assert_eq!(split("12.4 ميغابايت"), fig("12.4", "ميغابايت"));
        assert_eq!(split("3 ملفات"), fig("3", "ملفات"));
        // Micro sign and degree sign are multi-byte too.
        assert_eq!(split("250 µs"), fig("250", "µs"));
        assert_eq!(split("21°C"), fig("21", "°C"));
    }

    #[test]
    fn split_non_ascii_digits_are_not_numbers() {
        // Arabic-Indic digits: not produced by any formatter here, so they are
        // treated as text rather than guessed at.
        assert_eq!(split("١٢٣ ملفات"), fig("", "١٢٣ ملفات"));
    }

    #[test]
    fn split_trims_surrounding_whitespace() {
        assert_eq!(split("  12.4 MB  "), fig("12.4", "MB"));
        assert_eq!(split("\t7\n"), fig("7", ""));
        assert_eq!(split("  MB "), fig("", "MB"));
        // Thin space and non-breaking space separate number from unit too.
        assert_eq!(split("12.4\u{2009}MB"), fig("12.4", "MB"));
        assert_eq!(split("12.4\u{00a0}MB"), fig("12.4", "MB"));
    }

    #[test]
    fn split_preserves_internal_unit_whitespace() {
        assert_eq!(split("3 files here"), fig("3", "files here"));
        assert_eq!(split("2 days ago"), fig("2", "days ago"));
    }

    #[test]
    fn split_handles_house_formatter_output() {
        // Exactly the strings human_bytes / human_dur / fmt_rate produce.
        assert_eq!(split("1.2 MB/s"), fig("1.2", "MB/s"));
        assert_eq!(split("2.0 KB/s"), fig("2.0", "KB/s"));
        assert_eq!(split("512 B/s"), fig("512", "B/s"));
        assert_eq!(split("0 B"), fig("0", "B"));
        assert_eq!(split("5.0 TB"), fig("5.0", "TB"));
    }

    #[test]
    fn split_never_loses_characters() {
        // value + unit must reconstruct the trimmed input up to inner trimming.
        for text in [
            "12.4 MB",
            "900ms",
            "7",
            "",
            "MB",
            "-3 KB",
            "1,024 files",
            "—",
            "12.4 ميغابايت",
            "  padded  ",
            "3. seconds",
        ] {
            let f = split(text);
            let joined: String = f.value.chars().chain(f.unit.chars()).collect();
            let want: String = text.trim().chars().filter(|c| !c.is_whitespace()).collect();
            let got: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
            assert_eq!(got, want, "input {text:?}");
        }
    }

    // --- value_width ---------------------------------------------------------

    #[test]
    fn value_width_of_empty_slice_is_zero() {
        assert_eq!(value_width(&[]), 0);
    }

    #[test]
    fn value_width_of_a_single_figure_is_its_own_width() {
        assert_eq!(value_width(&[fig("12.4", "MB")]), 4);
        assert_eq!(value_width(&[fig("7", "")]), 1);
        assert_eq!(value_width(&[fig("", "—")]), 0);
    }

    #[test]
    fn value_width_takes_the_widest() {
        let column = [
            fig("7", "B"),
            fig("1,024", "B"),
            fig("12.4", "MB"),
            fig("", "—"),
        ];
        assert_eq!(value_width(&column), 5);
    }

    #[test]
    fn value_width_counts_chars_not_bytes() {
        // Six bytes, three chars: the column must not widen to six.
        let arabic = fig("١٢٣", "ملفات");
        assert_eq!(arabic.value.len(), 6);
        assert_eq!(value_width(&[arabic]), 3);
        assert_eq!(value_width(&[fig("−1", "%")]), 2);
    }

    // --- align ---------------------------------------------------------------

    #[test]
    fn align_pads_narrower_values_on_the_left() {
        assert_eq!(align(&fig("7", "B"), 5), "    7 B");
        assert_eq!(align(&fig("12.4", "MB"), 5), " 12.4 MB");
    }

    #[test]
    fn align_at_exact_width_adds_no_padding() {
        assert_eq!(align(&fig("1,024", "files"), 5), "1,024 files");
        assert_eq!(align(&fig("7", ""), 1), "7");
    }

    #[test]
    fn align_never_truncates_a_wider_value() {
        // Dropping a digit to fit would be a wrong number, not a layout nit.
        assert_eq!(align(&fig("1,048,576", "B"), 3), "1,048,576 B");
        assert_eq!(align(&fig("12.4", "MB"), 0), "12.4 MB");
        assert_eq!(align(&fig("-3", "KB"), 1), "-3 KB");
    }

    #[test]
    fn align_omits_the_space_when_there_is_no_unit() {
        assert_eq!(align(&fig("7", ""), 4), "   7");
        assert!(!align(&fig("7", ""), 4).ends_with(' '));
        assert_eq!(align(&fig("", ""), 3), "   ");
    }

    #[test]
    fn align_puts_a_valueless_figure_in_the_unit_column() {
        // The em-dash lands where units land, not at the left margin.
        let width = 5;
        let dash = align(&fig("", "—"), width);
        let size = align(&fig("12.4", "MB"), width);
        assert_eq!(dash.chars().position(|c| c == '—'), Some(width + 1));
        assert_eq!(size.chars().position(|c| c == 'M'), Some(width + 1));
    }

    #[test]
    fn align_counts_chars_not_bytes_when_padding() {
        let out = align(&fig("١٢٣", "ملفات"), 5);
        // Three chars of value => two spaces of padding, regardless of bytes.
        assert_eq!(out.chars().take_while(|&c| c == ' ').count(), 2);
        assert_eq!(out.chars().count(), 2 + 3 + 1 + 5);
    }

    #[test]
    fn align_lines_a_real_column_up_on_its_units() {
        let column: Vec<Figure> = ["512 B", "1.5 KB", "12.4 MB", "1,024 B", "—"]
            .into_iter()
            .map(split)
            .collect();
        let width = value_width(&column);
        assert_eq!(width, 5);

        let rows: Vec<String> = column.iter().map(|f| align(f, width)).collect();
        // Every unit starts at the same character offset — that is the point.
        for (f, row) in column.iter().zip(&rows) {
            let unit_start = row.chars().count() - f.unit.chars().count();
            assert_eq!(unit_start, width + 1, "row {row:?} misaligned");
        }
        assert_eq!(rows[0], "  512 B");
        assert_eq!(rows[1], "  1.5 KB");
        assert_eq!(rows[2], " 12.4 MB");
        assert_eq!(rows[3], "1,024 B");
        assert_eq!(rows[4], "      —");
    }

    // --- ago -----------------------------------------------------------------

    #[test]
    fn ago_zero_gap_is_just_now() {
        assert_eq!(ago(0, 0), "just now");
        assert_eq!(ago(1_700_000_000, 1_700_000_000), "just now");
    }

    #[test]
    fn ago_just_now_boundary() {
        let now = 1_000_000;
        assert_eq!(ago(now - 1, now), "just now");
        assert_eq!(ago(now - 30, now), "just now");
        assert_eq!(ago(now - 59, now), "just now");
        assert_eq!(ago(now - 60, now), "1 minute ago");
    }

    #[test]
    fn ago_minute_boundaries_floor() {
        let now = 1_000_000;
        assert_eq!(ago(now - 60, now), "1 minute ago");
        assert_eq!(ago(now - 61, now), "1 minute ago");
        assert_eq!(ago(now - 119, now), "1 minute ago");
        assert_eq!(ago(now - 120, now), "2 minutes ago");
        assert_eq!(ago(now - 300, now), "5 minutes ago");
        assert_eq!(ago(now - 3599, now), "59 minutes ago");
    }

    #[test]
    fn ago_hour_boundaries_floor() {
        let now = 1_000_000;
        assert_eq!(ago(now - 3600, now), "1 hour ago");
        assert_eq!(ago(now - 3601, now), "1 hour ago");
        assert_eq!(ago(now - 7199, now), "1 hour ago");
        assert_eq!(ago(now - 7200, now), "2 hours ago");
        assert_eq!(ago(now - 3 * 3600, now), "3 hours ago");
        assert_eq!(ago(now - 86399, now), "23 hours ago");
    }

    #[test]
    fn ago_day_boundaries_floor() {
        let now = 10_000_000;
        assert_eq!(ago(now - 86400, now), "1 day ago");
        assert_eq!(ago(now - 86401, now), "1 day ago");
        assert_eq!(ago(now - 2 * 86400, now), "2 days ago");
        assert_eq!(ago(now - 172_799, now), "1 day ago");
        assert_eq!(ago(now - 30 * 86400, now), "30 days ago");
    }

    #[test]
    fn ago_future_reads_just_now() {
        // Clock skew must not print a negative duration or panic.
        assert_eq!(ago(100, 0), "just now");
        assert_eq!(ago(u64::MAX, 0), "just now");
        assert_eq!(ago(1_700_000_060, 1_700_000_000), "just now");
    }

    #[test]
    fn ago_multi_year_gap_does_not_overflow() {
        assert_eq!(ago(0, 730 * 86400), "730 days ago");
        // Two years and a bit, in the shape a real timestamp would take.
        assert_eq!(ago(1_600_000_000, 1_700_000_000), "1157 days ago");
        // The extreme end still renders rather than panicking.
        let far = ago(0, u64::MAX);
        assert!(far.ends_with(" days ago"), "got {far:?}");
    }

    #[test]
    fn ago_singular_forms_have_no_s() {
        let now = 10_000_000;
        assert_eq!(ago(now - 60, now), "1 minute ago");
        assert_eq!(ago(now - 3600, now), "1 hour ago");
        assert_eq!(ago(now - 86400, now), "1 day ago");
        for text in [
            ago(now - 60, now),
            ago(now - 3600, now),
            ago(now - 86400, now),
        ] {
            assert!(!text.contains("s ago"), "{text:?} pluralised a 1");
        }
    }

    #[test]
    fn ago_is_lowercase_and_ends_in_ago_or_just_now() {
        let now = 10_000_000u64;
        for gap in [0u64, 1, 59, 60, 3599, 3600, 86399, 86400, 999_999] {
            let text = ago(now - gap, now);
            assert_eq!(text, text.to_lowercase(), "{text:?} is not lowercase");
            assert!(
                text == "just now" || text.ends_with(" ago"),
                "unexpected phrasing {text:?}"
            );
        }
    }

    // --- count ---------------------------------------------------------------

    #[test]
    fn count_zero_one_many() {
        assert_eq!(count(0, "file"), "0 files");
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(2, "file"), "2 files");
        assert_eq!(count(3, "conflict"), "3 conflicts");
        assert_eq!(count(1_024, "file"), "1024 files");
    }

    #[test]
    fn count_every_documented_noun() {
        for noun in [
            "file", "folder", "peer", "version", "conflict", "change", "lease", "session",
            "minute", "hour", "day",
        ] {
            assert_eq!(count(1, noun), format!("1 {noun}"));
            assert_eq!(count(0, noun), format!("0 {noun}s"));
            assert_eq!(count(7, noun), format!("7 {noun}s"));
        }
    }

    #[test]
    fn count_matches_the_existing_caption_voice() {
        // grouping::group_caption spells these the same way; keep them in step.
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(0, "file"), "0 files");
        assert_eq!(count(3, "file"), "3 files");
    }
}
