mod support;

use std::{
    cell::RefCell,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use chrono::{DateTime, Utc};
use mko_core::{
    approve::{
        ApprovalObserver, ApprovalTerminal, ApproveSourceRequest, GitSnapshot, GitSnapshotProvider,
        approve_source_with_terminal_and_clock,
    },
    clock::Clock,
    front_matter::{parse_markdown, render_markdown},
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    prepare::{PrepareRequest, prepare_source_with_extractor},
    registry::{CaptureRequest, capture_asset, read_asset},
    review::{
        ReviewOutcome, ReviewTerminal, SystemGitSnapshotProvider, list_pending_sources,
        review_and_approve,
    },
    revision::calculate_source_revision,
    source::{WriteSourceRequest, write_source_draft_with_clock},
};
use tempfile::TempDir;

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct Env {
    _root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
    local_config: PathBuf,
    source_path: PathBuf,
    asset_path: PathBuf,
    source_id: String,
    asset_id: String,
    revision: String,
    clock: FixedClock,
}

impl Env {
    fn new() -> Self {
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
            DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
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
        let bundle = repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        prepare_source_with_extractor(
            PrepareRequest::new(&repository, &asset_id, &bundle).with_local_config(&local_config),
            |_, _| Ok(vec!["Fixture page".into()]),
        )
        .unwrap();
        let draft = write_source_draft_with_clock(
            WriteSourceRequest::new(
                &repository,
                &bundle,
                include_bytes!("../../../tests/fixtures/semantic-response.json").to_vec(),
            ),
            &clock,
        )
        .unwrap();
        let source_path = repository.join(&draft.source_path);
        Self {
            _root: root,
            asset_path: repository
                .join("assets/registry")
                .join(format!("{asset_id}.md")),
            repository,
            provider,
            local_config,
            source_path,
            source_id: draft.source_id,
            asset_id,
            revision: draft.content_revision,
            clock,
        }
    }

    fn add_pending(&self, filename: &str, bytes: &[u8]) -> (String, PathBuf, String) {
        let pdf = self.provider.join(filename);
        fs::write(&pdf, bytes).unwrap();
        let asset_id = capture_asset(
            CaptureRequest::new(&self.repository, &pdf)
                .with_local_config(&self.local_config)
                .with_captured_at(self.clock.now_utc()),
        )
        .unwrap()
        .asset_id;
        let bundle = self
            .repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"));
        prepare_source_with_extractor(
            PrepareRequest::new(&self.repository, &asset_id, &bundle)
                .with_local_config(&self.local_config),
            |_, _| Ok(vec!["Second fixture page".into()]),
        )
        .unwrap();
        let draft = write_source_draft_with_clock(
            WriteSourceRequest::new(
                &self.repository,
                &bundle,
                include_bytes!("../../../tests/fixtures/semantic-response.json").to_vec(),
            ),
            &self.clock,
        )
        .unwrap();
        (
            draft.source_id,
            self.repository.join(draft.source_path),
            draft.content_revision,
        )
    }

    fn terminal(&self, approval: &str) -> ScriptedTerminal {
        ScriptedTerminal::new(vec!["1\n".into(), format!("{approval}\n")])
    }

    fn assert_pending(&self) {
        let source =
            parse_markdown::<SourceRecord>(&fs::read_to_string(&self.source_path).unwrap())
                .unwrap()
                .metadata;
        assert_eq!(source.status, SourceStatus::ReviewPending);
        assert_eq!(source.review.status, ReviewStatus::Pending);
        assert_eq!(
            read_asset(&self.repository, &self.asset_id)
                .unwrap()
                .asset_status,
            AssetStatus::ReviewPending
        );
    }
}

struct ScriptedTerminal {
    stdin_tty: bool,
    stdout_tty: bool,
    lines: Vec<String>,
    read_count: usize,
    output: String,
    on_read: Option<Box<dyn FnMut(usize) -> io::Result<()>>>,
}

impl ScriptedTerminal {
    fn new(lines: Vec<String>) -> Self {
        Self {
            stdin_tty: true,
            stdout_tty: true,
            lines,
            read_count: 0,
            output: String::new(),
            on_read: None,
        }
    }
}

impl ReviewTerminal for ScriptedTerminal {
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
        self.read_count += 1;
        if let Some(action) = &mut self.on_read {
            action(self.read_count)?;
        }
        let line = self
            .lines
            .get(self.read_count - 1)
            .cloned()
            .unwrap_or_default();
        output.push_str(&line);
        Ok(line.len())
    }
}

