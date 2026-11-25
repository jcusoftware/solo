//! Advisory Virginia ABC advertising lint (M2).
//!
//! Flags phrases and happy-hour windows that commonly conflict with Virginia
//! ABC advertising and happy-hour rules (3 VAC 5-20-10 through 3 VAC 5-20-100,
//! advertising; 3 VAC 5-50-160, happy hour). The rule set is a heuristic: it
//! MUST be re-verified against current Virginia ABC regulations before client
//! use, and human sign-off is still required. Mirrored in
//! `packs/hospitality/tools/abc-lint.mjs`; keep both in sync.

/// Regulatory citations surfaced with every finding.
pub const CITATIONS: &str = "3 VAC 5-20-10 through 3 VAC 5-20-100 (advertising); 3 VAC 5-50-160 (happy hour). Advisory only: re-verify against current Virginia ABC regulations; human sign-off required.";

/// Case-insensitive phrase rules: (rule id, phrase).
pub const PHRASE_RULES: &[(&str, &str)] = &[
    ("bottomless", "bottomless"),
    ("two_for_one", "2-for-1"),
    ("two_for_one", "2 for 1"),
    ("two_for_one", "2for1"),
    ("two_for_one", "two for one"),
    ("two_for_one", "two-for-one"),
    ("unlimited_drinks", "unlimited drinks"),
];

/// Earliest flagged happy-hour end/start (inclusive), minutes after midnight: 21:00.
pub const HH_LATE_START_MIN: u32 = 21 * 60;
/// Exclusive upper bound for overnight/early-morning hours: 06:00.
pub const HH_OVERNIGHT_END_MIN: u32 = 6 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable rule id, e.g. `bottomless`, `two_for_one`, `happy_hour_late_end`.
    pub rule: &'static str,
    /// The matched text (original casing) or offending value.
    pub matched: String,
    /// Byte offset of the match in the linted text (0 for time findings).
    pub offset: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonFinding {
    pub path: String,
    pub finding: Finding,
}

fn is_boundary(c: Option<char>) -> bool {
    c.is_none_or(|c| !c.is_alphanumeric())
}

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2013}' | '\u{2014}' | '\u{2212}')
}

fn normalize_for_lint(text: &str) -> (String, Vec<usize>, Vec<usize>) {
    let mut norm = String::with_capacity(text.len());
    let mut starts = Vec::with_capacity(text.len());
    let mut ends = Vec::with_capacity(text.len());
    let mut iter = text.char_indices().peekable();
    while let Some((idx, ch)) = iter.next() {
        if ch.is_whitespace() {
            let ws_start = idx;
            let mut ws_end = idx + ch.len_utf8();
            while let Some(&(next_idx, next_ch)) = iter.peek() {
                if next_ch.is_whitespace() {
                    ws_end = next_idx + next_ch.len_utf8();
                    iter.next();
                } else {
                    break;
                }
            }
            norm.push(' ');
            starts.push(ws_start);
            ends.push(ws_end);
        } else {
            let ch_end = idx + ch.len_utf8();
            let mapped = if is_dash(ch) {
                '-'
            } else {
                ch.to_ascii_lowercase()
            };
            let mut buf = [0u8; 4];
            let encoded = mapped.encode_utf8(&mut buf);
            for _ in 0..encoded.len() {
                starts.push(idx);
                ends.push(ch_end);
            }
            norm.push_str(encoded);
        }
    }
    (norm, starts, ends)
}

/// Lint free text for flagged promotional phrases.
pub fn lint_text(text: &str) -> Vec<Finding> {
    let (norm, starts, ends) = normalize_for_lint(text);
    let mut findings = Vec::new();
    for (rule, phrase) in PHRASE_RULES {
        let mut start = 0;
        while let Some(haystack) = norm.get(start..)
            && let Some(pos) = haystack.find(phrase)
        {
            let at = start + pos;
            let end = at + phrase.len();
            if let (Some(&orig_at), Some(&orig_end)) = (
                starts.get(at),
                end.checked_sub(1).and_then(|i| ends.get(i)),
            ) && let Some(matched) = text.get(orig_at..orig_end)
            {
                let before = text.get(..orig_at).and_then(|s| s.chars().next_back());
                let after = text.get(orig_end..).and_then(|s| s.chars().next());
                if is_boundary(before) && is_boundary(after) {
                    findings.push(Finding {
                        rule,
                        matched: matched.to_string(),
                        offset: orig_at,
                        message: format!("Flagged promotional phrase \"{phrase}\". {CITATIONS}"),
                    });
                }
            }
            start = end;
        }
    }
    findings.sort_by_key(|f| f.offset);
    findings
}

