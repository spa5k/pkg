//! Spike S4 (PR-6 / DR-004) — TIMEPARSE slice: strict extraction of maximum-RSS
//! (KiB) from the captured stderr of macOS `/usr/bin/time -l` and GNU `time -v`.
//!
//! The runner spawns the child under `/usr/bin/time` (macOS/BSD `-l`, or GNU
//! `time -v` on Linux) and captures the combined stderr. Both tools print a
//! single maximum-resident-set-size metric line among their diagnostics:
//!
//!   * macOS `-l` emits `<BYTES> maximum resident set size` where the integer is
//!     in BYTES; this module converts it CEILING to KiB (1 byte rounds up to 1
//!     KiB, 1024 bytes == 1 KiB, 1025 bytes == 2 KiB).
//!   * GNU `time -v` emits `Maximum resident set size (kbytes): <KIB>` where the
//!     integer is already in KiB; it is taken as-is.
//!
//! Parsing is STRICT and format-agnostic about which tool produced the capture:
//! the whole stderr is scanned for a metric line of EITHER shape, and exactly one
//! must be present. [`parse_max_rss`] returns the parsed KiB value plus the stderr
//! with precisely that one metric line (and its terminator) removed, so the report
//! layer can show the child's own diagnostics without the noisy metric.
//!
//! Rejected, each as a distinct stable error:
//!   * [`ParseError::Missing`]   — no metric line at all;
//!   * [`ParseError::Duplicate`] — two or more metric lines;
//!   * [`ParseError::Negative`]  — a metric line whose number slot is `-<digits>`;
//!   * [`ParseError::Overflow`]  — a metric line whose digit run exceeds `u64`;
//!   * [`ParseError::Malformed`] — a metric line whose number slot is empty, a
//!     decimal, has a sign other than a single leading `-`, or non-digit junk.
//!
//! A bad number on any metric-shaped line takes precedence over missing/duplicate
//! (the first one in line order wins). Leading/trailing ASCII whitespace
//! (space/tab/CR) on a line is tolerated because real `time` output is indented;
//! any other extra content on the metric line is not.

use std::fmt;

/// Parsed maximum-RSS and the child stderr with the metric line excised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    /// Maximum resident set size, normalized to KiB.
    pub max_rss_kib: u64,
    /// The captured stderr with exactly the one metric line (and its line
    /// terminator) removed; all other child output is preserved verbatim,
    /// including blank lines and ordering.
    pub stderr: String,
}

/// Error returned by [`parse_max_rss`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// No metric line was found in the captured stderr.
    Missing,
    /// Two or more metric lines were found.
    Duplicate,
    /// A metric line carried a negative number (`-<digits>`).
    Negative,
    /// A metric line carried a digit run too large for `u64`.
    Overflow,
    /// A metric line's number slot was empty, a decimal, or otherwise junky.
    Malformed,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Missing => f.write_str("timeparse: no maximum resident set size line"),
            ParseError::Duplicate => {
                f.write_str("timeparse: multiple maximum resident set size lines")
            }
            ParseError::Negative => f.write_str("timeparse: negative maximum resident set size"),
            ParseError::Overflow => {
                f.write_str("timeparse: maximum resident set size overflows u64")
            }
            ParseError::Malformed => {
                f.write_str("timeparse: malformed maximum resident set size line")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// --- internal: per-line classification --------------------------------------

/// Which tool's metric line shape matched (drives the KiB conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// macOS `-l`: value is in BYTES, converted ceiling to KiB.
    Macos,
    /// GNU `time -v`: value is already in KiB.
    Gnu,
}

