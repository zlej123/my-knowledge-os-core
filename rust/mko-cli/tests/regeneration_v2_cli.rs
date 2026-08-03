use std::{fs, path::Path};

use assert_cmd::Command;
use chrono::{Duration, Utc};
use mko_core::{
    asset_v2::{HydrationConfirmationV2, RegisterAssetRequestV2, register_pdf_asset_v2},
    model_v2::{
        EvidenceRefV2, KnowledgeRecommendationOutcomeV2, KnowledgeRecommendationV2,
        LimitationBasisV2, PreparedMetadataV2, SourceClaimV2, SourceLimitationV2, SourceResponseV2,
    },
    prepared_v2::build_pdf_prepared_content_v2,
    revision_v2::canonical_json_bytes,
    scaffold_v2::scaffold_personal_kb_v2,
};
use serde_json::Value;
use tempfile::tempdir;

const FEEDBACK: &str = "Replace the key claim evidence with the block that carries the figure.";

struct Environment {
    _root: tempfile::TempDir,
    repository: std::path::PathBuf,
    provider: std::path::PathBuf,
    bundle_path: std::path::PathBuf,
    response_path: std::path::PathBuf,
    asset_id: String,
}

// The regeneration loop is exercised exactly as the Skill drives it: every
// step below is a real CLI invocation against a scaffolded v2 repository.
#[allow(deprecated)]
fn command(environment: &Environment) -> Command {
    let mut command = Command::cargo_bin("mko").unwrap();
    command
        .env("MKO_PERSONAL_PROVIDER_ROOT", &environment.provider)
        .current_dir(&environment.repository);
    command
}

fn environment() -> Environment {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    fs::write(provider.join("paper.pdf"), b"%PDF-1.7\nfixture").unwrap();
    let asset = register_pdf_asset_v2(RegisterAssetRequestV2 {
        repository_root: &repository,
        provider_root: &provider,
        logical_locator: "paper.pdf",
        hydration_confirmation: HydrationConfirmationV2::NotConfirmed,
    })
    .unwrap()
    .asset;
    let bundle = build_pdf_prepared_content_v2(
        &asset,
        &["Evidence text for the test.".into()],
        PreparedMetadataV2 {
            title: Some("Example paper".into()),
            authors: Vec::new(),
            created_at: None,
        },
    )
    .unwrap();
    let bundle_path = root.path().join("prepared.json");
    let created_at = Utc::now();
    let session = serde_json::json!({
        "schema_version": 2,
        "artifact_type": "prepared_session",
        "created_at": created_at,
        "expires_at": created_at + Duration::hours(24),
        "bundle": bundle,
    });
    fs::write(&bundle_path, canonical_json_bytes(&session).unwrap()).unwrap();
    make_owner_only(&bundle_path);
    let response_path = root.path().join("source-response.json");
    fs::write(
        &response_path,
        serde_json::to_vec(&source_response("A grounded general summary.")).unwrap(),
    )
    .unwrap();
    Environment {
        _root: root,
        repository,
        provider,
        bundle_path,
        response_path,
        asset_id: asset.id,
    }
}

fn source_response(general_summary: &str) -> SourceResponseV2 {
    let evidence = EvidenceRefV2 {
        block_id: "block-000001".into(),
        locator: "page:1;chunk:1;granularity:coarse".into(),
        text_span_utf8: None,
        table_range: None,
    };
    SourceResponseV2 {
        schema_version: 2,
        title: "Example paper".into(),
        authors: Vec::new(),
        publication_date: None,
        one_sentence_summary: "A bounded summary.".into(),
        general_summary: general_summary.into(),
        key_claims: vec![SourceClaimV2 {
            text: "The evidence text exists.".into(),
            evidence_refs: vec![evidence],
        }],
        limitations: vec![SourceLimitationV2 {
            text: "No additional evidence was supplied.".into(),
            basis: LimitationBasisV2::ObservedMissingEvidence,
            evidence_refs: Vec::new(),
        }],
        tags: vec!["example".into()],
        knowledge_recommendation: KnowledgeRecommendationV2 {
            outcome: KnowledgeRecommendationOutcomeV2::Recommend,
            reasons: vec!["Reusable concept.".into()],
        },
    }
}

fn run_json(environment: &Environment, arguments: &[&str], input: Option<&Path>) -> (Value, bool) {
    let mut command = command(environment);
    command.args(arguments);
    if let Some(input) = input {
        command.arg(input);
    }
    command.args(["--format", "json-v2"]);
    let output = command.assert().get_output().clone();
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        output.status.success(),
    )
}

