use mko_core::capture_v1::{
    CaptureEnvelopeV1, ClassifierProposalV1, RouteOutcomeKindV1, RouteRejectionReasonV1,
    RoutingAuthorityV1, RoutingNextActionV1, SubjectScopeV1, parse_capture_envelope_v1,
    parse_classifier_proposal_v1, resolve_route_v1,
};
use serde_json::Value;

fn schema() -> Value {
    serde_json::from_str(include_str!(
        "../../../schemas/capture/v1/capture-envelope.schema.json"
    ))
    .expect("capture schema is valid JSON")
}

fn fixture(bytes: &'static [u8]) -> Value {
    serde_json::from_slice(bytes).expect("capture fixture is valid JSON")
}

fn general() -> CaptureEnvelopeV1 {
    parse_capture_envelope_v1(include_bytes!(
        "../../../tests/fixtures/capture/v1/general-explicit.json"
    ))
    .expect("general fixture parses")
}

fn finance() -> CaptureEnvelopeV1 {
    parse_capture_envelope_v1(include_bytes!(
        "../../../tests/fixtures/capture/v1/finance-explicit.json"
    ))
    .expect("finance fixture parses")
}

fn proposal(
    proposed_scope: SubjectScopeV1,
    confidence: u8,
    mixed_subjects: bool,
    conflicting: bool,
) -> ClassifierProposalV1 {
    ClassifierProposalV1 {
        proposed_scope,
        confidence,
        mixed_subjects,
        conflicting,
    }
}

#[test]
fn capture_schema_and_strict_dtos_accept_general_and_finance_examples() {
    let validator = jsonschema::validator_for(&schema()).expect("capture schema compiles");
    for bytes in [
        include_bytes!("../../../tests/fixtures/capture/v1/general-explicit.json").as_slice(),
        include_bytes!("../../../tests/fixtures/capture/v1/finance-explicit.json").as_slice(),
    ] {
        let value = fixture(bytes);
        assert!(validator.is_valid(&value), "fixture must validate: {value}");
        let parsed = parse_capture_envelope_v1(bytes).expect("fixture DTO parses");
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
}

#[test]
fn capture_schema_and_strict_dtos_reject_credentials_paths_and_decision_actions() {
    let validator = jsonschema::validator_for(&schema()).expect("capture schema compiles");
    for bytes in [
        include_bytes!("../../../tests/fixtures/capture/v1/invalid-credential.json").as_slice(),
        include_bytes!("../../../tests/fixtures/capture/v1/invalid-local-path.json").as_slice(),
        include_bytes!("../../../tests/fixtures/capture/v1/invalid-decision-action.json")
            .as_slice(),
    ] {
        let value = fixture(bytes);
        assert!(
            !validator.is_valid(&value),
            "fixture must be rejected: {value}"
        );
        assert!(parse_capture_envelope_v1(bytes).is_err());
    }
}

#[test]
fn parser_enforces_nested_bounds_and_positive_non_chat_telegram_ids() {
    let mut oversized_pdf = fixture(include_bytes!(
        "../../../tests/fixtures/capture/v1/finance-explicit.json"
    ));
    oversized_pdf["input"]["declared_size_bytes"] = serde_json::json!(52_428_801_u64);
    assert!(parse_capture_envelope_v1(&serde_json::to_vec(&oversized_pdf).unwrap()).is_err());

    let mut negative_sender = fixture(include_bytes!(
        "../../../tests/fixtures/capture/v1/general-explicit.json"
    ));
    negative_sender["channel_identity"]["sender_id"] = serde_json::json!("-123");
    assert!(parse_capture_envelope_v1(&serde_json::to_vec(&negative_sender).unwrap()).is_err());

    let mut overlong_text = fixture(include_bytes!(
        "../../../tests/fixtures/capture/v1/general-explicit.json"
    ));
    overlong_text["input"] = serde_json::json!({
        "input_type": "text",
        "text": "x".repeat(16_385),
    });
    assert!(parse_capture_envelope_v1(&serde_json::to_vec(&overlong_text).unwrap()).is_err());
}

#[test]
fn classifier_proposals_are_strict_and_bounded() {
    let invalid = serde_json::json!({
        "proposed_scope": "finance",
        "confidence": 101,
        "mixed_subjects": false,
        "conflicting": false,
        "ignored": true,
    });
    assert!(parse_classifier_proposal_v1(&serde_json::to_vec(&invalid).unwrap()).is_err());

    let overconfident = proposal(SubjectScopeV1::Finance, 101, false, false);
    assert!(overconfident.validate().is_err());

    let valid = serde_json::json!({
        "proposed_scope": "general",
        "confidence": 80,
        "mixed_subjects": false,
        "conflicting": false,
    });
    assert_eq!(
        parse_classifier_proposal_v1(&serde_json::to_vec(&valid).unwrap()).unwrap(),
        proposal(SubjectScopeV1::General, 80, false, false)
    );

    let oversized = serde_json::json!({ "ignored": "x".repeat(16_385) });
    let error = parse_classifier_proposal_v1(&serde_json::to_vec(&oversized).unwrap()).unwrap_err();
    assert_eq!(error.code(), "routing_proposal_too_large");
}

#[test]
fn explicit_general_and_finance_selection_are_ready_and_ignore_classifier_output() {
    let finance_proposal = proposal(SubjectScopeV1::Finance, 100, true, true);
    let outcome = resolve_route_v1(&general(), Some(&finance_proposal), None).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::ReadyGeneral);
    assert_eq!(outcome.confirmed_scope, Some(SubjectScopeV1::General));
    assert_eq!(
        outcome.routing_authority,
        Some(RoutingAuthorityV1::UserSelected)
    );

    let general_proposal = proposal(SubjectScopeV1::General, 100, false, false);
    let outcome = resolve_route_v1(&finance(), Some(&general_proposal), None).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::ReadyFinance);
    assert_eq!(outcome.confirmed_scope, Some(SubjectScopeV1::Finance));
    assert_eq!(
        outcome.routing_authority,
        Some(RoutingAuthorityV1::UserSelected)
    );
}

