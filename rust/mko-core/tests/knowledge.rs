use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use chrono::{DateTime, Utc};
use mko_core::{
    check::{CheckRequest, check_repository},
    clock::Clock,
    knowledge::{
        ConceptKind, KnowledgeMutationObserver, KnowledgeScanObserver, KnowledgeSearchQuery,
        WriteKnowledgeRequest, approve_knowledge, approve_knowledge_with_clock_and_observer,
        list_knowledge, list_unreviewed_knowledge, normalize_and_validate_knowledge,
        parse_knowledge_response, search_knowledge, search_knowledge_with_scan,
        write_knowledge_note_with_clock, write_knowledge_note_with_clock_and_observer,
    },
    prepare::{PrepareRequest, prepare_source_with_extractor},
    provider_scan::{ElapsedClock, ScanLimits},
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
    provider: PathBuf,
    local_config: PathBuf,
    asset_id: String,
    bundle_path: PathBuf,
    clock: FixedClock,
}

impl KnowledgeFixture {
    fn repo(&self) -> &std::path::Path {
        &self.repository
    }

    fn asset_id(&self) -> &str {
        &self.asset_id
    }

    fn bundle_path(&self) -> &std::path::Path {
        &self.bundle_path
    }

    fn write(
        &self,
        asset_id: &str,
        response: &[u8],
        replace: bool,
    ) -> Result<mko_core::knowledge::WriteKnowledgeResult, mko_core::error::MkoError> {
        write_knowledge_note_with_clock(
            WriteKnowledgeRequest::new(&self.repository, asset_id, response.to_vec())
                .with_bundle(self.bundle_for(asset_id))
                .with_replace(replace),
            &self.clock,
        )
    }

    fn bundle_for(&self, asset_id: &str) -> PathBuf {
        self.repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"))
    }

    fn prepare_bundle(&self, asset_id: &str) -> PathBuf {
        let bundle_path = self.bundle_for(asset_id);
        prepare_source_with_extractor(
            PrepareRequest::new(&self.repository, asset_id, &bundle_path)
                .with_local_config(&self.local_config),
            |_, _| Ok(vec!["Fixture page".into()]),
        )
        .unwrap();
        bundle_path
    }

    fn second_asset(&self) -> (String, PathBuf) {
        let pdf = self.provider.join("second.pdf");
        fs::write(&pdf, b"%PDF-1.7\nsecond fixture").unwrap();
        let asset_id = capture_asset(
            CaptureRequest::new(&self.repository, &pdf)
                .with_local_config(&self.local_config)
                .with_captured_at(self.clock.now_utc()),
        )
        .unwrap()
        .asset_id;
        let bundle_path = self.prepare_bundle(&asset_id);
        (asset_id, bundle_path)
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
    let bundle_path = repository
        .join(".knowledge-os/runtime/prepared")
        .join(format!("{asset_id}.json"));
    prepare_source_with_extractor(
        PrepareRequest::new(&repository, &asset_id, &bundle_path).with_local_config(&local_config),
        |_, _| Ok(vec!["Fixture page".into()]),
    )
    .unwrap();
    KnowledgeFixture {
        _root: root,
        repository,
        provider,
        local_config,
        asset_id,
        bundle_path,
        clock,
    }
}

struct BarrierObserver(Arc<Barrier>);

impl KnowledgeMutationObserver for BarrierObserver {
    fn before_publication(&mut self) -> Result<(), mko_core::error::MkoError> {
        self.0.wait();
        Ok(())
    }
}

#[cfg(unix)]
struct SwapKnowledgeDirectoryObserver {
    repository: PathBuf,
}

#[cfg(unix)]
impl SwapKnowledgeDirectoryObserver {
    fn swap(&self) {
        fs::rename(
            self.repository.join("knowledge"),
            self.repository.join("knowledge-retained"),
        )
        .unwrap();
        fs::create_dir(self.repository.join("knowledge")).unwrap();
    }
}

#[cfg(unix)]
impl KnowledgeMutationObserver for SwapKnowledgeDirectoryObserver {
    fn before_publication(&mut self) -> Result<(), mko_core::error::MkoError> {
        self.swap();
        Ok(())
    }
}

#[cfg(unix)]
struct SwapKnowledgeAfterMetadataObserver {
    repository: PathBuf,
    outside: PathBuf,
}

#[cfg(unix)]
impl KnowledgeMutationObserver for SwapKnowledgeAfterMetadataObserver {
    fn after_knowledge_directory_metadata(&mut self) -> Result<(), mko_core::error::MkoError> {
        fs::rename(
            self.repository.join("knowledge"),
            self.repository.join("knowledge-retained"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&self.outside, self.repository.join("knowledge")).unwrap();
        Ok(())
    }

    fn before_publication(&mut self) -> Result<(), mko_core::error::MkoError> {
        Ok(())
    }
}

const VALID: &str = r#"{
  "synthesis": "A signals-and-systems text covering LTI systems and transforms.",
  "concepts": [
    {"name": "Convolution", "kind": "formula", "body": "x*h(t)=∫x(τ)h(t−τ)dτ", "tags": ["LTI"], "locator": "§4.2"},
    {"name": "Causal signal", "kind": "definition", "body": "x(t)=0 for t<0", "tags": [], "locator": null}
  ]
}"#;

const EMPTY_CONCEPTS: &str = r#"{
  "synthesis": "A useful note with no separately addressable concepts.",
  "concepts": []
}"#;