impl ApprovalTerminal for ScriptedTerminal {
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
        self.read_count += 1;
        if let Some(action) = &mut self.on_read {
            action(self.read_count)?;
        }
        let line = self
            .lines
            .get(self.read_count - 1)
            .cloned()
            .unwrap_or_default();
        output.push_str(&line);
        Ok(line.len())
    }
}

#[derive(Clone)]
struct FakeGit(Rc<RefCell<GitSnapshot>>);

impl GitSnapshotProvider for FakeGit {
    fn snapshot(
        &self,
        _repository: &Path,
        _source: &Path,
        _asset: &Path,
    ) -> Result<GitSnapshot, mko_core::error::MkoError> {
        Ok(self.0.borrow().clone())
    }
}

struct SequencedGit(RefCell<Vec<GitSnapshot>>);

impl GitSnapshotProvider for SequencedGit {
    fn snapshot(
        &self,
        _repository: &Path,
        _source: &Path,
        _asset: &Path,
    ) -> Result<GitSnapshot, mko_core::error::MkoError> {
        Ok(self.0.borrow_mut().remove(0))
    }
}

struct MutatingObserver(Box<dyn FnMut() -> io::Result<()>>);

impl ApprovalObserver for MutatingObserver {
    fn before_publication(&mut self) -> io::Result<()> {
        (self.0)()
    }
}

fn git() -> FakeGit {
    FakeGit(Rc::new(RefCell::new(GitSnapshot {
        working: b"working diff\n".to_vec(),
        staged: b"staged diff\n".to_vec(),
    })))
}

fn run_git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

#[test]
fn duplicate_titles_are_listed_with_unambiguous_identity_path_and_revision() {
    let env = Env::new();
    let (second_id, second_path, second_revision) =
        env.add_pending("second.pdf", b"%PDF-1.7\nsecond fixture");

    let listed = list_pending_sources(&env.repository).unwrap();

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].title, listed[1].title);
    assert_ne!(listed[0].source_id, listed[1].source_id);
    assert_ne!(listed[0].source_path, listed[1].source_path);
    assert!(listed.iter().any(|item| item.source_id == second_id
        && env.repository.join(&item.source_path) == second_path
        && item.revision == second_revision));
}

