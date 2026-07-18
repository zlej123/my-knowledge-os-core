mod support;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use mko_core::{
    approve::{
        ApprovalObserver, ApprovalTerminal, ApproveSourceRequest,
        approve_source_with_terminal_and_clock, approve_source_with_terminal_clock_and_observer,
    },
    check::{CheckRequest, check_repository},
    clock::Clock,
    front_matter::{parse_markdown, render_markdown},
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    prepare::{PrepareRequest, prepare_source_with_extractor},
    registry::{CaptureRequest, capture_asset, read_asset},
    revision::calculate_source_revision,
    source::{
        RepairSourceStateRequest, WriteSourceRequest, repair_source_state_with_clock,
        write_source_draft_with_clock,
    },
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
    source_id: String,
    source_path: PathBuf,
    clock: FixedClock,
}

impl TestEnv {
    fn pending_source() -> Self {
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
            PrepareRequest::new(&repository, &asset_id, &bundle_path)
                .with_local_config(&local_config),
            |_, _| Ok(vec!["Fixture page".into()]),
        )
        .unwrap();
        let result = write_source_draft_with_clock(
            WriteSourceRequest::new(
                &repository,
                &bundle_path,
                include_bytes!("../../../tests/fixtures/semantic-response.json").to_vec(),
            ),
            &clock,
        )
        .unwrap();
        Self {
            _root: root,
            source_path: repository.join(&result.source_path),
            repository,
            asset_id,
            source_id: result.source_id,
            clock,
        }
    }

    fn approve_fixture_without_asset_transition(&self) {
        let input = fs::read_to_string(&self.source_path).unwrap();
        let parsed = parse_markdown::<SourceRecord>(&input).unwrap();
        let mut source = parsed.metadata;
        let revision = calculate_source_revision(&source, &parsed.body).unwrap();
        source.status = SourceStatus::Approved;
        source.content_revision = revision.clone();
        source.review.status = ReviewStatus::Approved;
        source.review.approved_revision = Some(revision);
        source.review.reviewed_at = Some(self.clock.now_utc());
        fs::write(
            &self.source_path,
            render_markdown(&source, &parsed.body).unwrap(),
        )
        .unwrap();
    }

    fn check(&self) -> mko_core::check::CheckReport {
        check_repository(CheckRequest::new(&self.repository)).unwrap()
    }
}

#[derive(Default)]
struct FakeTerminal {
    stdin_tty: bool,
    stdout_tty: bool,
    input: String,
    output: String,
}

struct MutatingTerminal {
    input: String,
    output: String,
    source_path: PathBuf,
    replacement: Vec<u8>,
}

struct PublicationMutator {
    source_path: PathBuf,
    replacement: Vec<u8>,
}

#[cfg(unix)]
struct PublicationSymlinkSwap {
    source_path: PathBuf,
    outside_path: PathBuf,
}

#[cfg(unix)]
impl ApprovalObserver for PublicationSymlinkSwap {
    fn before_publication(&mut self) -> io::Result<()> {
        fs::remove_file(&self.source_path)?;
        std::os::unix::fs::symlink(&self.outside_path, &self.source_path)
    }
}

impl ApprovalObserver for PublicationMutator {
    fn before_publication(&mut self) -> io::Result<()> {
        fs::write(&self.source_path, &self.replacement)
    }
}

impl ApprovalTerminal for MutatingTerminal {
    fn stdin_is_terminal(&self) -> bool {
        true
    }

    fn stdout_is_terminal(&self) -> bool {
        true
    }

    fn write_all(&mut self, text: &str) -> io::Result<()> {
        self.output.push_str(text);
        Ok(())
    }

    fn read_line(&mut self, output: &mut String) -> io::Result<usize> {
        fs::write(&self.source_path, &self.replacement)?;
        output.push_str(&self.input);
        Ok(self.input.len())
    }
}

impl ApprovalTerminal for FakeTerminal {
    fn stdin_is_terminal(&self) -> bool {
        self.stdin_tty
    }

    fn stdout_is_terminal(&self) -> bool {
        self.stdout_tty
    }