struct IncrementingElapsedClock(AtomicU64);

impl IncrementingElapsedClock {
    fn new() -> Self {
        Self(AtomicU64::new(0))
    }
}

impl ElapsedClock for IncrementingElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

fn knowledge_scan_limits(max_total_bytes: u64, max_elapsed_ms: u64) -> ScanLimits {
    ScanLimits {
        max_entries: 16,
        max_total_bytes,
        max_elapsed_ms,
        max_depth: 1,
        max_batch_items: 16,
    }
}

#[cfg(unix)]
struct SwapKnowledgeEntryObserver {
    entry: PathBuf,
    retained: PathBuf,
    outside: PathBuf,
    swapped: bool,
}

#[cfg(unix)]
impl KnowledgeScanObserver for SwapKnowledgeEntryObserver {
    fn before_entry_open(&mut self, _: &std::path::Path) -> Result<(), mko_core::error::MkoError> {
        if !self.swapped {
            fs::rename(&self.entry, &self.retained).unwrap();
            std::os::unix::fs::symlink(&self.outside, &self.entry).unwrap();
            self.swapped = true;
        }
        Ok(())
    }
}

struct GrowKnowledgeEntryObserver {
    entry: PathBuf,
    grown: bool,
}

impl KnowledgeScanObserver for GrowKnowledgeEntryObserver {
    fn after_entry_metadata(
        &mut self,
        _: &std::path::Path,
    ) -> Result<(), mko_core::error::MkoError> {
        if !self.grown {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(&self.entry)
                .unwrap();
            file.write_all(&vec![b'x'; 256]).unwrap();
            self.grown = true;
        }
        Ok(())
    }
}

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
    let original = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    approve_knowledge(
        kb.repo(),
        &original.knowledge_id,
        &original.content_revision,
    )
    .unwrap();
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
    assert!(doc.contains("reviewed_at: null"));
    assert!(doc.contains(&format!("approved_revision: {}", original.content_revision)));

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();
    assert!(!report.has_code("review_invalid"));
    assert!(!report.has_code("revision_mismatch"));
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
fn write_rejects_a_missing_prepared_bundle() {
    let kb = knowledge_fixture();
    fs::remove_file(kb.bundle_path()).unwrap();

    let err = kb
        .write(kb.asset_id(), VALID.as_bytes(), false)
        .unwrap_err();

    assert_eq!(err.code(), "runtime_publication_invalid");
}

#[test]
fn core_write_requires_a_prepared_bundle_even_with_source_compatible_constructor() {
    let kb = knowledge_fixture();

    let err = write_knowledge_note_with_clock(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec()),
        &kb.clock,
    )
    .unwrap_err();

    assert_eq!(err.code(), "bundle_required");
}

#[test]
fn write_rejects_a_bundle_outside_the_canonical_runtime_path() {
    let kb = knowledge_fixture();
    let outside = kb.repo().join("prepared.json");
    fs::copy(kb.bundle_path(), &outside).unwrap();

    let err = write_knowledge_note_with_clock(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())
            .with_bundle(&outside),
        &kb.clock,
    )
    .unwrap_err();

    assert_eq!(err.code(), "runtime_output_invalid");
}

#[test]
fn write_rejects_a_bundle_for_a_different_asset() {
    let kb = knowledge_fixture();
    let (_second_asset, second_bundle) = kb.second_asset();

    let err = write_knowledge_note_with_clock(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())
            .with_bundle(second_bundle),
        &kb.clock,
    )
    .unwrap_err();

    assert_eq!(err.code(), "bundle_invalid");
}

