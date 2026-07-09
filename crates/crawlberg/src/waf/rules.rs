//! TOML loader, validation, and the compiled `Rules` struct.
//!
//! # Observability
//!
//! OTel counters (`opentelemetry::global`) emit unconditionally — consumers
//! (xberg-enterprise) expect these always. Tracing spans/events are always compiled
//! as `tracing` is now an unconditional dependency.

use std::collections::HashMap;
use std::path::Path;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use opentelemetry::KeyValue;
use serde::Deserialize;
use thiserror::Error;

use crate::http::HttpResponse;
use crate::types::{WafClassifyError, WafSignal};

/// Maximum body size (bytes) at which body fingerprints are checked on 2xx.
/// Real content pages overwhelmingly exceed this; challenge pages are tiny.
pub(crate) const CHALLENGE_BODY_LIMIT: usize = 100 * 1024;

/// Maximum number of fingerprints allowed in a rules file.
pub(crate) const MAX_FINGERPRINTS: usize = 1_000;
/// Maximum byte length for any individual pattern string.
pub(crate) const MAX_PATTERN_LEN: usize = 4_096;
/// Maximum number of signals per fingerprint.
pub(crate) const MAX_SIGNALS_PER_FINGERPRINT: usize = 16;

#[derive(Debug, Deserialize)]
struct TomlRules {
    fingerprint: Vec<TomlFingerprint>,
}

#[derive(Debug, Deserialize, Clone)]
struct TomlFingerprint {
    id: String,
    vendor: String,
    weight: f32,
    signals: Vec<TomlSignal>,
}

#[derive(Debug, Deserialize, Clone)]
struct TomlSignal {
    kind: String,
    name: Option<String>,
    value_contains: Option<String>,
    pattern: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum Signal {
    /// Header must be present. `value_contains` is an optional substring.
    ResponseHeader {
        name: String,
        value_contains: Option<String>,
    },
    /// Body substring (case-insensitive). Matched via Aho-Corasick.
    /// The actual matching is done by the AC automaton; the fingerprint index
    /// is retrieved via `Rules::pattern_to_fp`. No per-signal fields needed at
    /// match time — this variant's presence indicates the fingerprint requires
    /// at least one body match (checked via `matched_fp_indices` in `classify`).
    BodySubstring,
}

#[derive(Debug, Clone)]
pub(crate) struct Fingerprint {
    pub(crate) id: String,
    pub(crate) vendor: String,
    pub(crate) weight: f32,
    pub(crate) signals: Vec<Signal>,
}

/// Compiled WAF rules: fingerprint list + single Aho-Corasick automaton.
///
/// `builtin()` loads from the compile-time TOML corpus. Hot-reload swaps
/// the `Rules` wrapped in [`arc_swap::ArcSwap`] (Commit 1.6).
#[derive(Debug)]
pub struct Rules {
    pub(crate) fingerprints: Vec<Fingerprint>,
    pub(crate) automaton: AhoCorasick,
    /// Maps an AC pattern index → fingerprint index in `fingerprints`.
    pub(crate) pattern_to_fp: Vec<usize>,
}

/// Error returned when loading or validating a rules file.
#[derive(Debug, Error)]
pub enum RulesError {
    /// TOML parse failure.
    #[error("parse error: {0}")]
    ParseError(#[from] toml::de::Error),
    /// A fingerprint failed validation.
    #[error("validation error for fingerprint '{fingerprint_id}': {reason}")]
    Validation {
        /// The `id` field of the fingerprint that failed validation.
        fingerprint_id: String,
        /// Human-readable description of the validation failure.
        reason: String,
    },
    /// Aho-Corasick build failure.
    #[error("failed to build Aho-Corasick automaton: {0}")]
    MatcherBuild(String),
}

/// Load and compile rules from a TOML file on disk.
///
/// Used by [`crate::waf::TomlClassifier::watch`] to reload rules on file
/// change. Also useful in tests or when the caller manages the rules file.
pub fn load_from_path(path: &Path) -> Result<Rules, RulesError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| RulesError::MatcherBuild(format!("cannot read {}: {e}", path.display())))?;
    load_from_str(&content)
}