    fn write_all(&mut self, text: &str) -> io::Result<()> {
        self.output.push_str(text);
        Ok(())
    }

    fn read_line(&mut self, output: &mut String) -> io::Result<usize> {
        output.push_str(&self.input);
        Ok(self.input.len())
    }
}

#[test]
fn recomputes_revision_instead_of_trusting_saved_value() {
    let env = TestEnv::pending_source();
    env.approve_fixture_without_asset_transition();
    let input = fs::read_to_string(&env.source_path).unwrap();
    let parsed = parse_markdown::<SourceRecord>(&input).unwrap();
    fs::write(
        &env.source_path,
        render_markdown(&parsed.metadata, "# Changed\n").unwrap(),
    )
    .unwrap();

    let report = env.check();

    assert!(report.has_code("revision_mismatch"));
    assert!(report.has_code("approval_stale"));
}

#[test]
fn secret_pattern_blocks_check_without_echoing_the_secret() {
    let env = TestEnv::pending_source();
    let secret = "sk-test-12345678901234567890";
    fs::create_dir_all(env.repository.join("notes")).unwrap();
    fs::write(
        env.repository.join("notes/credential.md"),
        format!("token = {secret}"),
    )
    .unwrap();

    let report = env.check();
    let serialized = serde_json::to_string(&report).unwrap();

    assert!(report.has_code("secret_detected"));
    assert!(serialized.contains("notes/credential.md"));
    assert!(!serialized.contains(secret));
}

#[test]
fn non_tty_approval_is_rejected() {
    let env = TestEnv::pending_source();
    let mut terminal = FakeTerminal {
        input: format!("APPROVE {}\n", env.source_id),
        ..FakeTerminal::default()
    };

    let error = approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "human_confirmation_required");
}

#[test]
fn exact_confirmation_approves_current_revision_then_processes_asset() {
    let env = TestEnv::pending_source();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: format!("APPROVE {}\n", env.source_id),
        output: String::new(),
    };

    let result = approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap();

    let parsed = parse_markdown::<SourceRecord>(&fs::read_to_string(&env.source_path).unwrap())
        .unwrap()
        .metadata;
    assert_eq!(result.source_id, env.source_id);
    assert_eq!(parsed.status, SourceStatus::Approved);
    assert_eq!(parsed.review.approved_revision, Some(result.revision));
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::Processed
    );
    assert!(terminal.output.contains(&env.source_id));
    assert_eq!(
        repair_source_state_with_clock(
            RepairSourceStateRequest::new(&env.repository, &env.asset_id),
            &env.clock,
        )
        .unwrap()
        .result,
        "already_consistent"
    );
}

#[test]
fn incorrect_confirmation_does_not_publish_or_process() {
    let env = TestEnv::pending_source();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: "yes\n".into(),
        output: String::new(),
    };

    let error = approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "human_confirmation_required");
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
    assert_eq!(
        parse_markdown::<SourceRecord>(&fs::read_to_string(&env.source_path).unwrap())
            .unwrap()
            .metadata
            .status,
        SourceStatus::ReviewPending
    );
}

#[test]
fn source_changed_after_prompt_is_not_overwritten_or_approved() {
    let env = TestEnv::pending_source();
    let replacement = b"external edit made while approval prompt was open\n".to_vec();
    let mut terminal = MutatingTerminal {
        input: format!("APPROVE {}\n", env.source_id),
        output: String::new(),
        source_path: env.source_path.clone(),
        replacement: replacement.clone(),
    };

    let error = approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "source_changed_during_approval");
    assert_eq!(fs::read(&env.source_path).unwrap(), replacement);
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
}

#[test]
fn approval_rejects_a_revision_consistent_but_noncanonical_source() {
    let env = TestEnv::pending_source();
    let input = fs::read_to_string(&env.source_path).unwrap();
    let parsed = parse_markdown::<SourceRecord>(&input).unwrap();
    let mut source = parsed.metadata;
    source.ai_assisted = false;
    source.content_revision = calculate_source_revision(&source, &parsed.body).unwrap();
    fs::write(
        &env.source_path,
        render_markdown(&source, &parsed.body).unwrap(),
    )
    .unwrap();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: format!("APPROVE {}\n", env.source_id),
        output: String::new(),
    };

    let error = approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "source_invalid");
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
}

