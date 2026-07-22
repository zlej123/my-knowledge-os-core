use std::fs;

use assert_cmd::Command;
use chrono::{Duration, Utc};
use mko_core::{
    asset_v2::{HydrationConfirmationV2, RegisterAssetRequestV2, register_pdf_asset_v2},
    model_v2::{
        ConfidenceV2, EvidenceRefV2, KnowledgeBasisV2, KnowledgeRecommendationOutcomeV2,
        KnowledgeRecommendationV2, KnowledgeResponseV2, KnowledgeUnitKindV2, KnowledgeUnitV2,
        LimitationBasisV2, PreparedMetadataV2, SourceClaimV2, SourceLimitationV2, SourceResponseV2,
    },
    prepared_v2::build_pdf_prepared_content_v2,
    revision_v2::canonical_json_bytes,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

#[test]
#[allow(deprecated)]
fn source_and_knowledge_writes_return_strict_v2_envelopes_and_join_one_queue_item() {
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
    let evidence = EvidenceRefV2 {
        block_id: "block-000001".into(),
        locator: "page:1;chunk:1;granularity:coarse".into(),
        text_span_utf8: None,
        table_range: None,
    };
    let source = SourceResponseV2 {
        schema_version: 2,
        title: "Example paper".into(),
        authors: Vec::new(),
        publication_date: None,
        one_sentence_summary: "A bounded summary.".into(),
        general_summary: "A grounded general summary.".into(),
        key_claims: vec![SourceClaimV2 {
            text: "The evidence text exists.".into(),
            evidence_refs: vec![evidence.clone()],
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
    };
    let source_path = root.path().join("source-response.json");
    fs::write(&source_path, serde_json::to_vec(&source).unwrap()).unwrap();

    let source_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["source", "write-draft", "--bundle"])
        .arg(&bundle_path)
        .arg("--response")
        .arg(&source_path)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let source_output: serde_json::Value = serde_json::from_slice(&source_output).unwrap();
    assert_eq!(source_output["command"], "source.write");
    assert_eq!(source_output["data"]["outcome"], "created");
    assert_eq!(source_output["data"]["projection_state"], "current");

    let knowledge = KnowledgeResponseV2 {
        schema_version: 2,
        synthesis: "The document supports one reusable fact.".into(),
        units: vec![KnowledgeUnitV2 {
            kind: KnowledgeUnitKindV2::Fact,
            title: "Evidence fact".into(),
            body: "The evidence text exists.".into(),
            confidence: ConfidenceV2::High,
            basis: KnowledgeBasisV2::Evidence,
            evidence_refs: vec![evidence],
            tags: vec!["example".into()],
        }],
    };
    let knowledge_path = root.path().join("knowledge-response.json");
    fs::write(&knowledge_path, serde_json::to_vec(&knowledge).unwrap()).unwrap();
    let knowledge_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["knowledge", "write", "--asset-id"])
        .arg(&asset.id)
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--response")
        .arg(&knowledge_path)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let knowledge_output: serde_json::Value = serde_json::from_slice(&knowledge_output).unwrap();
    assert_eq!(knowledge_output["command"], "knowledge.write");
    assert_eq!(knowledge_output["data"]["outcome"], "created");
    assert_eq!(knowledge_output["data"]["projection_state"], "current");

    let queue_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["queue", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let queue: serde_json::Value = serde_json::from_slice(&queue_output).unwrap();
    assert_eq!(queue["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue["data"]["items"][0]["item_type"], "combined");
}

fn make_owner_only(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