/// Load and compile rules from a TOML string.
pub fn load_from_str(toml_src: &str) -> Result<Rules, RulesError> {
    let parsed: TomlRules = toml::from_str(toml_src)?;
    compile(parsed)
}

impl Rules {
    /// Load the canonical built-in corpus embedded at compile time.
    ///
    /// The corpus is validated by unit tests; a broken corpus is a
    /// programming error so `expect` is appropriate here.
    pub fn builtin() -> Self {
        let src = include_str!("../../rules/waf_fingerprints.toml");
        load_from_str(src).expect("builtin waf_fingerprints.toml must be valid")
    }

    /// Number of fingerprints in this compiled rule set.
    #[cfg(test)]
    pub(crate) fn fingerprint_count(&self) -> usize {
        self.fingerprints.len()
    }
}

fn compile(raw: TomlRules) -> Result<Rules, RulesError> {
    if raw.fingerprint.len() > MAX_FINGERPRINTS {
        return Err(RulesError::Validation {
            fingerprint_id: String::new(),
            reason: format!(
                "too many fingerprints: {} > MAX_FINGERPRINTS={MAX_FINGERPRINTS}",
                raw.fingerprint.len()
            ),
        });
    }

    let mut fingerprints: Vec<Fingerprint> = Vec::with_capacity(raw.fingerprint.len());
    let mut ac_patterns: Vec<String> = Vec::new();
    let mut pattern_to_fp: Vec<usize> = Vec::new();

    let mut seen_ids: HashMap<String, ()> = HashMap::new();

    for (fp_idx, raw_fp) in raw.fingerprint.iter().enumerate() {
        if seen_ids.contains_key(&raw_fp.id) {
            return Err(RulesError::Validation {
                fingerprint_id: raw_fp.id.clone(),
                reason: "duplicate fingerprint id".into(),
            });
        }
        if raw_fp.id.contains('.') {
            return Err(RulesError::Validation {
                fingerprint_id: raw_fp.id.clone(),
                reason: "fingerprint id must not contain dots".into(),
            });
        }
        seen_ids.insert(raw_fp.id.clone(), ());

        if raw_fp.signals.len() > MAX_SIGNALS_PER_FINGERPRINT {
            return Err(RulesError::Validation {
                fingerprint_id: raw_fp.id.clone(),
                reason: format!(
                    "too many signals: {} > MAX_SIGNALS_PER_FINGERPRINT={MAX_SIGNALS_PER_FINGERPRINT}",
                    raw_fp.signals.len()
                ),
            });
        }

        let mut signals: Vec<Signal> = Vec::with_capacity(raw_fp.signals.len());

        for raw_sig in &raw_fp.signals {
            match raw_sig.kind.as_str() {
                "response_header" => {
                    let name = raw_sig
                        .name
                        .clone()
                        .ok_or_else(|| RulesError::Validation {
                            fingerprint_id: raw_fp.id.clone(),
                            reason: "response_header signal requires 'name'".into(),
                        })?
                        .to_lowercase();
                    if raw_sig
                        .value_contains
                        .as_deref()
                        .is_some_and(|vc| vc.len() > MAX_PATTERN_LEN)
                    {
                        return Err(RulesError::Validation {
                            fingerprint_id: raw_fp.id.clone(),
                            reason: format!(
                                "pattern too long: {} > MAX_PATTERN_LEN={MAX_PATTERN_LEN}",
                                raw_sig.value_contains.as_ref().map_or(0, |s| s.len())
                            ),
                        });
                    }
                    signals.push(Signal::ResponseHeader {
                        name,
                        value_contains: raw_sig.value_contains.as_ref().map(|s| s.to_lowercase()),
                    });
                }
                "body_substring" => {
                    let pattern = raw_sig
                        .pattern
                        .clone()
                        .ok_or_else(|| RulesError::Validation {
                            fingerprint_id: raw_fp.id.clone(),
                            reason: "body_substring signal requires 'pattern'".into(),
                        })?
                        .to_lowercase();
                    if pattern.len() > MAX_PATTERN_LEN {
                        return Err(RulesError::Validation {
                            fingerprint_id: raw_fp.id.clone(),
                            reason: format!(
                                "pattern too long: {} > MAX_PATTERN_LEN={MAX_PATTERN_LEN}",
                                pattern.len()
                            ),
                        });
                    }
                    ac_patterns.push(pattern);
                    pattern_to_fp.push(fp_idx);
                    signals.push(Signal::BodySubstring);
                }
                other => {
                    return Err(RulesError::Validation {
                        fingerprint_id: raw_fp.id.clone(),
                        reason: format!("unknown signal kind '{other}'"),
                    });
                }
            }
        }

        fingerprints.push(Fingerprint {
            id: raw_fp.id.clone(),
            vendor: raw_fp.vendor.clone(),
            weight: raw_fp.weight,
            signals,
        });
    }

    let automaton = AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::LeftmostFirst)
        .build(ac_patterns)
        .map_err(|e| RulesError::MatcherBuild(e.to_string()))?;

    Ok(Rules {
        fingerprints,
        automaton,
        pattern_to_fp,
    })
}