#[test]
fn approval_prompt_includes_deterministic_diff_summary() {
    let env = TestEnv::pending_source();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: "decline\n".into(),
        output: String::new(),
    };

    approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap_err();

    assert!(
        terminal
            .output
            .contains("Status: review_pending -> approved")
    );
    assert!(terminal.output.contains("Source bytes:"));
    assert!(terminal.output.contains("Source lines:"));
    assert!(terminal.output.contains("Git diff:"));
}

#[test]
fn source_changed_immediately_before_publication_is_not_overwritten() {
    let env = TestEnv::pending_source();
    let replacement = b"external edit immediately before publication\n".to_vec();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: format!("APPROVE {}\n", env.source_id),
        output: String::new(),
    };
    let mut observer = PublicationMutator {
        source_path: env.source_path.clone(),
        replacement: replacement.clone(),
    };

    let error = approve_source_with_terminal_clock_and_observer(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "source_changed_during_approval");
    assert_eq!(fs::read(&env.source_path).unwrap(), replacement);
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
}

#[cfg(unix)]
#[test]
fn final_publication_rejects_a_source_symlink_swap_without_touching_outside() {
    let env = TestEnv::pending_source();
    let original = fs::read(&env.source_path).unwrap();
    let outside = env._root.path().join("outside-source.md");
    fs::write(&outside, &original).unwrap();
    let mut terminal = FakeTerminal {
        stdin_tty: true,
        stdout_tty: true,
        input: format!("APPROVE {}\n", env.source_id),
        output: String::new(),
    };
    let mut observer = PublicationSymlinkSwap {
        source_path: env.source_path.clone(),
        outside_path: outside.clone(),
    };

    let error = approve_source_with_terminal_clock_and_observer(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
        &mut observer,
    )
    .unwrap_err();

    assert_eq!(error.code(), "source_changed_during_approval");
    assert_eq!(fs::read(outside).unwrap(), original);
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
}

#[test]
fn check_rejects_a_revision_consistent_noncanonical_body_shape() {
    let env = TestEnv::pending_source();
    let input = fs::read_to_string(&env.source_path).unwrap();
    let parsed = parse_markdown::<SourceRecord>(&input).unwrap();
    let body = parsed.body.replace("\n\n## Related Knowledge\n\n", "\n\n");
    let mut source = parsed.metadata;
    source.content_revision = calculate_source_revision(&source, &body).unwrap();
    fs::write(&env.source_path, render_markdown(&source, &body).unwrap()).unwrap();

    let report = env.check();

    assert!(report.has_code("source_invalid"));
}

#[test]
fn working_tree_and_staged_checks_apply_full_portability_rules() {
    let env = TestEnv::pending_source();
    fs::create_dir_all(env.repository.join("notes")).unwrap();
    fs::write(env.repository.join("notes/CON.txt"), "reserved\n").unwrap();
    fs::write(env.repository.join("notes/trailing."), "trailing\n").unwrap();
    fs::write(env.repository.join("notes/forbidden:name.md"), "colon\n").unwrap();
    let long_path = format!("notes/{}/{}/long.md", "a".repeat(120), "b".repeat(120));
    fs::create_dir_all(env.repository.join(Path::new(&long_path).parent().unwrap())).unwrap();
    fs::write(env.repository.join(&long_path), "long\n").unwrap();

    let working = env.check();
    assert!(
        working
            .issues
            .iter()
            .filter(|issue| issue.code == "path_not_portable")
            .count()
            >= 4
    );

    git(&env.repository, &["init"]);
    git(&env.repository, &["add", "."]);
    let staged = check_repository(CheckRequest::new(&env.repository).with_staged(true)).unwrap();
    assert!(
        staged
            .issues
            .iter()
            .filter(|issue| issue.code == "path_not_portable")
            .count()
            >= 4
    );
}