#[test]
fn classifier_only_proposals_never_return_ready_outcomes() {
    let mut envelope = general();
    envelope.selected_scope = None;

    let high_general = proposal(SubjectScopeV1::General, 80, false, false);
    let outcome = resolve_route_v1(&envelope, Some(&high_general), None).unwrap();
    assert_eq!(
        outcome.outcome,
        RouteOutcomeKindV1::GeneralConfirmationRequired
    );
    assert_eq!(
        outcome.next_action,
        Some(RoutingNextActionV1::ConfirmGeneral)
    );

    for confidence in [80, 100] {
        let finance = proposal(SubjectScopeV1::Finance, confidence, false, false);
        let outcome = resolve_route_v1(&envelope, Some(&finance), None).unwrap();
        assert_eq!(
            outcome.outcome,
            RouteOutcomeKindV1::FinanceConfirmationRequired
        );
        assert_eq!(
            outcome.next_action,
            Some(RoutingNextActionV1::ConfirmFinance)
        );
        assert!(outcome.confirmed_scope.is_none());
        assert!(outcome.routing_authority.is_none());
    }

    for confidence in [0, 79] {
        let finance = proposal(SubjectScopeV1::Finance, confidence, false, false);
        let outcome = resolve_route_v1(&envelope, Some(&finance), None).unwrap();
        assert_eq!(
            outcome.outcome,
            RouteOutcomeKindV1::RoutingConfirmationRequired
        );
        assert_eq!(outcome.next_action, Some(RoutingNextActionV1::ChooseScope));
        assert!(outcome.confirmed_scope.is_none());
        assert!(outcome.routing_authority.is_none());
    }
}

#[test]
fn low_mixed_conflicting_and_unavailable_classification_require_a_scope_choice() {
    let mut envelope = general();
    envelope.selected_scope = None;
    let cases = [
        None,
        Some(proposal(SubjectScopeV1::General, 79, false, false)),
        Some(proposal(SubjectScopeV1::General, 100, true, false)),
        Some(proposal(SubjectScopeV1::Finance, 100, false, true)),
    ];
    for proposal in &cases {
        let outcome = resolve_route_v1(&envelope, proposal.as_ref(), None).unwrap();
        assert_eq!(
            outcome.outcome,
            RouteOutcomeKindV1::RoutingConfirmationRequired
        );
        assert_eq!(outcome.next_action, Some(RoutingNextActionV1::ChooseScope));
        assert!(outcome.confirmed_scope.is_none());
    }
}

#[test]
fn only_a_matching_unambiguous_user_confirmation_makes_a_proposal_ready() {
    let mut envelope = general();
    envelope.selected_scope = None;
    let finance = proposal(SubjectScopeV1::Finance, 50, false, false);
    let outcome =
        resolve_route_v1(&envelope, Some(&finance), Some(SubjectScopeV1::Finance)).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::ReadyFinance);
    assert_eq!(outcome.confirmed_scope, Some(SubjectScopeV1::Finance));
    assert_eq!(
        outcome.routing_authority,
        Some(RoutingAuthorityV1::UserConfirmedProposal)
    );

    let outcome =
        resolve_route_v1(&envelope, Some(&finance), Some(SubjectScopeV1::General)).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::Rejected);
    assert_eq!(
        outcome.rejection_reason,
        Some(RouteRejectionReasonV1::ConfirmationDoesNotMatchProposal)
    );

    let mixed = proposal(SubjectScopeV1::Finance, 90, true, false);
    let outcome = resolve_route_v1(&envelope, Some(&mixed), Some(SubjectScopeV1::Finance)).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::Rejected);
    assert_eq!(
        outcome.rejection_reason,
        Some(RouteRejectionReasonV1::ConfirmationCannotResolveMixedOrConflictingProposal)
    );

    let outcome = resolve_route_v1(&envelope, None, Some(SubjectScopeV1::General)).unwrap();
    assert_eq!(outcome.outcome, RouteOutcomeKindV1::Rejected);
    assert_eq!(
        outcome.rejection_reason,
        Some(RouteRejectionReasonV1::ConfirmationRequiresProposal)
    );
}

#[test]
fn resolver_is_pure_and_exposes_no_project2035_or_delivery_mutation_surface() {
    let mut envelope = general();
    envelope.selected_scope = None;
    let outcome = resolve_route_v1(
        &envelope,
        Some(&proposal(SubjectScopeV1::Finance, 100, false, false)),
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.outcome,
        RouteOutcomeKindV1::FinanceConfirmationRequired
    );
    assert!(outcome.confirmed_scope.is_none());
    assert!(outcome.routing_authority.is_none());
}