impl Rules {
    /// Inspect `response` and return the first matching [`WafSignal`], if any.
    ///
    /// The algorithm runs in two passes:
    ///
    /// 1. **Header-first short-circuit**: fingerprints whose signals are ALL
    ///    `response_header` are evaluated before the body is scanned. If any
    ///    header-only fingerprint matches, its signal is returned immediately
    ///    without running the AC body scan. This makes the TOML corpus the
    ///    single source of truth for the 2xx header-stamp early-exit path in
    ///    `http.rs` (replacing the old `headers_only_waf_match` function).
    ///
    /// 2. **Full scan**: Aho-Corasick runs over the body and all fingerprints
    ///    (including mixed header+body ones) are evaluated.
    ///
    /// On a 2xx response the body fingerprint check is only applied when the
    /// body is ≤ `CHALLENGE_BODY_LIMIT` — real content pages are much larger.
    /// Header signals are always checked regardless of status code.
    ///
    /// Returns `Ok(None)` for clean responses, `Ok(Some(sig))` for a match,
    /// and `Err(WafClassifyError)` for classifier-internal failures.
    pub fn classify(&self, response: &HttpResponse) -> Result<Option<WafSignal>, WafClassifyError> {
        let is_2xx = (200..300).contains(&response.status);
        let body_too_large = response.body_bytes.len() > CHALLENGE_BODY_LIMIT;

        // ~keep Header-only fingerprints short-circuit before body scans; the TOML corpus stays authoritative.
        for fingerprint in &self.fingerprints {
            if fingerprint
                .signals
                .iter()
                .all(|s| matches!(s, Signal::ResponseHeader { .. }))
                && fingerprint.signals.iter().all(|s| match s {
                    Signal::ResponseHeader { name, value_contains } => {
                        header_matches(&response.headers, name, value_contains.as_deref())
                    }
                    Signal::BodySubstring => false,
                })
            {
                let signal = WafSignal {
                    vendor: fingerprint.vendor.clone(),
                    fingerprint_id: fingerprint.id.clone(),
                    weight: fingerprint.weight,
                };
                crate::telemetry::metrics::registry()
                    .waf_blocks_total
                    .add(1, &[KeyValue::new("vendor", signal.vendor.clone())]);
                return Ok(Some(signal));
            }
        }

        // ~keep Skip body matching on large 2xx responses because they are likely legitimate content.
        let check_body = !is_2xx || !body_too_large;

        let mut matched_fp_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
        if check_body {
            for mat in self.automaton.find_iter(&response.body) {
                let fp_idx = self.pattern_to_fp[mat.pattern().as_usize()];
                matched_fp_indices.insert(fp_idx);
            }
        }

        for (fp_idx, fingerprint) in self.fingerprints.iter().enumerate() {
            if self.fingerprint_matches(fingerprint, fp_idx, &matched_fp_indices, response, is_2xx) {
                let signal = WafSignal {
                    vendor: fingerprint.vendor.clone(),
                    fingerprint_id: fingerprint.id.clone(),
                    weight: fingerprint.weight,
                };
                crate::telemetry::metrics::registry()
                    .waf_blocks_total
                    .add(1, &[KeyValue::new("vendor", signal.vendor.clone())]);
                return Ok(Some(signal));
            }
        }
        Ok(None)
    }