/// Parse a strict 24-hour `HH:MM` string into minutes after midnight.
pub fn parse_hhmm(value: &str) -> Option<u32> {
    let (h, m) = value.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn is_restricted_hh_minute(min: u32) -> bool {
    !(HH_OVERNIGHT_END_MIN..HH_LATE_START_MIN).contains(&min)
}

/// Flag a happy-hour start time inside the restricted 21:00–05:59 overnight window,
/// or an unparseable value.
pub fn lint_happy_hour_start(start: &str) -> Option<Finding> {
    match parse_hhmm(start) {
        None => Some(Finding {
            rule: "happy_hour_start_invalid",
            matched: start.to_string(),
            offset: 0,
            message: format!("Happy-hour start \"{start}\" is not a 24-hour HH:MM time. {CITATIONS}"),
        }),
        Some(min) if is_restricted_hh_minute(min) => Some(Finding {
            rule: "happy_hour_late_start",
            matched: start.to_string(),
            offset: 0,
            message: format!(
                "Happy-hour start \"{start}\" falls within restricted 21:00-05:59 overnight window. {CITATIONS}"
            ),
        }),
        Some(_) => None,
    }
}

/// Flag a happy-hour end time inside the restricted 21:00–05:59 overnight window,
/// or an unparseable value.
pub fn lint_happy_hour_end(end: &str) -> Option<Finding> {
    match parse_hhmm(end) {
        None => Some(Finding {
            rule: "happy_hour_end_invalid",
            matched: end.to_string(),
            offset: 0,
            message: format!("Happy-hour end \"{end}\" is not a 24-hour HH:MM time. {CITATIONS}"),
        }),
        Some(min) if is_restricted_hh_minute(min) => Some(Finding {
            rule: "happy_hour_late_end",
            matched: end.to_string(),
            offset: 0,
            message: format!(
                "Happy-hour end \"{end}\" falls within restricted 21:00-05:59 overnight window. {CITATIONS}"
            ),
        }),
        Some(_) => None,
    }
}

fn is_js_ident(k: &str) -> bool {
    let mut chars = k.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn walk_json(node: &serde_json::Value, path: &str, out: &mut Vec<JsonFinding>) {
    match node {
        serde_json::Value::String(s) => {
            for finding in lint_text(s) {
                out.push(JsonFinding {
                    path: path.to_string(),
                    finding,
                });
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                walk_json(v, &format!("{path}[{i}]"), out);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = if is_js_ident(k) {
                    format!(".{k}")
                } else {
                    let quoted = serde_json::to_string(k).unwrap_or_else(|_| format!("{k:?}"));
                    format!("[{quoted}]")
                };
                let next_path = format!("{path}{key}");
                walk_json(v, &next_path, out);
                if k == "happy_hour"
                    && let serde_json::Value::Object(hh) = v
                {
                    if let Some(start_val) = hh.get("start") {
                        let s = match start_val {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if let Some(finding) = lint_happy_hour_start(&s) {
                            out.push(JsonFinding {
                                path: format!("{next_path}.start"),
                                finding,
                            });
                        }
                    }
                    if let Some(end_val) = hh.get("end") {
                        let s = match end_val {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if let Some(finding) = lint_happy_hour_end(&s) {
                            out.push(JsonFinding {
                                path: format!("{next_path}.end"),
                                finding,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Lint every string value in a JSON document and validate `happy_hour.start` / `happy_hour.end`.
pub fn lint_json_value(doc: &serde_json::Value) -> Vec<JsonFinding> {
    let mut out = Vec::new();
    walk_json(doc, "$", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_copy_passes() {
        assert!(lint_text("$2 off house wines by the glass, 3-6:30pm weekdays.").is_empty());
        assert!(lint_text("Our espresso is bottom-of-the-cup good.").is_empty());
    }

    #[test]
    fn flags_each_phrase_case_insensitively() {
        for (text, rule) in [
            ("BOTTOMLESS mimosas", "bottomless"),
            ("2-for-1 drafts", "two_for_one"),
            ("2 For 1 wings and beer", "two_for_one"),
            ("2for1 tuesday", "two_for_one"),
            ("Two for One pints", "two_for_one"),
            ("two-for-one cocktails", "two_for_one"),
            ("Unlimited Drinks all night", "unlimited_drinks"),
        ] {
            let f = lint_text(text);
            assert_eq!(f.len(), 1, "{text}");
            assert_eq!(f[0].rule, rule, "{text}");
            assert!(f[0].message.contains("3 VAC 5-20-10"));
        }
    }

    #[test]
    fn normalizes_unicode_dashes_nbsp_and_multiline_whitespace() {
        for (text, rule, expected_match) in [
            ("2\u{2013}for\u{2013}1 drafts", "two_for_one", "2\u{2013}for\u{2013}1"),
            ("2\u{2014}for\u{2014}1 cocktails", "two_for_one", "2\u{2014}for\u{2014}1"),
            ("Two\u{2212}for\u{2212}One pints", "two_for_one", "Two\u{2212}for\u{2212}One"),
            ("2\u{00A0}for\u{00A0}1 beers", "two_for_one", "2\u{00A0}for\u{00A0}1"),
            ("2  for  1 wines", "two_for_one", "2  for  1"),
            ("unlimited\ndrinks nightly", "unlimited_drinks", "unlimited\ndrinks"),
            ("Unlimited\r\n  Drinks", "unlimited_drinks", "Unlimited\r\n  Drinks"),
        ] {
            let f = lint_text(text);
            assert_eq!(f.len(), 1, "{text}");
            assert_eq!(f[0].rule, rule, "{text}");
            assert_eq!(f[0].matched, expected_match, "{text}");
            assert_eq!(f[0].offset, 0, "{text}");
        }
    }

    #[test]
    fn respects_word_boundaries() {
        assert!(lint_text("Entrees $12 for 15 minutes").is_empty());
        assert!(lint_text("Serves 2 for 10").is_empty());
        assert!(lint_text("\u{1D41B}bottomless").is_empty());
    }

    #[test]
    fn reports_offsets_and_multiple_matches() {
        let f = lint_text("Bottomless brunch and 2-for-1 wine");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].offset, 0);
        assert_eq!(f[1].matched, "2-for-1");

        let unicode = lint_text("\u{1F942} 2\u{2013}for\u{2013}1 drafts");
        assert_eq!(unicode.len(), 1);
        assert_eq!(unicode[0].offset, 5);
        assert_eq!(unicode[0].matched, "2\u{2013}for\u{2013}1");
    }

    #[test]
    fn happy_hour_window() {
        assert!(lint_happy_hour_end("06:00").is_none());
        assert!(lint_happy_hour_end("18:30").is_none());
        assert!(lint_happy_hour_end("20:59").is_none());
        for t in ["21:00", "23:59", "00:00", "01:30", "02:00", "02:01", "03:00", "05:59"] {
            assert_eq!(
                lint_happy_hour_end(t).map(|f| f.rule),
                Some("happy_hour_late_end"),
                "{t}"
            );
        }
        for t in ["9pm", "24:00", "7:00", "18:60", ""] {
            assert_eq!(
                lint_happy_hour_end(t).map(|f| f.rule),
                Some("happy_hour_end_invalid"),
                "{t}"
            );
        }

        assert!(lint_happy_hour_start("06:00").is_none());
        assert!(lint_happy_hour_start("15:00").is_none());
        assert!(lint_happy_hour_start("20:59").is_none());
        for t in ["21:00", "22:00", "00:00", "03:00", "05:59"] {
            assert_eq!(
                lint_happy_hour_start(t).map(|f| f.rule),
                Some("happy_hour_late_start"),
                "{t}"
            );
        }
        for t in ["3pm", "24:00", "7:00", ""] {
            assert_eq!(
                lint_happy_hour_start(t).map(|f| f.rule),
                Some("happy_hour_start_invalid"),
                "{t}"
            );
        }
    }

    #[test]
    fn lint_json_walks_nested_happy_hour_and_strings() {
        let doc = json!({
            "events": {
                "late_night": {
                    "happy_hour": {
                        "start": "22:00",
                        "end": "03:00",
                        "offers": ["Bottomless sangria"]
                    }
                }
            },
            "special-key": "2-for-1 drafts"
        });
        let findings = lint_json_value(&doc);
        let pairs: Vec<(&str, &str)> = findings
            .iter()
            .map(|f| (f.path.as_str(), f.finding.rule))
            .collect();
        assert!(pairs.contains(&("$.events.late_night.happy_hour.offers[0]", "bottomless")));
        assert!(pairs.contains(&("$.events.late_night.happy_hour.start", "happy_hour_late_start")));
        assert!(pairs.contains(&("$.events.late_night.happy_hour.end", "happy_hour_late_end")));
        assert!(pairs.contains(&("$[\"special-key\"]", "two_for_one")));
        assert_eq!(findings.len(), 4);
    }

    #[test]
    fn demo_venue_manifest_passes_abc_lint() {
        let raw = include_str!("../../demo/venue.json");
        let doc: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert!(lint_json_value(&doc).is_empty());
    }
}