#[test]
fn write_rejects_an_untrusted_prepared_bundle() {
    let kb = knowledge_fixture();
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(kb.bundle_path()).unwrap()).unwrap();
    bundle["trust"] = "trusted".into();
    fs::write(
        kb.bundle_path(),
        serde_json::to_vec_pretty(&bundle).unwrap(),
    )
    .unwrap();

    let err = kb
        .write(kb.asset_id(), VALID.as_bytes(), false)
        .unwrap_err();

    assert_eq!(err.code(), "bundle_invalid");
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
fn approve_rejects_body_tampering_even_when_the_stored_revision_is_unchanged() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&written.knowledge_path);
    let tampered = fs::read_to_string(&path)
        .unwrap()
        .replace("LTI systems and transforms", "tampered after selection");
    fs::write(&path, tampered).unwrap();

    let err =
        approve_knowledge(kb.repo(), &written.knowledge_id, &written.content_revision).unwrap_err();

    assert_eq!(err.code(), "knowledge_revision_mismatch");
    assert!(
        fs::read_to_string(path)
            .unwrap()
            .contains("status: unreviewed")
    );
}

#[test]
fn concurrent_replace_and_approve_publish_only_one_stale_snapshot() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let changed = VALID.replace(
        "LTI systems and transforms",
        "LTI systems, transforms, and sampling",
    );
    let barrier = Arc::new(Barrier::new(2));

    let replace_repository = kb.repository.clone();
    let replace_bundle = kb.bundle_path.clone();
    let replace_asset = kb.asset_id.clone();
    let replace_clock = kb.clock.clone();
    let replace_barrier = Arc::clone(&barrier);
    let replace = thread::spawn(move || {
        let mut observer = BarrierObserver(replace_barrier);
        write_knowledge_note_with_clock_and_observer(
            WriteKnowledgeRequest::new(replace_repository, replace_asset, changed.into_bytes())
                .with_bundle(replace_bundle)
                .with_replace(true),
            &replace_clock,
            &mut observer,
        )
    });

    let approve_repository = kb.repository.clone();
    let approve_clock = kb.clock.clone();
    let approve_barrier = Arc::clone(&barrier);
    let knowledge_id = written.knowledge_id.clone();
    let content_revision = written.content_revision.clone();
    let approve = thread::spawn(move || {
        let mut observer = BarrierObserver(approve_barrier);
        approve_knowledge_with_clock_and_observer(
            &approve_repository,
            &knowledge_id,
            &content_revision,
            &approve_clock,
            &mut observer,
        )
    });

    let replace_result = replace.join().unwrap();
    let approve_result = approve.join().unwrap();
    assert_eq!(
        usize::from(replace_result.is_ok()) + usize::from(approve_result.is_ok()),
        1
    );
    for error in [replace_result.err(), approve_result.err()]
        .into_iter()
        .flatten()
    {
        assert_eq!(error.code(), "knowledge_revision_mismatch");
    }
    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();
    assert!(!report.has_code("review_invalid"));
    assert!(!report.has_code("revision_mismatch"));
}

#[test]
fn concurrent_first_creation_reports_created_then_existing() {
    let kb = knowledge_fixture();
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for timestamp in ["2026-07-18T14:59:59Z", "2026-07-18T15:00:01Z"] {
        let repository = kb.repository.clone();
        let bundle = kb.bundle_path.clone();
        let asset_id = kb.asset_id.clone();
        let clock = FixedClock(
            DateTime::parse_from_rfc3339(timestamp)
                .unwrap()
                .with_timezone(&Utc),
        );
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            write_knowledge_note_with_clock(
                WriteKnowledgeRequest::new(repository, asset_id, VALID.as_bytes().to_vec())
                    .with_bundle(bundle),
                &clock,
            )
            .unwrap()
            .result
        }));
    }

    let mut outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    outcomes.sort();
    assert_eq!(outcomes, ["created", "existing"]);
}