    fn fingerprint_matches(
        &self,
        fingerprint: &Fingerprint,
        fp_idx: usize,
        matched_body_fps: &std::collections::HashSet<usize>,
        response: &HttpResponse,
        is_2xx: bool,
    ) -> bool {
        let body_too_large = response.body_bytes.len() > CHALLENGE_BODY_LIMIT;
        let check_body = !is_2xx || !body_too_large;

        for signal in &fingerprint.signals {
            match signal {
                Signal::BodySubstring => {
                    if !check_body {
                        // ~keep Body signals cannot fire when the body is over the scan limit.
                        return false;
                    }
                    if !matched_body_fps.contains(&fp_idx) {
                        return false;
                    }
                }
                Signal::ResponseHeader { name, value_contains } => {
                    if !header_matches(&response.headers, name, value_contains.as_deref()) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// Returns true if the header `name` is present and (optionally) any of its
/// values contain `value_contains` (case-insensitive).
fn header_matches(headers: &HashMap<String, Vec<String>>, name: &str, value_contains: Option<&str>) -> bool {
    if name == "x-px-" {
        return headers.keys().any(|k| k.starts_with("x-px-"));
    }

    match headers.get(name) {
        None => false,
        Some(values) => match value_contains {
            None => true,
            Some(needle) => values.iter().any(|v| v.to_lowercase().contains(needle)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_response(status: u16, headers: Vec<(&str, &str)>, body: &str) -> crate::http::HttpResponse {
        let mut header_map: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in headers {
            header_map.entry(k.to_lowercase()).or_default().push(v.to_string());
        }
        let body_bytes = body.as_bytes().to_vec();
        crate::http::HttpResponse {
            status,
            content_type: "text/html".into(),
            body: body.to_string(),
            body_bytes,
            headers: header_map,
            browser_extras: None,
            final_url: "https://example.com/".into(),
        }
    }

    #[test]
    fn builtin_rules_parse_without_error() {
        let rules = Rules::builtin();
        assert!(
            !rules.fingerprints.is_empty(),
            "builtin must have at least one fingerprint"
        );
    }

    #[test]
    fn load_from_str_rejects_duplicate_id() {
        let src = r#"
[[fingerprint]]
id = "duplicate_id"
vendor = "test"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "foo"

[[fingerprint]]
id = "duplicate_id"
vendor = "test"
weight = 1.0
[[fingerprint.signals]]
kind = "body_substring"
pattern = "bar"
"#;
        assert!(matches!(load_from_str(src), Err(RulesError::Validation { .. })));
    }

    #[test]
    fn load_from_str_rejects_unknown_signal_kind() {
        let src = r#"
[[fingerprint]]
id = "bad_signal"
vendor = "test"
weight = 1.0
[[fingerprint.signals]]
kind = "magic_beam"
"#;
        assert!(matches!(load_from_str(src), Err(RulesError::Validation { .. })));
    }

    #[test]
    fn classify_returns_ok_none_for_clean_response() {
        let rules = Rules::builtin();
        let resp = make_response(200, vec![], "<html><body><p>Hello world</p></body></html>");
        assert!(
            matches!(rules.classify(&resp), Ok(None)),
            "clean response must return Ok(None)"
        );
    }

    #[test]
    fn classify_returns_ok_some_for_matching_response() {
        let rules = Rules::builtin();
        // ~keep `x-datadome` is header-only, so this must match without a body token.
        let resp = make_response(200, vec![("x-datadome", "blocked")], "<html>ok</html>");
        assert!(
            matches!(rules.classify(&resp), Ok(Some(_))),
            "x-datadome header must return Ok(Some(_))"
        );
    }
}
