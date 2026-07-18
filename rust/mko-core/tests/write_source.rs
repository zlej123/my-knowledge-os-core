mod support;

use std::{fs, path::PathBuf};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    front_matter::{parse_markdown, render_markdown},
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    prepare::{PrepareRequest, PreparedSourceBundle, prepare_source_with_extractor},
    registry::{CaptureRequest, capture_asset, lineage_repair_needed, read_asset},
    source::{WriteSourceRequest, parse_semantic_response, write_source_draft_with_clock},
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

struct TestEnv {
    _root: TempDir,
    repository: PathBuf,
    asset_id: String,
    bundle: PreparedSourceBundle,
    clock: FixedClock,
}

impl TestEnv {
    fn prepared() -> Self {
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
        let output = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        let bundle = prepare_source_with_extractor(
            PrepareRequest::new(&repository, &asset_id, output).with_local_config(&local_config),
            |_, _| Ok(vec!["Fixture page".into()]),
        )
        .unwrap();
        Self {
            _root: root,
            repository,
            asset_id,
            bundle,
            clock,
        }
    }

    fn write(
        &self,
        response: &[u8],
        replace_pending: bool,
    ) -> Result<mko_core::source::WriteSourceResult, mko_core::error::MkoError> {
        write_source_draft_with_clock(
            WriteSourceRequest::new(&self.repository, self.bundle.clone(), response.to_vec())
                .with_replace_pending(replace_pending),
            &self.clock,
        )
    }

    fn write_with_slug(
        &self,
        response: &[u8],
        slug: &str,
    ) -> Result<mko_core::source::WriteSourceResult, mko_core::error::MkoError> {
        write_source_draft_with_clock(
            WriteSourceRequest::new(&self.repository, self.bundle.clone(), response.to_vec())
                .with_slug(Some(slug.into())),
            &self.clock,
        )
    }

    fn source_path(&self) -> PathBuf {
        fs::read_dir(self.repository.join("sources"))
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().and_then(|value| value.to_str()) == Some("md")).then_some(path)
            })
            .unwrap()
    }
}

fn golden_response() -> Vec<u8> {
    include_bytes!("../../../tests/fixtures/semantic-response.json").to_vec()
}

#[test]
fn rejects_unknown_protected_and_missing_fields() {
    let protected = br#"{"id":"evil","title":"Paper","sections":{}}"#;
    assert_eq!(
        parse_semantic_response(protected).unwrap_err().code(),
        "schema_invalid"
    );
    let unknown = br#"{"title":"Paper","unknown":"value"}"#;
    assert_eq!(
        parse_semantic_response(unknown).unwrap_err().code(),
        "schema_invalid"
    );
    let missing = br#"{"title":"Paper"}"#;
    assert_eq!(
        parse_semantic_response(missing).unwrap_err().code(),
        "schema_invalid"
    );
}

#[test]
fn enforces_raw_and_per_section_limits() {
    let oversized_raw = vec![b' '; 1024 * 1024 + 1];
    assert_eq!(
        parse_semantic_response(&oversized_raw).unwrap_err().code(),
        "schema_invalid"
    );

    let mut value: serde_json::Value = serde_json::from_slice(&golden_response()).unwrap();
    value["problem"] = serde_json::Value::String("x".repeat(64 * 1024 + 1));
    assert_eq!(
        parse_semantic_response(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code(),
        "schema_invalid"
    );
}

#[test]
fn writes_canonical_source_and_keeps_relation_only_on_source() {
    let env = TestEnv::prepared();

    let result = env.write(&golden_response(), false).unwrap();
    let source_text = fs::read_to_string(env.repository.join(&result.source_path)).unwrap();
    let source = parse_markdown::<SourceRecord>(&source_text).unwrap();

    assert_eq!(source.metadata.id, env.bundle.source_id);
    assert_eq!(
        source.metadata.relations.asset_ids,
        vec![env.asset_id.clone()]
    );
    assert_eq!(source.metadata.status, SourceStatus::ReviewPending);
    assert_eq!(source.metadata.review.status, ReviewStatus::Pending);
    assert_eq!(source.metadata.content_revision, result.content_revision);
    assert!(!source_text.contains('\r'));
    let headings = source
        .body
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        vec![
            "# Paper",
            "## Source Metadata",
            "## One-Sentence Summary",
            "## Problem",
            "## Method",
            "## Contributions",
            "## Reported Evidence",
            "## Stated Limitations",
            "## Domain Perspective",
            "## Implementation Considerations",
            "## Questions and Unknowns",
            "## Related Knowledge",
        ]
    );
    assert!(result.source_path.starts_with("sources/2026-07-18-paper-"));
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
    assert!(
        !fs::read_to_string(
            env.repository
                .join("assets/registry")
                .join(format!("{}.md", env.asset_id))
        )
        .unwrap()
        .contains("source_ids")
    );
}