#[test]
fn non_tty_is_rejected_before_any_mutation() {
    let env = Env::new();
    let before = fs::read(&env.source_path).unwrap();
    let mut terminal = env.terminal("DEFER");
    terminal.stdin_tty = false;
    let error = review_and_approve(
        &env.repository,
        &mut terminal,
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();
    assert_eq!(error.code(), "human_confirmation_required");
    assert_eq!(fs::read(&env.source_path).unwrap(), before);
    env.assert_pending();
}

#[test]
fn stdout_non_tty_is_rejected_before_repository_access() {
    let missing = PathBuf::from("this-repository-must-not-be-opened");
    let mut terminal = ScriptedTerminal::new(Vec::new());
    terminal.stdout_tty = false;
    let error = review_and_approve(
        &missing,
        &mut terminal,
        &git(),
        &FixedClock(Utc::now()),
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();
    assert_eq!(error.code(), "human_confirmation_required");
}

#[test]
fn approval_token_must_bind_both_source_id_and_revision() {
    for approval in [
        "APPROVE wrong-id sha256:wrong",
        "APPROVE personal-source-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa sha256:wrong",
        "yes",
    ] {
        let env = Env::new();
        let error = review_and_approve(
            &env.repository,
            &mut env.terminal(approval),
            &git(),
            &env.clock,
            &mut MutatingObserver(Box::new(|| Ok(()))),
        )
        .unwrap_err();
        assert_eq!(error.code(), "human_confirmation_required");
        env.assert_pending();
    }
}

#[test]
fn review_display_escapes_terminal_control_and_bidi_characters() {
    let env = Env::new();
    let parsed =
        parse_markdown::<SourceRecord>(&fs::read_to_string(&env.source_path).unwrap()).unwrap();
    let mut source = parsed.metadata;
    let title = "Unsafe \u{1b}[31m \u{202e} title";
    let body = parsed.body.replacen(&source.title, title, 1);
    source.title = title.into();
    source.content_revision = calculate_source_revision(&source, &body).unwrap();
    fs::write(&env.source_path, render_markdown(&source, &body).unwrap()).unwrap();
    let mut terminal = env.terminal("DEFER");

    let outcome = review_and_approve(
        &env.repository,
        &mut terminal,
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap();

    assert_eq!(outcome, ReviewOutcome::Deferred);
    assert!(!terminal.output.contains('\u{1b}'));
    assert!(!terminal.output.contains('\u{202e}'));
    assert!(terminal.output.contains("\\u{1b}"));
    assert!(terminal.output.contains("\\u{202e}"));
}

#[test]
fn defer_has_zero_mutation() {
    let env = Env::new();
    let source = fs::read(&env.source_path).unwrap();
    let asset = fs::read(&env.asset_path).unwrap();
    let outcome = review_and_approve(
        &env.repository,
        &mut env.terminal("DEFER"),
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap();
    assert_eq!(outcome, ReviewOutcome::Deferred);
    assert_eq!(fs::read(&env.source_path).unwrap(), source);
    assert_eq!(fs::read(&env.asset_path).unwrap(), asset);
}

#[test]
fn exact_displayed_snapshot_is_approved_and_uses_one_asset_lock() {
    let env = Env::new();
    let approval = format!("APPROVE {} {}", env.source_id, env.revision);
    let observed = Rc::new(RefCell::new(0usize));
    let observed_in_hook = observed.clone();
    let lock_dir = env.repository.join(".knowledge-os/runtime/locks");
    let mut terminal = env.terminal(&approval);
    terminal.on_read = Some(Box::new(move |read| {
        if read == 2 {
            *observed_in_hook.borrow_mut() = fs::read_dir(&lock_dir)?.count();
        }
        Ok(())
    }));
    let outcome = review_and_approve(
        &env.repository,
        &mut terminal,
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap();
    assert!(matches!(outcome, ReviewOutcome::Approved(_)));
    assert_eq!(*observed.borrow(), 1);
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::Processed
    );
}

#[test]
fn legacy_approval_also_holds_exactly_one_asset_lock() {
    let env = Env::new();
    let observed = Rc::new(RefCell::new(0usize));
    let observed_in_hook = observed.clone();
    let lock_dir = env.repository.join(".knowledge-os/runtime/locks");
    let mut terminal = ScriptedTerminal::new(vec![format!("APPROVE {}\n", env.source_id)]);
    terminal.on_read = Some(Box::new(move |_| {
        *observed_in_hook.borrow_mut() = fs::read_dir(&lock_dir)?.count();
        Ok(())
    }));
    approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(&env.repository, &env.source_id),
        &mut terminal,
        &env.clock,
    )
    .unwrap();
    assert_eq!(*observed.borrow(), 1);
}

#[cfg(unix)]
#[test]
fn review_rejects_a_symlinked_lock_directory_without_writing_outside() {
    let env = Env::new();
    let locks = env.repository.join(".knowledge-os/runtime/locks");
    fs::remove_dir(&locks).unwrap();
    let outside = env._root.path().join("outside-locks");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &locks).unwrap();

    let error = review_and_approve(
        &env.repository,
        &mut env.terminal("DEFER"),
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();

    assert_eq!(error.code(), "lock_write_failed");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    env.assert_pending();
}

#[test]
fn source_changed_after_display_is_rejected() {
    let env = Env::new();
    let mut terminal = env.terminal(&format!("APPROVE {} {}", env.source_id, env.revision));
    let path = env.source_path.clone();
    terminal.on_read = Some(Box::new(move |read| {
        if read == 2 {
            fs::write(&path, "changed after display\n")?;
        }
        Ok(())
    }));
    let error = review_and_approve(
        &env.repository,
        &mut terminal,
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();
    assert_eq!(error.code(), "source_changed_during_approval");
    assert_eq!(
        read_asset(&env.repository, &env.asset_id)
            .unwrap()
            .asset_status,
        AssetStatus::ReviewPending
    );
}

#[test]
fn asset_changed_after_display_is_rejected() {
    let env = Env::new();
    let mut terminal = env.terminal(&format!("APPROVE {} {}", env.source_id, env.revision));
    let path = env.asset_path.clone();
    terminal.on_read = Some(Box::new(move |read| {
        if read == 2 {
            fs::write(&path, "changed asset\n")?;
        }
        Ok(())
    }));
    let error = review_and_approve(
        &env.repository,
        &mut terminal,
        &git(),
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();
    assert_eq!(error.code(), "asset_changed_during_approval");
    let source = fs::read_to_string(&env.source_path).unwrap();
    assert_eq!(
        parse_markdown::<SourceRecord>(&source)
            .unwrap()
            .metadata
            .status,
        SourceStatus::ReviewPending
    );
}

#[test]
fn working_and_staged_diff_changes_after_display_are_rejected() {
    for staged in [false, true] {
        let env = Env::new();
        let git = git();
        let state = git.0.clone();
        let mut terminal = env.terminal(&format!("APPROVE {} {}", env.source_id, env.revision));
        terminal.on_read = Some(Box::new(move |read| {
            if read == 2 {
                if staged {
                    state.borrow_mut().staged = b"changed staged".to_vec();
                } else {
                    state.borrow_mut().working = b"changed working".to_vec();
                }
                Ok(())
            } else {
                Ok(())
            }
        }));
        let error = review_and_approve(
            &env.repository,
            &mut terminal,
            &git,
            &env.clock,
            &mut MutatingObserver(Box::new(|| Ok(()))),
        )
        .unwrap_err();
        assert_eq!(error.code(), "git_snapshot_changed_during_approval");
        env.assert_pending();
    }
}

#[test]
fn incoherent_initial_git_collection_is_never_displayed() {
    let env = Env::new();
    let git = SequencedGit(RefCell::new(vec![
        GitSnapshot {
            working: b"first".to_vec(),
            staged: Vec::new(),
        },
        GitSnapshot {
            working: b"second".to_vec(),
            staged: Vec::new(),
        },
    ]));
    let mut terminal = env.terminal("DEFER");
    let error = review_and_approve(
        &env.repository,
        &mut terminal,
        &git,
        &env.clock,
        &mut MutatingObserver(Box::new(|| Ok(()))),
    )
    .unwrap_err();
    assert_eq!(error.code(), "git_snapshot_unstable");
    assert!(!terminal.output.contains("=== SOURCE"));
    env.assert_pending();
}

#[test]
fn every_snapshot_is_revalidated_immediately_before_publication() {
    for target in ["source", "asset", "working", "staged"] {
        let env = Env::new();
        let git = git();
        let source_path = env.source_path.clone();
        let asset_path = env.asset_path.clone();
        let git_state = git.0.clone();
        let target = target.to_string();
        let target_in_hook = target.clone();
        let mut observer = MutatingObserver(Box::new(move || {
            match target_in_hook.as_str() {
                "source" => fs::write(&source_path, "changed before publication\n")?,
                "asset" => fs::write(&asset_path, "changed before publication\n")?,
                "working" => git_state.borrow_mut().working = b"changed working".to_vec(),
                "staged" => git_state.borrow_mut().staged = b"changed staged".to_vec(),
                _ => unreachable!(),
            }
            Ok(())
        }));
        let approval = format!("APPROVE {} {}", env.source_id, env.revision);
        let error = review_and_approve(
            &env.repository,
            &mut env.terminal(&approval),
            &git,
            &env.clock,
            &mut observer,
        )
        .unwrap_err();
        assert!(matches!(
            error.code(),
            "source_changed_during_approval"
                | "asset_changed_during_approval"
                | "git_snapshot_changed_during_approval"
        ));
        if target != "asset" {
            assert_eq!(
                read_asset(&env.repository, &env.asset_id)
                    .unwrap()
                    .asset_status,
                AssetStatus::ReviewPending
            );
        }
    }
}

#[test]
fn system_git_provider_treats_review_paths_as_pathspecs_after_separator() {
    let root = tempfile::tempdir().unwrap();
    let source = Path::new("sources/-source.md");
    let asset = Path::new("assets/registry/-asset.md");
    fs::create_dir_all(root.path().join("sources")).unwrap();
    fs::create_dir_all(root.path().join("assets/registry")).unwrap();
    fs::write(root.path().join(source), "source\n").unwrap();
    fs::write(root.path().join(asset), "asset\n").unwrap();
    run_git(root.path(), &["init", "-q"]);

    let snapshot = SystemGitSnapshotProvider
        .snapshot(root.path(), source, asset)
        .unwrap();

    assert!(snapshot.working.is_empty());
    assert!(snapshot.staged.is_empty());
}

#[test]
fn system_git_provider_fails_closed_on_aggregate_overflow_and_non_utf8() {
    for mode in ["overflow", "non_utf8"] {
        let root = tempfile::tempdir().unwrap();
        let source = Path::new("sources/source.md");
        let asset = Path::new("assets/registry/asset.md");
        fs::create_dir_all(root.path().join("sources")).unwrap();
        fs::create_dir_all(root.path().join("assets/registry")).unwrap();
        fs::write(root.path().join(source), "original\n").unwrap();
        fs::write(root.path().join(asset), "asset\n").unwrap();
        fs::write(root.path().join(".gitattributes"), "*.md diff\n").unwrap();
        run_git(root.path(), &["init", "-q"]);
        run_git(
            root.path(),
            &["config", "user.email", "fixture@example.invalid"],
        );
        run_git(root.path(), &["config", "user.name", "Fixture"]);
        run_git(root.path(), &["add", "."]);
        run_git(root.path(), &["commit", "-qm", "baseline"]);
        if mode == "overflow" {
            fs::write(root.path().join(source), vec![b'x'; 2 * 1024 * 1024 + 1024]).unwrap();
        } else {
            fs::write(root.path().join(source), [b'x', 0xff, b'\n']).unwrap();
        }

        let error = SystemGitSnapshotProvider
            .snapshot(root.path(), source, asset)
            .unwrap_err();

        assert_eq!(error.code(), "git_snapshot_unavailable");
        assert!(
            !error
                .message()
                .contains(root.path().to_string_lossy().as_ref())
        );
    }
}

#[test]
fn system_git_provider_rejects_unmerged_review_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = Path::new("sources/source.md");
    let asset = Path::new("assets/registry/asset.md");
    fs::create_dir_all(root.path().join("sources")).unwrap();
    fs::create_dir_all(root.path().join("assets/registry")).unwrap();
    fs::write(root.path().join(source), "base\n").unwrap();
    fs::write(root.path().join(asset), "asset\n").unwrap();
    run_git(root.path(), &["init", "-q"]);
    run_git(
        root.path(),
        &["config", "user.email", "fixture@example.invalid"],
    );
    run_git(root.path(), &["config", "user.name", "Fixture"]);
    run_git(root.path(), &["add", "."]);
    run_git(root.path(), &["commit", "-qm", "baseline"]);
    let branch = Command::new("git")
        .arg("-C")
        .arg(root.path())
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    let branch = String::from_utf8(branch.stdout).unwrap().trim().to_owned();
    run_git(root.path(), &["checkout", "-qb", "other"]);
    fs::write(root.path().join(source), "other\n").unwrap();
    run_git(root.path(), &["commit", "-qam", "other"]);
    run_git(root.path(), &["checkout", "-q", &branch]);
    fs::write(root.path().join(source), "main\n").unwrap();
    run_git(root.path(), &["commit", "-qam", "main"]);
    let _ = Command::new("git")
        .arg("-C")
        .arg(root.path())
        .args(["merge", "other"])
        .status()
        .unwrap();

    let error = SystemGitSnapshotProvider
        .snapshot(root.path(), source, asset)
        .unwrap_err();

    assert_eq!(error.code(), "git_snapshot_unavailable");
}