#[cfg(unix)]
#[test]
fn knowledge_directory_acquisition_rejects_a_symlink_swap_after_metadata_validation() {
    let kb = knowledge_fixture();
    let outside = kb._root.path().join("outside-knowledge");
    fs::create_dir(&outside).unwrap();
    let mut observer = SwapKnowledgeAfterMetadataObserver {
        repository: kb.repository.clone(),
        outside: outside.clone(),
    };

    let error = write_knowledge_note_with_clock_and_observer(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())
            .with_bundle(kb.bundle_path()),
        &kb.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_path_invalid");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    assert_eq!(
        fs::read_dir(kb.repo().join("knowledge-retained"))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn create_reports_failure_when_public_knowledge_directory_is_rebound_before_publication() {
    let kb = knowledge_fixture();
    let mut observer = SwapKnowledgeDirectoryObserver {
        repository: kb.repository.clone(),
    };

    let error = write_knowledge_note_with_clock_and_observer(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())
            .with_bundle(kb.bundle_path()),
        &kb.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_publication_invalid");
    assert_eq!(
        fs::read_dir(kb.repo().join("knowledge")).unwrap().count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn replace_reports_failure_when_public_knowledge_directory_is_rebound_before_publication() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let changed = VALID.replace(
        "LTI systems and transforms",
        "LTI systems, transforms, and sampling",
    );
    let mut observer = SwapKnowledgeDirectoryObserver {
        repository: kb.repository.clone(),
    };

    let error = write_knowledge_note_with_clock_and_observer(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), changed.into_bytes())
            .with_bundle(kb.bundle_path())
            .with_replace(true),
        &kb.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_publication_invalid");
    assert_eq!(
        fs::read_dir(kb.repo().join("knowledge")).unwrap().count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn approve_reports_failure_when_public_knowledge_directory_is_rebound_before_publication() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let mut observer = SwapKnowledgeDirectoryObserver {
        repository: kb.repository.clone(),
    };

    let error = approve_knowledge_with_clock_and_observer(
        kb.repo(),
        &written.knowledge_id,
        &written.content_revision,
        &kb.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_publication_invalid");
    assert_eq!(
        fs::read_dir(kb.repo().join("knowledge")).unwrap().count(),
        0
    );
}

#[test]
fn list_unreviewed_returns_only_unreviewed_notes() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 1);
    approve_knowledge(kb.repo(), &w.knowledge_id, &w.content_revision).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 0);
}

#[test]
fn list_knowledge_keeps_reviewed_notes_with_no_concepts() {
    let kb = knowledge_fixture();
    let written = kb
        .write(kb.asset_id(), EMPTY_CONCEPTS.as_bytes(), false)
        .unwrap();
    approve_knowledge(kb.repo(), &written.knowledge_id, &written.content_revision).unwrap();

    let notes = list_knowledge(kb.repo()).unwrap();

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].asset_id, kb.asset_id());
    assert_eq!(
        notes[0].review_status,
        mko_core::knowledge::ReviewState::Reviewed
    );
    assert!(notes[0].concepts.is_empty());
}

const OTHER: &str = r#"{
  "synthesis": "A control-theory text covering feedback stability.",
  "concepts": [
    {"name": "Feedback loop", "kind": "concept", "body": "output routed back as input", "tags": ["control"], "locator": "§1.1"}
  ]
}"#;

#[test]
fn search_finds_matches_across_documents_and_respects_filters() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let (second, _) = kb.second_asset();
    kb.write(&second, OTHER.as_bytes(), false).unwrap();

    let hits = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: "convolution".into(),
            kind: None,
            tag: None,
        },
    )
    .unwrap();
    assert!(hits.iter().any(|h| h.name == "Convolution"));

    let formulas = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: "x".into(),
            kind: Some(ConceptKind::Formula),
            tag: None,
        },
    )
    .unwrap();
    assert!(!formulas.is_empty());
    assert!(formulas.iter().all(|h| h.kind == ConceptKind::Formula));

    let tagged = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: String::new(),
            kind: None,
            tag: Some("control".into()),
        },
    )
    .unwrap();
    assert!(tagged.iter().any(|h| h.name == "Feedback loop"));
    assert!(tagged.iter().all(|h| h.name != "Convolution"));

    let none = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: "nonexistent-term".into(),
            kind: None,
            tag: None,
        },
    )
    .unwrap();
    assert!(none.is_empty());
}

#[test]
fn search_term_includes_the_concept_kind() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();

    let hits = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: "formula".into(),
            kind: None,
            tag: None,
        },
    )
    .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Convolution");
}

#[test]
fn knowledge_scan_has_one_elapsed_deadline() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let clock = IncrementingElapsedClock::new();
    let mut observer = ();

    let error = search_knowledge_with_scan(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: String::new(),
            kind: None,
            tag: None,
        },
        knowledge_scan_limits(1024 * 1024, 2),
        &clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "scan_time_limit");
}

#[cfg(unix)]
#[test]
fn knowledge_scan_does_not_follow_an_entry_swapped_to_a_symlink() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let entry = kb.repo().join(&written.knowledge_path);
    let retained = entry.with_extension("retained");
    let outside = kb._root.path().join("outside.md");
    let outside_bytes = fs::read(&entry).unwrap();
    fs::write(&outside, &outside_bytes).unwrap();
    let mut observer = SwapKnowledgeEntryObserver {
        entry,
        retained,
        outside: outside.clone(),
        swapped: false,
    };

    let error = search_knowledge_with_scan(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: String::new(),
            kind: None,
            tag: None,
        },
        knowledge_scan_limits(1024 * 1024, 5_000),
        &IncrementingElapsedClock::new(),
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_path_invalid");
    assert_eq!(fs::read(&outside).unwrap(), outside_bytes);
}

