use std::{fs, path::PathBuf};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    knowledge::{
        ConceptKind, WriteKnowledgeRequest, approve_knowledge, list_unreviewed_knowledge,
        normalize_and_validate_knowledge, parse_knowledge_response,
        write_knowledge_note_with_clock,
    },
    registry::{CaptureRequest, capture_asset},
};
use tempfile::TempDir;

const NOW: &str = "2026-07-18T00:00:00Z";

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct KnowledgeFixture {
    _root: TempDir,
    repository: PathBuf,
    asset_id: String,
    clock: FixedClock,
}

impl KnowledgeFixture {
    fn repo(&self) -> &std::path::Path {
        &self.repository
    }

    fn asset_id(&self) -> &str {
        &self.asset_id
    }

    fn write(
        &self,
        asset_id: &str,
        response: &[u8],
        replace: bool,
    ) -> Result<mko_core::knowledge::WriteKnowledgeResult, mko_core::error::MkoError> {
        write_knowledge_note_with_clock(
            WriteKnowledgeRequest::new(&self.repository, asset_id, response.to_vec())
                .with_replace(replace),
            &self.clock,
        )
    }
}

fn knowledge_fixture() -> KnowledgeFixture {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let provider = root.path().join("provider");
    let local_config = root.path().join("local-config.yaml");
    fs::create_dir_all(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    fs::write(
        repository.join("knowledge-os.yaml"),
        "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
    )
    .unwrap();
    fs::write(
        &local_config,
        format!("provider_root: {}\n", provider.display()),
    )
    .unwrap();
    let pdf = provider.join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nfixture").unwrap();
    let clock = FixedClock(
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc),
    );
    let asset_id = capture_asset(
        CaptureRequest::new(&repository, &pdf)
            .with_local_config(&local_config)
            .with_captured_at(clock.now_utc()),
    )
    .unwrap()
    .asset_id;
    KnowledgeFixture {
        _root: root,
        repository,
        asset_id,
        clock,
    }
}

const VALID: &str = r#"{
  "synthesis": "A signals-and-systems text covering LTI systems and transforms.",
  "concepts": [
    {"name": "Convolution", "kind": "formula", "body": "x*h(t)=∫x(τ)h(t−τ)dτ", "tags": ["LTI"], "locator": "§4.2"},
    {"name": "Causal signal", "kind": "definition", "body": "x(t)=0 for t<0", "tags": [], "locator": null}
  ]
}"#;

#[test]
fn parses_and_validates_a_well_formed_response() {
    let mut r = parse_knowledge_response(VALID.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
    assert!(!r.synthesis.is_empty());
    assert_eq!(r.concepts.len(), 2);
    assert_eq!(r.concepts[0].kind, ConceptKind::Formula);
}

#[test]
fn rejects_unknown_fields() {
    let bad = r#"{"synthesis":"x","concepts":[],"extra":1}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn rejects_empty_synthesis() {
    let bad = r#"{"synthesis":"   ","concepts":[]}"#;
    let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
    assert_eq!(
        normalize_and_validate_knowledge(&mut r).unwrap_err().code(),
        "semantic_schema_invalid"
    );
}

#[test]
fn rejects_concept_with_empty_body_or_multiline_name() {
    for bad in [
        r#"{"synthesis":"x","concepts":[{"name":"A","kind":"concept","body":"  ","tags":[],"locator":null}]}"#,
        r#"{"synthesis":"x","concepts":[{"name":"A\nB","kind":"concept","body":"y","tags":[],"locator":null}]}"#,
    ] {
        let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
        assert!(normalize_and_validate_knowledge(&mut r).is_err());
    }
}

#[test]
fn rejects_invalid_kind() {
    let bad = r#"{"synthesis":"x","concepts":[{"name":"A","kind":"joke","body":"y","tags":[],"locator":null}]}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn allows_empty_concepts() {
    let mut r = parse_knowledge_response(r#"{"synthesis":"x","concepts":[]}"#.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
}

#[test]
fn write_creates_an_unreviewed_note_with_a_content_revision() {
    let kb = knowledge_fixture();
    let res = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    assert_eq!(res.result, "created");
    assert!(res.content_revision.starts_with("sha256:"));
    let doc = fs::read_to_string(kb.repo().join(&res.knowledge_path)).unwrap();
    assert!(doc.contains("status: unreviewed"));
    assert!(doc.contains("approved_revision: null"));
    assert!(doc.contains("# ") && doc.contains("## Synthesis") && doc.contains("## Concepts"));
    assert!(doc.contains("Convolution"));
}

#[test]
fn write_is_idempotent_for_identical_content() {
    let kb = knowledge_fixture();
    let a = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let b = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    assert_eq!(b.result, "existing");
    assert_eq!(a.content_revision, b.content_revision);
}

#[test]
fn regenerating_requires_replace_and_resets_to_unreviewed_keeping_prior_approved_revision() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let other = VALID.replace(
        "LTI systems and transforms",
        "LTI systems, transforms, and sampling",
    );
    let err = kb
        .write(kb.asset_id(), other.as_bytes(), false)
        .unwrap_err();
    assert_eq!(err.code(), "replace_required");
    let ok = kb.write(kb.asset_id(), other.as_bytes(), true).unwrap();
    assert_eq!(ok.result, "replaced");
    let doc = fs::read_to_string(kb.repo().join(&ok.knowledge_path)).unwrap();
    assert!(doc.contains("status: unreviewed"));
}

#[test]
fn write_rejects_unknown_asset() {
    let kb = knowledge_fixture();
    let err = kb
        .write("personal-asset-deadbeef", VALID.as_bytes(), false)
        .unwrap_err();
    assert_eq!(err.code(), "asset_not_found");
}

#[test]
fn approve_marks_reviewed_and_records_approved_revision() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    approve_knowledge(kb.repo(), &w.knowledge_id, &w.content_revision).unwrap();
    let doc = fs::read_to_string(kb.repo().join(&w.knowledge_path)).unwrap();
    assert!(doc.contains("status: reviewed"));
    assert!(doc.contains(&format!("approved_revision: {}", w.content_revision)));
    assert!(doc.contains("reviewed_at:") && !doc.contains("reviewed_at: null"));
}

#[test]
fn approve_rejects_a_stale_revision() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let err = approve_knowledge(kb.repo(), &w.knowledge_id, "sha256:0000").unwrap_err();
    assert_eq!(err.code(), "knowledge_revision_mismatch");
}

#[test]
fn list_unreviewed_returns_only_unreviewed_notes() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 1);
    approve_knowledge(kb.repo(), &w.knowledge_id, &w.content_revision).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 0);
}