#[test]
fn regeneration_closes_the_request_changes_loop_with_typed_surfaces() {
    let environment = environment();

    let (written, ok) = run_json(
        &environment,
        &[
            "source",
            "write-draft",
            "--bundle",
            environment.bundle_path.to_str().unwrap(),
            "--response",
            environment.response_path.to_str().unwrap(),
        ],
        None,
    );
    assert!(ok);
    assert_eq!(written["data"]["outcome"], "created");
    let record_id = written["data"]["record_id"].as_str().unwrap().to_owned();
    let first_revision = written["data"]["revision"].as_str().unwrap().to_owned();

    let (opened, ok) = run_json(&environment, &["review-open", &record_id], None);
    assert!(ok);
    let decision = serde_json::json!({
        "session_id": opened["data"]["session_id"],
        "card_digest": opened["data"]["card_digest"],
        "target_decisions": [{
            "record_id": record_id,
            "decision": "request_changes",
            "feedback": FEEDBACK
        }]
    });
    let decision_path = environment.repository.join("decision.json");
    fs::write(&decision_path, serde_json::to_vec(&decision).unwrap()).unwrap();
    let (feedback, ok) = run_json(
        &environment,
        &["review-feedback", "--input"],
        Some(&decision_path),
    );
    assert!(ok, "review-feedback failed: {feedback}");
    assert_eq!(feedback["command"], "review.feedback");

    // The typed regeneration context: feedback, binding revision, and asset.
    let (context, ok) = run_json(&environment, &["show", &record_id], None);
    assert!(ok);
    assert_eq!(context["data"]["asset_id"], environment.asset_id);
    let target = &context["data"]["targets"][0];
    assert_eq!(target["state"], "changes_requested");
    assert_eq!(target["current_feedback"], FEEDBACK);
    assert_eq!(target["addressed_feedback"], Value::Null);
    assert_eq!(target["displayed_revision"], first_revision);

    let replacement = source_response("A regenerated summary that follows the requested change.");
    fs::write(
        &environment.response_path,
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();

    // Unbound replacement is refused with a typed re-read instruction.
    let (unbound, ok) = run_json(
        &environment,
        &[
            "source",
            "write-draft",
            "--bundle",
            environment.bundle_path.to_str().unwrap(),
            "--response",
            environment.response_path.to_str().unwrap(),
        ],
        None,
    );
    assert!(!ok);
    assert_eq!(unbound["error"]["code"], "replacement_revision_required");
    assert_eq!(unbound["error"]["next_action"], "review");

    let stale_revision = format!("sha256:{}", "f".repeat(64));
    let (stale, ok) = run_json(
        &environment,
        &[
            "source",
            "write-draft",
            "--bundle",
            environment.bundle_path.to_str().unwrap(),
            "--response",
            environment.response_path.to_str().unwrap(),
            "--expected-revision",
            &stale_revision,
        ],
        None,
    );
    assert!(!ok);
    assert_eq!(stale["error"]["code"], "record_revision_stale");
    assert_eq!(stale["error"]["next_action"], "review");

    let (replaced, ok) = run_json(
        &environment,
        &[
            "source",
            "write-draft",
            "--bundle",
            environment.bundle_path.to_str().unwrap(),
            "--response",
            environment.response_path.to_str().unwrap(),
            "--expected-revision",
            &first_revision,
        ],
        None,
    );
    assert!(ok);
    assert_eq!(replaced["data"]["outcome"], "replaced");
    assert_eq!(replaced["data"]["projection_state"], "current");
    let second_revision = replaced["data"]["revision"].as_str().unwrap().to_owned();
    assert_ne!(second_revision, first_revision);

    let (queue, ok) = run_json(&environment, &["queue"], None);
    assert!(ok);
    assert_eq!(queue["data"]["items"][0]["state"], "revised_unreviewed");
    assert_eq!(queue["data"]["items"][0]["next_action"], "display");

    let (revised, ok) = run_json(&environment, &["show", &record_id], None);
    assert!(ok);
    let target = &revised["data"]["targets"][0];
    assert_eq!(target["state"], "revised_unreviewed");
    assert_eq!(target["current_feedback"], Value::Null);
    assert_eq!(target["addressed_feedback"], FEEDBACK);
    assert_eq!(target["previous_reviewed_revision"], first_revision);
    assert_eq!(target["displayed_revision"], second_revision);

    let card = revised["data"]["card_markdown"].as_str().unwrap();
    assert!(card.contains("Feedback addressed by this revision for"));
    assert!(card.contains(FEEDBACK));
    assert!(card.contains("Changes since the reviewed revision for"));
    assert!(card.contains(&format!("--- reviewed {first_revision}")));
    assert!(card.contains(&format!("+++ current {second_revision}")));
    assert!(card.contains("-  \"general_summary\": \"A grounded general summary.\""));
    assert!(card.contains(
        "+  \"general_summary\": \"A regenerated summary that follows the requested change.\""
    ));
}

fn make_owner_only(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(windows)]
    mko_windows_acl::apply_owner_only_to_path(path, mko_windows_acl::Inheritance::None).unwrap();
    #[cfg(not(any(unix, windows)))]
    let _ = path;
}