#[test]
fn stable_asset_state_rejects_a_manipulated_recovery_checkpoint() {
    let env = TestEnv::pending_source();
    let path = env
        .repository
        .join("assets/registry")
        .join(format!("{}.md", env.asset_id));
    let input = fs::read_to_string(&path).unwrap();
    let parsed = parse_markdown::<mko_core::model::AssetRecord>(&input).unwrap();
    let mut asset = parsed.metadata;
    asset.durable_state_history = vec![AssetStatus::Registered];
    fs::write(&path, render_markdown(&asset, &parsed.body).unwrap()).unwrap();

    assert!(env.check().has_code("invalid_state_transition"));
}

#[test]
fn failed_asset_rejects_an_impossible_nested_checkpoint_history() {
    let env = TestEnv::pending_source();
    let path = env
        .repository
        .join("assets/registry")
        .join(format!("{}.md", env.asset_id));
    let input = fs::read_to_string(&path).unwrap();
    let parsed = parse_markdown::<mko_core::model::AssetRecord>(&input).unwrap();
    let mut asset = parsed.metadata;
    asset.asset_status = AssetStatus::Failed;
    asset.durable_state_history = vec![AssetStatus::ReviewPending, AssetStatus::Registered];
    fs::write(&path, render_markdown(&asset, &parsed.body).unwrap()).unwrap();

    assert!(env.check().has_code("invalid_state_transition"));
}

#[cfg(unix)]
#[test]
fn runtime_lock_scan_rejects_an_intermediate_symlink_without_listing_outside() {
    let env = TestEnv::pending_source();
    let outside = env._root.path().join("outside-runtime");
    fs::create_dir_all(outside.join("locks")).unwrap();
    fs::write(
        outside.join("locks/external-secret-name.lock"),
        "secret lock\n",
    )
    .unwrap();
    fs::create_dir_all(env.repository.join(".knowledge-os")).unwrap();
    fs::remove_dir_all(env.repository.join(".knowledge-os/runtime")).unwrap();
    std::os::unix::fs::symlink(&outside, env.repository.join(".knowledge-os/runtime")).unwrap();

    let report = env.check();
    let serialized = serde_json::to_string(&report).unwrap();

    assert!(report.has_code("runtime_path_invalid"));
    assert!(!serialized.contains("external-secret-name"));
}

#[test]
fn auth_configuration_filenames_are_secret_findings() {
    let env = TestEnv::pending_source();
    for name in [".netrc", ".npmrc", ".pypirc"] {
        fs::write(env.repository.join(name), "placeholder\n").unwrap();
    }

    let report = env.check();
    for name in [".netrc", ".npmrc", ".pypirc"] {
        assert!(report.issues.iter().any(|issue| {
            issue.code == "secret_detected" && issue.path.as_deref() == Some(name)
        }));
    }
}

#[test]
fn source_state_mismatch_has_repo_scoped_repair_action() {
    let env = TestEnv::pending_source();
    env.approve_fixture_without_asset_transition();

    let report = env.check();
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "source_state_mismatch")
        .unwrap();

    assert!(issue.safe_action.as_deref().unwrap().contains("--repo"));
    assert!(issue.safe_action.as_deref().unwrap().contains("--asset-id"));
}

#[test]
fn staged_check_reads_index_content_not_the_working_tree() {
    let env = TestEnv::pending_source();
    git(&env.repository, &["init"]);
    git(&env.repository, &["add", "."]);
    let note = env.repository.join("notes/staged.md");
    fs::create_dir_all(note.parent().unwrap()).unwrap();
    fs::write(&note, "safe staged content\n").unwrap();
    git(&env.repository, &["add", "notes/staged.md"]);
    fs::write(&note, "Bearer secret-secret-secret-secret\n").unwrap();

    let report = check_repository(CheckRequest::new(&env.repository).with_staged(true)).unwrap();

    assert!(!report.issues.iter().any(|issue| {
        issue.code == "secret_detected" && issue.path.as_deref() == Some("notes/staged.md")
    }));
}