#[test]
fn semantic_text_cannot_inject_canonical_markdown_headings() {
    let env = TestEnv::prepared();
    let mut response: serde_json::Value = serde_json::from_slice(&golden_response()).unwrap();
    response["problem"] =
        serde_json::Value::String("Problem text\n\n## Injected Heading\n\n# Injected Title".into());

    let result = env
        .write(&serde_json::to_vec(&response).unwrap(), false)
        .unwrap();
    let input = fs::read_to_string(env.repository.join(result.source_path)).unwrap();
    let document = parse_markdown::<SourceRecord>(&input).unwrap();
    let headings = document
        .body
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect::<Vec<_>>();

    assert_eq!(headings.len(), 12);
    assert!(!headings.contains(&"## Injected Heading"));
    assert!(!headings.contains(&"# Injected Title"));
    assert!(document.body.contains("\\## Injected Heading"));
    assert!(document.body.contains("\\# Injected Title"));
}

#[test]
fn rejects_multiline_titles_that_cannot_form_one_canonical_heading() {
    let mut response: serde_json::Value = serde_json::from_slice(&golden_response()).unwrap();
    response["title"] = serde_json::Value::String("Paper\n## Injected".into());

    let error = parse_semantic_response(&serde_json::to_vec(&response).unwrap()).unwrap_err();

    assert_eq!(error.code(), "schema_invalid");
}

#[test]
fn automatic_slug_falls_back_for_windows_reserved_titles() {
    let env = TestEnv::prepared();
    let mut response: serde_json::Value = serde_json::from_slice(&golden_response()).unwrap();
    response["title"] = serde_json::Value::String("CON".into());

    let result = env
        .write(&serde_json::to_vec(&response).unwrap(), false)
        .unwrap();

    assert!(result.source_path.contains("-document-"));
}

#[test]
fn rejects_an_overlong_requested_slug_before_creating_a_source() {
    let env = TestEnv::prepared();

    let error = env
        .write_with_slug(&golden_response(), &"a".repeat(97))
        .unwrap_err();

    assert_eq!(error.code(), "invalid_slug");
    assert!(!env.repository.join("sources").exists());
}

#[test]
fn pending_source_requires_explicit_replacement_and_keeps_id_and_path() {
    let env = TestEnv::prepared();
    let original = env.write(&golden_response(), false).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&golden_response()).unwrap();
    changed["problem"] = serde_json::Value::String("Changed problem".into());
    let changed = serde_json::to_vec(&changed).unwrap();

    let error = env.write(&changed, false).unwrap_err();
    assert_eq!(error.code(), "replace_pending_required");

    let replaced = env.write(&changed, true).unwrap();
    assert_eq!(replaced.source_id, original.source_id);
    assert_eq!(replaced.source_path, original.source_path);
    assert_ne!(replaced.content_revision, original.content_revision);
}

#[test]
fn identical_pending_source_is_reused_without_replacement_flag() {
    let env = TestEnv::prepared();
    let original = env.write(&golden_response(), false).unwrap();

    let existing = env.write(&golden_response(), false).unwrap();

    assert_eq!(existing.result, "existing");
    assert_eq!(existing.source_path, original.source_path);
}

#[test]
fn approved_source_is_never_overwritten() {
    let env = TestEnv::prepared();
    env.write(&golden_response(), false).unwrap();
    let path = env.source_path();
    let input = fs::read_to_string(&path).unwrap();
    let mut document = parse_markdown::<SourceRecord>(&input).unwrap();
    document.metadata.status = SourceStatus::Approved;
    document.metadata.review.status = ReviewStatus::Approved;
    document.metadata.review.approved_revision = Some(document.metadata.content_revision.clone());
    fs::write(
        &path,
        render_markdown(&document.metadata, &document.body).unwrap(),
    )
    .unwrap();

    let error = env.write(&golden_response(), true).unwrap_err();

    assert_eq!(error.code(), "approved_source_immutable");
}

#[test]
fn preserves_source_and_reports_mismatch_when_asset_transition_fails() {
    let env = TestEnv::prepared();
    let registry_lock = env
        .repository
        .join("assets/registry")
        .join(format!(".{}.md.publish.lock", env.asset_id));
    fs::write(&registry_lock, b"interrupt transition").unwrap();

    let error = env.write(&golden_response(), false).unwrap_err();

    assert_eq!(error.code(), "registry_locked");
    assert!(env.source_path().is_file());
    fs::remove_file(registry_lock).unwrap();
    assert!(
        lineage_repair_needed(&env.repository)
            .unwrap()
            .contains(&env.asset_id)
    );
}

#[cfg(unix)]
#[test]
fn rejects_a_sources_symlink_without_writing_outside_the_repository() {
    let env = TestEnv::prepared();
    let outside = env._root.path().join("outside-sources");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, env.repository.join("sources")).unwrap();

    let error = env.write(&golden_response(), false).unwrap_err();

    assert_eq!(error.code(), "source_path_invalid");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}
