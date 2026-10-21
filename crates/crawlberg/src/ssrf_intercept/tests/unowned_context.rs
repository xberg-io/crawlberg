use super::*;

#[test]
fn an_unrelated_context_lifecycle_preserves_the_observed_context_identity() {
    let mut registry = Registry::default();
    let context = BrowserContextId::new("OBSERVED");
    let unrelated = BrowserContextId::new("UNRELATED");
    let proof = registry.observe_unowned_context(&context).unwrap();
    let shared = registry.observe_unowned_context(&context).unwrap();
    assert!(Arc::ptr_eq(&proof, &shared));
    registry.register_context(unrelated.clone(), None);
    assert!(registry.unowned_context_matches(&context, &proof));
    registry.contexts.get_mut(&unrelated).unwrap().disposal_acknowledged = true;
    assert!(registry.retire_context(&unrelated));
    assert!(registry.unowned_context_matches(&context, &proof));
    assert_eq!(registry.unowned_context_observers.len(), 1);
}

#[test]
fn a_same_context_claim_or_retirement_permanently_invalidates_its_original_proof() {
    let mut registry = Registry::default();
    let context = BrowserContextId::new("REUSED");
    let original = registry.observe_unowned_context(&context).unwrap();
    registry.register_context(context.clone(), None);
    assert!(!registry.unowned_context_matches(&context, &original));
    registry.contexts.remove(&context);
    assert!(!registry.unowned_context_matches(&context, &original));
    let replacement = registry.observe_unowned_context(&context).unwrap();
    assert!(!Arc::ptr_eq(&original, &replacement));
    registry.register_context(context.clone(), None);
    registry.contexts.get_mut(&context).unwrap().disposal_acknowledged = true;
    let before_retirement = registry.observe_unowned_context(&context).unwrap();
    assert!(registry.retire_context(&context));
    assert!(!registry.unowned_context_matches(&context, &before_retirement));
    registry.retired_contexts.clear();
    let reused = registry.observe_unowned_context(&context).unwrap();
    assert!(!Arc::ptr_eq(&original, &reused));
    assert!(!Arc::ptr_eq(&replacement, &reused));
    assert!(!Arc::ptr_eq(&before_retirement, &reused));
    assert!(!registry.unowned_context_matches(&context, &original));
    assert!(registry.unowned_context_matches(&context, &reused));
}

#[test]
fn a_full_context_observer_map_preserves_live_proofs_and_reclaims_only_expired_entries() {
    let mut registry = Registry::default();
    let mut proofs = Vec::new();
    for index in 0..super::super::CHILD_CONTROLLER_PENDING_LIMIT {
        let context = BrowserContextId::new(format!("CONTEXT-{index}"));
        let proof = registry.observe_unowned_context(&context).unwrap();
        proofs.push((context, proof));
    }
    assert_eq!(
        registry.unowned_context_observers.len(),
        super::super::CHILD_CONTROLLER_PENDING_LIMIT
    );
    let shared = registry.observe_unowned_context(&proofs[0].0).unwrap();
    assert!(Arc::ptr_eq(&shared, &proofs[0].1));
    let fresh = BrowserContextId::new("FRESH");
    assert!(registry.observe_unowned_context(&fresh).is_none());
    for (context, proof) in &proofs {
        assert!(registry.unowned_context_matches(context, proof));
    }
    let (expired, last) = proofs.pop().unwrap();
    drop(last);
    let fresh_proof = registry.observe_unowned_context(&fresh).unwrap();
    assert!(!registry.unowned_context_observers.contains_key(&expired));
    assert_eq!(
        registry.unowned_context_observers.len(),
        super::super::CHILD_CONTROLLER_PENDING_LIMIT
    );
    for (context, proof) in &proofs {
        assert!(registry.unowned_context_matches(context, proof));
    }
    assert!(registry.unowned_context_matches(&fresh, &fresh_proof));
    drop(proofs);
    drop(shared);
    drop(fresh_proof);
    let final_context = BrowserContextId::new("FINAL");
    let final_proof = registry.observe_unowned_context(&final_context).unwrap();
    assert_eq!(registry.unowned_context_observers.len(), 1);
    assert!(registry.unowned_context_matches(&final_context, &final_proof));
}
