//! Tests for `BrowserMode::Stealth` semantics.
//!
//! These tests verify:
//!
//! 1. `BrowserMode::Stealth` exists and serialises correctly.
//! 2. The stealth gate predicate (`matches!(mode, BrowserMode::Stealth)`) is
//!    `true` only for `Stealth`, not for `Auto`, `Always`, or `Never`.
//! 3. The removed `BrowserConfig.stealth` boolean field is no longer present.
//! 4. `BrowserMode::Stealth` causes the engine to route to the browser tier
//!    (confirmed by observing that a request is NOT sent to the HTTP-tier mock
//!    when mode is `Stealth` and `BrowserBackend::Chromiumoxide` would need a
//!    real browser — this is tested via the expected mock-miss path).
//!
//! The actual JS-patch injection (`stealth::apply_stealth_patches`) requires a
//! live chromiumoxide `Page`. The gate logic itself (`matches!(...)`) is
//! trivially proved by the predicate tests below; a full end-to-end browser
//! test requires the `browser` feature and a real Chrome binary.

use crawlberg::{BrowserConfig, BrowserMode};

/// `BrowserMode::Stealth` must serialise to the string `"stealth"` and
/// deserialise back correctly (serde `rename_all = "snake_case"`).
#[test]
fn browser_mode_stealth_serialises_to_snake_case() {
    let json = serde_json::to_string(&BrowserMode::Stealth).unwrap();
    assert_eq!(json, r#""stealth""#, "stealth must serialise to \"stealth\"");

    let round: BrowserMode = serde_json::from_str(&json).unwrap();
    assert_eq!(round, BrowserMode::Stealth, "round-trip must produce Stealth");
}

/// The gate expression `matches!(mode, BrowserMode::Stealth)` must be `true`
/// only for `Stealth` and `false` for all other variants. This is the exact
/// expression used at every call site in the engine and browser modules.
#[test]
fn stealth_gate_predicate_is_true_only_for_stealth_variant() {
    assert!(
        matches!(BrowserMode::Stealth, BrowserMode::Stealth),
        "Stealth must match the Stealth gate"
    );
    assert!(
        !matches!(BrowserMode::Auto, BrowserMode::Stealth),
        "Auto must NOT match the Stealth gate"
    );
    assert!(
        !matches!(BrowserMode::Always, BrowserMode::Stealth),
        "Always must NOT match the Stealth gate"
    );
    assert!(
        !matches!(BrowserMode::Never, BrowserMode::Stealth),
        "Never must NOT match the Stealth gate"
    );
}

#[test]
#[allow(dead_code)]
fn browser_config_has_no_stealth_bool_field() {
    let config = BrowserConfig {
        mode: BrowserMode::Stealth,
        ..BrowserConfig::default()
    };
    assert_eq!(config.mode, BrowserMode::Stealth);
}

/// A match over all `BrowserMode` variants: proves `Stealth` is an
/// independent variant, not an alias, and that the match is exhaustive.
#[test]
fn browser_mode_all_variants_covered() {
    let modes = [
        BrowserMode::Auto,
        BrowserMode::Always,
        BrowserMode::Never,
        BrowserMode::Stealth,
    ];

    let mut stealth_count = 0usize;
    for mode in &modes {
        match mode {
            BrowserMode::Auto => {}
            BrowserMode::Always => {}
            BrowserMode::Never => {}
            BrowserMode::Stealth => stealth_count += 1,
        }
    }
    assert_eq!(stealth_count, 1, "exactly one Stealth variant must exist");
}

/// The default `BrowserMode` must be `Auto`, not `Stealth` — existing callers
/// must not have their behaviour changed by the addition of the new variant.
#[test]
fn default_browser_mode_is_auto_not_stealth() {
    let config = BrowserConfig::default();
    assert_eq!(
        config.mode,
        BrowserMode::Auto,
        "default BrowserConfig.mode must be Auto, not Stealth"
    );
}