#[test]
fn staged_check_handles_deleted_entries_without_reading_restored_worktree_content() {
    let env = TestEnv::pending_source();
    git(&env.repository, &["init"]);
    let note = env.repository.join("notes/deleted.md");
    fs::create_dir_all(note.parent().unwrap()).unwrap();
    fs::write(&note, "safe\n").unwrap();
    git(&env.repository, &["add", "."]);
    fs::remove_file(&note).unwrap();
    git(&env.repository, &["add", "-u"]);
    fs::write(&note, "Bearer restored-but-not-staged-secret\n").unwrap();

    let report = check_repository(CheckRequest::new(&env.repository).with_staged(true)).unwrap();

    assert!(!report.issues.iter().any(|issue| {
        issue.code == "secret_detected" && issue.path.as_deref() == Some("notes/deleted.md")
    }));
}

#[test]
fn staged_source_deletion_reports_the_orphaned_review_pending_asset() {
    let env = TestEnv::pending_source();
    git(&env.repository, &["init"]);
    git(&env.repository, &["add", "."]);
    let original = fs::read(&env.source_path).unwrap();
    fs::remove_file(&env.source_path).unwrap();
    git(&env.repository, &["add", "-u"]);
    fs::write(&env.source_path, original).unwrap();

    let report = check_repository(CheckRequest::new(&env.repository).with_staged(true)).unwrap();
    let registry_path = format!("assets/registry/{}.md", env.asset_id);

    assert!(report.issues.iter().any(|issue| {
        issue.code == "relation_missing" && issue.path.as_deref() == Some(registry_path.as_str())
    }));
}

#[test]
fn staged_check_reports_unmerged_entries_without_reading_a_worktree_fallback() {
    let env = TestEnv::pending_source();
    git(&env.repository, &["init"]);
    let conflict = env.repository.join("notes/conflict.md");
    fs::create_dir_all(conflict.parent().unwrap()).unwrap();
    fs::write(&conflict, "safe indexed blob\n").unwrap();
    let oid_output = Command::new("git")
        .arg("-C")
        .arg(&env.repository)
        .args(["hash-object", "-w", "notes/conflict.md"])
        .output()
        .unwrap();
    assert!(oid_output.status.success());
    let oid = String::from_utf8(oid_output.stdout).unwrap();
    let oid = oid.trim();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&env.repository)
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let input = child.stdin.as_mut().unwrap();
        for stage in 1..=3 {
            writeln!(input, "100644 {oid} {stage}\tnotes/conflict.md").unwrap();
        }
    }
    assert!(child.wait().unwrap().success());
    fs::write(&conflict, "Bearer worktree-fallback-must-not-be-read\n").unwrap();

    let report = check_repository(CheckRequest::new(&env.repository).with_staged(true)).unwrap();
    let serialized = serde_json::to_string(&report).unwrap();

    assert!(report.issues.iter().any(|issue| {
        issue.code == "git_conflict" && issue.path.as_deref() == Some("notes/conflict.md")
    }));
    assert!(!serialized.contains("worktree-fallback-must-not-be-read"));
}

#[cfg(unix)]
#[test]
fn repository_check_rejects_symlinks_instead_of_following_them() {
    let env = TestEnv::pending_source();
    let outside = env._root.path().join("outside-secret.md");
    fs::write(&outside, "Bearer do-not-read-this-secret-value\n").unwrap();
    std::os::unix::fs::symlink(&outside, env.repository.join("linked.md")).unwrap();

    let report = env.check();
    let serialized = serde_json::to_string(&report).unwrap();

    assert!(report.has_code("symlink_not_allowed"));
    assert!(!serialized.contains("do-not-read-this-secret-value"));
}

#[test]
fn repository_check_bounds_individual_file_input() {
    let env = TestEnv::pending_source();
    let large = env.repository.join("notes/large.md");
    fs::create_dir_all(large.parent().unwrap()).unwrap();
    fs::write(&large, vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();

    let report = env.check();

    assert!(report.issues.iter().any(|issue| {
        issue.code == "check_input_too_large" && issue.path.as_deref() == Some("notes/large.md")
    }));
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git command failed: {arguments:?}");
}