#[test]
fn knowledge_scan_rejects_a_file_that_grows_after_open_metadata() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let entry = kb.repo().join(&written.knowledge_path);
    let initial_len = fs::metadata(&entry).unwrap().len();
    let mut observer = GrowKnowledgeEntryObserver {
        entry,
        grown: false,
    };

    let error = search_knowledge_with_scan(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: String::new(),
            kind: None,
            tag: None,
        },
        knowledge_scan_limits(initial_len + 32, 5_000),
        &IncrementingElapsedClock::new(),
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "knowledge_scan_limit");
}

#[test]
fn search_is_bounded_by_a_maximum_entry_count() {
    let kb = knowledge_fixture();
    let knowledge_dir = kb.repo().join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    for index in 0..1025 {
        fs::write(knowledge_dir.join(format!("note-{index:05}.md")), b"noise").unwrap();
    }
    let err = search_knowledge(
        kb.repo(),
        &KnowledgeSearchQuery {
            term: "anything".into(),
            kind: None,
            tag: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "knowledge_scan_limit");
}

#[test]
fn check_passes_a_clean_knowledge_note() {
    let kb = knowledge_fixture();
    kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();
    assert!(!report.has_code("review_invalid"));
    assert!(!report.has_code("revision_mismatch"));
    assert!(!report.has_code("relation_missing"));
    assert!(!report.has_code("knowledge_invalid"));
    assert!(!report.has_code("concept_id_invalid"));
}

#[test]
fn check_reports_review_state_inconsistency() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&w.knowledge_path);
    let corrupted = fs::read_to_string(&path)
        .unwrap()
        .replace("status: unreviewed", "status: reviewed");
    fs::write(&path, corrupted).unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();
    assert!(report.has_code("review_invalid"));
}

#[test]
fn check_reports_a_dangling_asset_relation() {
    let kb = knowledge_fixture();
    let w = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&w.knowledge_path);
    let dangling_asset_id = format!("personal-asset-{}", "0".repeat(64));
    let corrupted = fs::read_to_string(&path)
        .unwrap()
        .replace(kb.asset_id(), &dangling_asset_id);
    fs::write(&path, corrupted).unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();
    assert!(report.has_code("relation_missing"));
}

#[test]
fn check_rejects_noncanonical_knowledge_id() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&written.knowledge_path);
    let corrupted = fs::read_to_string(&path).unwrap().replace(
        &written.knowledge_id,
        &format!("personal-knowledge-{}", "0".repeat(64)),
    );
    fs::write(&path, corrupted).unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();

    assert!(report.has_code("knowledge_invalid"));
}

#[test]
fn check_rejects_noncanonical_knowledge_path() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    fs::rename(
        kb.repo().join(&written.knowledge_path),
        kb.repo().join("knowledge/not-canonical.md"),
    )
    .unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();

    assert!(report.has_code("knowledge_invalid"));
}

#[test]
fn check_rejects_noncanonical_knowledge_generation_and_fingerprint() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&written.knowledge_path);
    let corrupted = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "processor_version: knowledge-v1",
            "processor_version: future-v9",
        )
        .replace("asset_fingerprint: sha256:", "asset_fingerprint: sha256:00");
    fs::write(&path, corrupted).unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();

    assert!(report.has_code("knowledge_invalid"));
}

#[test]
fn check_rejects_noncanonical_concept_ids_and_content() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    let path = kb.repo().join(&written.knowledge_path);
    let corrupted = fs::read_to_string(&path)
        .unwrap()
        .replace("id: convolution", "id: wrong-id")
        .replace("name: Convolution", "name: ''");
    fs::write(&path, corrupted).unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();

    assert!(report.has_code("concept_id_invalid"));
    assert!(report.has_code("concept_invalid"));
}

#[test]
fn check_rejects_more_than_one_knowledge_note_for_an_asset() {
    let kb = knowledge_fixture();
    let written = kb.write(kb.asset_id(), VALID.as_bytes(), false).unwrap();
    fs::copy(
        kb.repo().join(&written.knowledge_path),
        kb.repo().join("knowledge/duplicate.md"),
    )
    .unwrap();

    let report = check_repository(CheckRequest::new(kb.repo())).unwrap();

    assert!(report.has_code("relation_conflict"));
}