/// Result of testing one captured line against the metric shape.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LineMatch {
    /// Not a metric line; left untouched in the cleaned stderr.
    None,
    /// A metric-shaped line: either a parsed KiB value or a bad-number cause.
    Metric(MetricMatch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MetricMatch {
    /// A clean metric line; the value already normalized to KiB.
    Value { kib: u64 },
    /// A metric line whose number slot was recognized but invalid.
    Bad(BadNumber),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadNumber {
    Negative,
    Overflow,
    Malformed,
}

impl From<BadNumber> for ParseError {
    fn from(b: BadNumber) -> Self {
        match b {
            BadNumber::Negative => ParseError::Negative,
            BadNumber::Overflow => ParseError::Overflow,
            BadNumber::Malformed => ParseError::Malformed,
        }
    }
}

/// ASCII horizontal whitespace (the only separators `time` emits inside a line).
fn is_hws_byte(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Trim ASCII space/tab/CR (NOT LF; lines are already split on LF) from both ends.
fn trim_ws(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\r'))
}

/// Convert a byte count to KiB with ceiling division, overflow-safe.
///
/// `(bytes + 1023) / 1024` would overflow near `u64::MAX`; instead the quotient
/// and remainder are computed separately and rounded up only on a nonzero
/// remainder. The quotient is at most `u64::MAX / 1024`, so `q + 1` cannot
/// overflow.
fn bytes_to_kib_ceiling(bytes: u64) -> u64 {
    let q = bytes / 1024;
    let r = bytes % 1024;
    if r != 0 { q + 1 } else { q }
}

/// Classify the number slot token of a metric line into a value (in KiB) or a
/// specific bad-number cause. The token is assumed whitespace-free; an empty
/// token is [`BadNumber::Malformed`].
fn classify_token(token: &str, format: Format) -> MetricMatch {
    if token.is_empty() {
        return MetricMatch::Bad(BadNumber::Malformed);
    }
    if let Some(rest) = token.strip_prefix('-') {
        // A leading '-' on an otherwise all-digit slot is an explicit negative.
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return MetricMatch::Bad(BadNumber::Negative);
        }
        return MetricMatch::Bad(BadNumber::Malformed);
    }
    if !token.bytes().all(|b| b.is_ascii_digit()) {
        // Covers '+5', '1.5', '0x10', 'abc', internal junk, etc.
        return MetricMatch::Bad(BadNumber::Malformed);
    }
    match token.parse::<u64>() {
        Ok(raw) => {
            let kib = match format {
                Format::Macos => bytes_to_kib_ceiling(raw),
                Format::Gnu => raw,
            };
            MetricMatch::Value { kib }
        }
        // Pure all-digit, non-empty: the only possible parse failure is overflow.
        Err(_) => MetricMatch::Bad(BadNumber::Overflow),
    }
}

/// Try to match a line as the macOS `-l` form `<BYTES> maximum resident set size`.
fn classify_macos(line: &str) -> LineMatch {
    const LABEL: &str = "maximum resident set size";
    let t = trim_ws(line);
    let prefix = match t.strip_suffix(LABEL) {
        Some(p) => p,
        None => return LineMatch::None,
    };
    // `prefix` must be `<token><ws+>`: strip the trailing whitespace separator.
    let mut sep = prefix.len();
    while sep > 0 && is_hws_byte(prefix.as_bytes()[sep - 1]) {
        sep -= 1;
    }
    if sep == prefix.len() {
        // No whitespace separator before the label. Label-only => malformed;
        // anything glued to the label (`xLABEL`) => not a metric line.
        if prefix.is_empty() {
            return LineMatch::Metric(MetricMatch::Bad(BadNumber::Malformed));
        }
        return LineMatch::None;
    }
    let candidate = &prefix[..sep];
    // After trim_ws the line has no leading whitespace, so a non-empty prefix
    // starts with a non-ws byte and the candidate is non-empty. Guard anyway.
    if candidate.is_empty() {
        return LineMatch::Metric(MetricMatch::Bad(BadNumber::Malformed));
    }
    // Internal whitespace => extra tokens before the number => not a clean line.
    if candidate.contains([' ', '\t']) {
        return LineMatch::None;
    }
    LineMatch::Metric(classify_token(candidate, Format::Macos))
}

/// Try to match a line as the GNU `time -v` form
/// `Maximum resident set size (kbytes): <KIB>`.
fn classify_gnu(line: &str) -> LineMatch {
    const LABEL: &str = "Maximum resident set size (kbytes):";
    let t = trim_ws(line);
    let suffix = match t.strip_prefix(LABEL) {
        Some(s) => s,
        None => return LineMatch::None,
    };
    // `suffix` must be `<ws+><token>`: strip the leading whitespace separator.
    let mut start = 0;
    while start < suffix.len() && is_hws_byte(suffix.as_bytes()[start]) {
        start += 1;
    }
    if start == 0 {
        // No whitespace after the colon. Label-only => malformed; glued => skip.
        if suffix.is_empty() {
            return LineMatch::Metric(MetricMatch::Bad(BadNumber::Malformed));
        }
        return LineMatch::None;
    }
    let candidate = &suffix[start..];
    if candidate.is_empty() {
        return LineMatch::Metric(MetricMatch::Bad(BadNumber::Malformed));
    }
    if candidate.contains([' ', '\t']) {
        return LineMatch::None;
    }
    LineMatch::Metric(classify_token(candidate, Format::Gnu))
}

fn classify_line(line: &str) -> LineMatch {
    match classify_macos(line) {
        LineMatch::None => classify_gnu(line),
        other => other,
    }
}

/// Strict parse of maximum-RSS from captured child stderr.
///
/// Scans every line for the macOS `-l` or GNU `time -v` metric line; exactly one
/// must be present (see module docs for the rejection rules). On success returns
/// the KiB-normalized value and the stderr with that single metric line removed.
pub fn parse_max_rss(stderr: &str) -> Result<ParseOutput, ParseError> {
    // Collect metric-shaped lines with their byte ranges. The range end includes
    // the trailing LF so removal also drops the line terminator (and a trailing
    // CR in CRLF captures, which sits just before the LF).
    let mut found: Vec<(usize, usize, MetricMatch)> = Vec::new();
    let bytes = stderr.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        let rel = bytes[start..].iter().position(|&b| b == b'\n');
        let (text_end, range_end) = match rel {
            Some(r) => (start + r, start + r + 1),
            None => (bytes.len(), bytes.len()),
        };
        let text = &stderr[start..text_end];
        if let LineMatch::Metric(m) = classify_line(text) {
            found.push((start, range_end, m));
        }
        start = range_end;
    }

    // A bad number on any metric-shaped line takes precedence over
    // missing/duplicate; the first one in line order wins.
    for (_, _, m) in &found {
        if let MetricMatch::Bad(b) = m {
            return Err(ParseError::from(*b));
        }
    }

    match found.len() {
        0 => Err(ParseError::Missing),
        1 => {
            let (start, end, m) = &found[0];
            let MetricMatch::Value { kib } = m else {
                unreachable!("bad-number matches are returned earlier");
            };
            let kib = *kib;
            let mut cleaned = String::with_capacity(stderr.len() - (end - start));
            cleaned.push_str(&stderr[..*start]);
            cleaned.push_str(&stderr[*end..]);
            Ok(ParseOutput {
                max_rss_kib: kib,
                stderr: cleaned,
            })
        }
        _ => Err(ParseError::Duplicate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- macOS /usr/bin/time -l: happy path + byte->KiB ceiling ----------------

    #[test]
    fn macos_valid_converts_bytes_ceiling_to_kib_and_strips_line() {
        let stderr = "header\n        2048  maximum resident set size\ntrailer\n";
        let out = parse_max_rss(stderr).unwrap();
        assert_eq!(out.max_rss_kib, 2);
        assert_eq!(out.stderr, "header\ntrailer\n");
    }

    #[test]
    fn macos_byte_to_kib_ceiling_boundaries() {
        for (bytes, expected_kib) in [
            ("0", 0u64),
            ("1", 1),
            ("1023", 1),
            ("1024", 1),
            ("1025", 2),
            ("2047", 2),
            ("2048", 2),
            ("2049", 3),
            (u64::MAX.to_string().as_str(), u64::MAX / 1024 + 1),
        ] {
            let stderr = format!("{bytes} maximum resident set size\n");
            let out = parse_max_rss(&stderr).unwrap_or_else(|e| panic!("{bytes}: {e}"));
            assert_eq!(out.max_rss_kib, expected_kib, "bytes={bytes}");
            assert_eq!(out.stderr, "", "bytes={bytes}");
        }
    }

    #[test]
    fn macos_realistic_dump_extracts_and_preserves_other_lines() {
        let stderr = "\
0.00 real         0.00 user         0.00 sys\n\
              0  involuntary context switches\n\
        6389760  maximum resident set size\n\
           1024  average memory resident set size\n\
              1  voluntary context switches\n";
        let out = parse_max_rss(stderr).unwrap();
        // 6389760 / 1024 = 6240 exactly => 6240 KiB.
        assert_eq!(out.max_rss_kib, 6240);
        assert_eq!(
            out.stderr,
            "0.00 real         0.00 user         0.00 sys\n\
             0  involuntary context switches\n\
             1024  average memory resident set size\n\
             1  voluntary context switches\n"
        );
    }

    // --- GNU time -v: happy path ----------------------------------------------

    #[test]
    fn gnu_valid_keeps_kib_as_is_and_strips_line() {
        let stderr = "\tCommand being timed: \"./prog\"\n\tMaximum resident set size (kbytes): 6384\n\tElapsed (wall clock) time: 0:00.42\n";
        let out = parse_max_rss(stderr).unwrap();
        assert_eq!(out.max_rss_kib, 6384);
        assert_eq!(
            out.stderr,
            "\tCommand being timed: \"./prog\"\n\tElapsed (wall clock) time: 0:00.42\n"
        );
    }

    #[test]
    fn gnu_realistic_dump_extracts_and_preserves_other_lines() {
        let stderr = "\
\tCommand being timed: \"true\"\n\
\tUser time (seconds): 0.00\n\
\tMaximum resident set size (kbytes): 6384\n\
\tAverage resident set size (kbytes): 0\n";
        let out = parse_max_rss(stderr).unwrap();
        assert_eq!(out.max_rss_kib, 6384);
        assert_eq!(
            out.stderr,
            "\tCommand being timed: \"true\"\n\
             \tUser time (seconds): 0.00\n\
             \tAverage resident set size (kbytes): 0\n"
        );
    }

    // --- placement / formatting robustness ------------------------------------

    #[test]
    fn preserves_blank_lines_and_order() {
        let stderr = "\nlead\n\n2048 maximum resident set size\n\ntrail\n\n";
        let out = parse_max_rss(stderr).unwrap();
        assert_eq!(out.max_rss_kib, 2);
        assert_eq!(out.stderr, "\nlead\n\n\ntrail\n\n");
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let stderr = "header\r\n2048 maximum resident set size\r\ntrailer\r\n";
        let out = parse_max_rss(stderr).unwrap();
        assert_eq!(out.max_rss_kib, 2);
        assert_eq!(out.stderr, "header\r\ntrailer\r\n");
    }

    #[test]
    fn metric_line_without_trailing_newline() {
        let out = parse_max_rss("2048 maximum resident set size").unwrap();
        assert_eq!(out.max_rss_kib, 2);
        assert_eq!(out.stderr, "");

        let out2 = parse_max_rss("lead\n2048 maximum resident set size").unwrap();
        assert_eq!(out2.max_rss_kib, 2);
        assert_eq!(out2.stderr, "lead\n");
    }

    #[test]
    fn no_false_positive_on_partial_or_glued_label() {
        // Ends with the label words but not the exact label suffix.
        let stderr = "the program reports resident set size info\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Missing);
        // Glued to the label.
        let stderr = "xmaximum resident set size\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Missing);
        // Hybrid junk.
        let stderr = "999 maximum resident set size (kbytes): 5\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Missing);
    }

    // --- failure modes --------------------------------------------------------

    #[test]
    fn missing_metric_line() {
        assert_eq!(parse_max_rss("").unwrap_err(), ParseError::Missing);
        assert_eq!(
            parse_max_rss("just some child stderr\nno metric at all\n").unwrap_err(),
            ParseError::Missing
        );
    }

    #[test]
    fn duplicate_metric_lines_macos() {
        let stderr = "123 maximum resident set size\n456 maximum resident set size\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Duplicate);
    }

    #[test]
    fn duplicate_metric_lines_gnu() {
        let stderr =
            "Maximum resident set size (kbytes): 1\nMaximum resident set size (kbytes): 2\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Duplicate);
    }

    #[test]
    fn duplicate_metric_lines_mixed_formats() {
        let stderr = "123 maximum resident set size\nMaximum resident set size (kbytes): 2\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Duplicate);
    }

    #[test]
    fn negative_is_rejected_for_both_formats() {
        assert_eq!(
            parse_max_rss("-5 maximum resident set size\n").unwrap_err(),
            ParseError::Negative
        );
        assert_eq!(
            parse_max_rss("Maximum resident set size (kbytes): -5\n").unwrap_err(),
            ParseError::Negative
        );
    }

    #[test]
    fn overflow_is_rejected_for_both_formats() {
        let huge = "99999999999999999999999999"; // > u64::MAX
        assert_eq!(
            parse_max_rss(&format!("{huge} maximum resident set size\n")).unwrap_err(),
            ParseError::Overflow
        );
        assert_eq!(
            parse_max_rss(&format!("Maximum resident set size (kbytes): {huge}\n")).unwrap_err(),
            ParseError::Overflow
        );
    }

    #[test]
    fn malformed_is_rejected_for_both_formats() {
        // macOS variants: non-digit, decimal, leading '+', label-only, lone sign,
        // signed-non-digit.
        for line in [
            "abc maximum resident set size",
            "1.5 maximum resident set size",
            "+5 maximum resident set size",
            "maximum resident set size",
            "- maximum resident set size",
            "-abc maximum resident set size",
            "0x10 maximum resident set size",
        ] {
            assert_eq!(
                parse_max_rss(&format!("{line}\n")).unwrap_err(),
                ParseError::Malformed,
                "line={line}"
            );
        }
        // GNU variants.
        for line in [
            "Maximum resident set size (kbytes): abc",
            "Maximum resident set size (kbytes): 1.5",
            "Maximum resident set size (kbytes):",
            "Maximum resident set size (kbytes): +5",
        ] {
            assert_eq!(
                parse_max_rss(&format!("{line}\n")).unwrap_err(),
                ParseError::Malformed,
                "line={line}"
            );
        }
    }

    #[test]
    fn bad_number_takes_precedence_over_duplicate() {
        // One clean line + one negative line: the negative is reported, not
        // Duplicate, and deterministically (first bad in line order).
        let stderr = "123 maximum resident set size\n-5 maximum resident set size\n";
        assert_eq!(parse_max_rss(stderr).unwrap_err(), ParseError::Negative);
    }

    #[test]
    fn error_type_is_display_and_std_error() {
        fn assert_display(e: ParseError, want: &str) {
            assert_eq!(e.to_string(), want);
            // Also exercises the std::error::Error impl.
            let _: &dyn std::error::Error = &e;
        }
        assert_display(
            ParseError::Missing,
            "timeparse: no maximum resident set size line",
        );
        assert_display(
            ParseError::Duplicate,
            "timeparse: multiple maximum resident set size lines",
        );
        assert_display(
            ParseError::Negative,
            "timeparse: negative maximum resident set size",
        );
        assert_display(
            ParseError::Overflow,
            "timeparse: maximum resident set size overflows u64",
        );
        assert_display(
            ParseError::Malformed,
            "timeparse: malformed maximum resident set size line",
        );
    }
}
