//! Regression tests for B4 (#[non_exhaustive]) and B7 (Serialize/Deserialize
//! on `Tier` and `EscalationReason`).

use crawlberg::{EscalationReason, Tier};

#[test]
fn tier_http_round_trips_through_serde() {
    let t = Tier::Http;
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, r#""http""#);
    let back: Tier = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t);
}

#[test]
fn tier_bypass_round_trips_through_serde() {
    let t = Tier::Bypass;
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, r#""bypass""#);
    let back: Tier = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t);
}

#[test]
fn tier_browser_round_trips_through_serde() {
    let t = Tier::Browser;
    let json = serde_json::to_string(&t).unwrap();
    assert_eq!(json, r#""browser""#);
    let back: Tier = serde_json::from_str(&json).unwrap();
    assert_eq!(back, t);
}

#[test]
fn escalation_reason_waf_blocked_round_trips_through_serde() {
    let r = EscalationReason::WafBlocked {
        vendor: "cloudflare".into(),
    };
    let json = serde_json::to_string(&r).unwrap();
    let back: EscalationReason = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn escalation_reason_soft_block_round_trips_through_serde() {
    let r = EscalationReason::SoftBlock;
    let json = serde_json::to_string(&r).unwrap();
    let back: EscalationReason = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn escalation_reason_render_needed_round_trips_through_serde() {
    let r = EscalationReason::RenderNeeded;
    let json = serde_json::to_string(&r).unwrap();
    let back: EscalationReason = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn escalation_reason_origin_unreliable_round_trips_through_serde() {
    let r = EscalationReason::OriginUnreliable;
    let json = serde_json::to_string(&r).unwrap();
    let back: EscalationReason = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}
