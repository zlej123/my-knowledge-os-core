use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    lock::{AssetLock, LockRecord},
    model::AssetStatus,
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset_with_clock, capture_asset,
        inspect_asset_with_clock, lineage_repair_needed, read_asset, repair_lineage_with_clock,
    },
    state::{transition_allowed, transition_asset, validate_asset_state},
};

static NEXT_TEST_ENV: AtomicU64 = AtomicU64::new(0);

struct FixedClock {
    now: DateTime<Utc>,
}

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.now
    }
}

struct TestEnv {
    root: PathBuf,
    repository: PathBuf,
    provider: PathBuf,
    local_config: PathBuf,
    old_asset_id: String,
}

impl TestEnv {
    fn new() -> Self {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mko-state-and-lock-test-{}-{unique}",
            std::process::id()
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let local_config = root.join("local-config.yaml");
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

        Self {
            root,
            repository,
            provider,
            local_config,
            old_asset_id: String::new(),
        }
    }

    fn with_asset() -> Self {
        let mut env = Self::new();
        let provider_file = env.provider.join("paper.pdf");
        fs::write(&provider_file, b"%PDF-1.7\nold-content").unwrap();
        env.old_asset_id = capture_asset(
            CaptureRequest::new(&env.repository, &provider_file)
                .with_local_config(&env.local_config)
                .with_captured_at(fixed_time()),
        )
        .unwrap()
        .asset_id;
        env
    }

    fn operation(&self) -> AssetOperationRequest {
        AssetOperationRequest::new(&self.repository, &self.old_asset_id)
            .with_local_config(&self.local_config)
    }

    fn lock(&self, asset_id: &str) -> Result<AssetLock, mko_core::error::MkoError> {
        AssetLock::acquire(
            &self.repository,
            asset_id,
            "state-and-lock-test",
            &fixed_clock(),
            false,
        )
    }

    fn old_registry_path(&self) -> PathBuf {
        self.repository
            .join("assets/registry")
            .join(format!("{}.md", self.old_asset_id))
    }

    fn replace_provider_bytes(&self, bytes: &[u8]) {
        fs::write(self.provider.join("paper.pdf"), bytes).unwrap();
    }

    fn inspect_asset(&self) -> Result<(), mko_core::error::MkoError> {
        inspect_asset_with_clock(self.operation(), &fixed_clock()).map(|_| ())
    }

    fn accept_change(&self) -> Result<mko_core::model::AssetRecord, mko_core::error::MkoError> {
        accept_changed_asset_with_clock(self.operation(), &fixed_clock())
    }

    fn old_asset(&self) -> mko_core::model::AssetRecord {
        read_asset(&self.repository, &self.old_asset_id).unwrap()
    }

    fn old_asset_id(&self) -> &str {
        &self.old_asset_id
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn fixed_clock() -> FixedClock {
    FixedClock { now: fixed_time() }
}

fn test_asset_id() -> String {
    format!("personal-asset-{}", "a".repeat(64))
}

#[test]
fn second_process_cannot_acquire_asset_lock() {
    let env = TestEnv::new();
    let asset_id = test_asset_id();
    let first = env.lock(&asset_id).unwrap();
    let error = env.lock(&asset_id).unwrap_err();
    assert_eq!(error.code(), "lock_held");
    drop(first);
    assert!(env.lock(&asset_id).is_ok());
}

#[test]
fn stale_asset_lock_requires_an_explicit_clear_request() {
    let env = TestEnv::new();
    let asset_id = test_asset_id();
    let lock_path = env
        .repository
        .join(".knowledge-os/runtime/locks")
        .join(format!("{asset_id}.lock"));
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let record = LockRecord {
        pid: u32::MAX,
        hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
        started_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        command: "interrupted-operation".into(),
        asset_id: asset_id.clone(),
        owner_token: format!("1-1-{}", "a".repeat(32)),
    };
    fs::write(&lock_path, serde_json::to_vec(&record).unwrap()).unwrap();

    let later_clock = FixedClock {
        now: DateTime::parse_from_rfc3339("2026-07-18T00:16:00Z")
            .unwrap()
            .with_timezone(&Utc),
    };
    let error =
        AssetLock::acquire(&env.repository, &asset_id, "retry", &later_clock, false).unwrap_err();
    assert_eq!(error.code(), "lock_held");
    assert!(lock_path.exists());

    let recovered = AssetLock::acquire(&env.repository, &asset_id, "retry", &later_clock, true);
    assert!(recovered.is_ok());
}

#[test]
fn malformed_asset_ids_cannot_create_runtime_lock_paths() {
    let env = TestEnv::new();
    for asset_id in [
        "personal-asset-deadbeef",
        "../personal-asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "/tmp/personal-asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let error = env.lock(asset_id).unwrap_err();
        assert_eq!(error.code(), "asset_id_invalid");
    }
    assert!(!env.repository.join(".knowledge-os/runtime/locks").exists());
}

#[test]
fn dropping_a_lock_does_not_remove_a_different_owner_lock() {
    let env = TestEnv::new();
    let asset_id = test_asset_id();
    let lock = env.lock(&asset_id).unwrap();
    let path = env
        .repository
        .join(".knowledge-os/runtime/locks")
        .join(format!("{asset_id}.lock"));
    let replacement = LockRecord {
        pid: std::process::id(),
        hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
        started_at: fixed_time(),
        command: "replacement-owner".into(),
        asset_id,
        owner_token: "replacement-owner-token".into(),
    };
    fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();

    drop(lock);

    assert!(path.exists());
}

#[test]
fn dropping_a_lock_does_not_race_a_stale_lock_takeover() {
    let env = TestEnv::new();
    let asset_id = test_asset_id();
    let lock = env.lock(&asset_id).unwrap();
    let path = env
        .repository
        .join(".knowledge-os/runtime/locks")
        .join(format!("{asset_id}.lock"));
    fs::write(path.with_extension("lock.takeover"), b"active takeover").unwrap();

    drop(lock);

    assert!(path.exists());
}

#[test]
fn concurrent_stale_clearers_leave_one_live_lock_intact() {
    let env = TestEnv::new();
    let asset_id = test_asset_id();
    let lock_path = env
        .repository
        .join(".knowledge-os/runtime/locks")
        .join(format!("{asset_id}.lock"));
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let stale = LockRecord {
        pid: u32::MAX,
        hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
        started_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        command: "interrupted-operation".into(),
        asset_id: asset_id.clone(),
        owner_token: "interrupted-owner".into(),
    };
    fs::write(&lock_path, serde_json::to_vec(&stale).unwrap()).unwrap();
    let later_clock = Arc::new(FixedClock {
        now: DateTime::parse_from_rfc3339("2026-07-18T00:16:00Z")
            .unwrap()
            .with_timezone(&Utc),
    });
    let barrier = Arc::new(Barrier::new(2));
    let results = thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..2 {
            let repository = env.repository.clone();
            let asset_id = asset_id.clone();
            let barrier = Arc::clone(&barrier);
            let clock = Arc::clone(&later_clock);
            workers.push(scope.spawn(move || {
                barrier.wait();
                AssetLock::acquire(&repository, &asset_id, "concurrent-clear", &*clock, true)
            }));
        }
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });

    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "concurrent stale-clear results: {results:?}"
    );
    assert!(lock_path.exists());
}

#[test]
fn transition_matrix_accepts_only_durable_lifecycle_edges() {
    assert!(transition_allowed(
        AssetStatus::Registered,
        AssetStatus::Extracted
    ));
    assert!(transition_allowed(
        AssetStatus::Processed,
        AssetStatus::Changed
    ));
    assert!(transition_allowed(
        AssetStatus::Changed,
        AssetStatus::Superseded
    ));
    assert!(transition_allowed(
        AssetStatus::Missing,
        AssetStatus::ReviewPending
    ));
    assert!(transition_allowed(
        AssetStatus::Failed,
        AssetStatus::Processed
    ));
    assert!(!transition_allowed(
        AssetStatus::Registered,
        AssetStatus::Processed
    ));
    assert!(!transition_allowed(
        AssetStatus::Superseded,
        AssetStatus::Registered
    ));
}

#[test]
fn validator_accepts_legitimate_nested_changed_and_missing_failures() {
    let env = TestEnv::with_asset();

    let mut changed = env.old_asset();
    transition_asset(&mut changed, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Failed, fixed_time()).unwrap();
    assert_eq!(
        changed.durable_state_history,
        vec![AssetStatus::Registered, AssetStatus::Changed]
    );
    validate_asset_state(&changed).unwrap();

    let mut missing = env.old_asset();
    transition_asset(&mut missing, AssetStatus::Missing, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Failed, fixed_time()).unwrap();
    assert_eq!(
        missing.durable_state_history,
        vec![AssetStatus::Registered, AssetStatus::Missing]
    );
    validate_asset_state(&missing).unwrap();
}

#[test]
fn transition_rejects_recovery_to_a_state_other_than_the_previous_durable_state() {
    let env = TestEnv::with_asset();
    let mut asset = env.old_asset();
    transition_asset(&mut asset, AssetStatus::Changed, fixed_time()).unwrap();

    let error = transition_asset(&mut asset, AssetStatus::Processed, fixed_time()).unwrap_err();

    assert_eq!(error.code(), "invalid_state_transition");
    transition_asset(&mut asset, AssetStatus::Registered, fixed_time()).unwrap();
}

#[test]
fn durable_transitions_update_the_checkpoint_used_for_change_and_failure_recovery() {
    let env = TestEnv::with_asset();
    let mut asset = env.old_asset();
    transition_asset(&mut asset, AssetStatus::Extracted, fixed_time()).unwrap();
    transition_asset(&mut asset, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut asset, AssetStatus::Extracted, fixed_time()).unwrap();
    transition_asset(&mut asset, AssetStatus::Failed, fixed_time()).unwrap();

    let error = transition_asset(&mut asset, AssetStatus::Registered, fixed_time()).unwrap_err();

    assert_eq!(error.code(), "invalid_state_transition");
    transition_asset(&mut asset, AssetStatus::Extracted, fixed_time()).unwrap();
}

#[test]
fn failed_recovery_returns_to_changed_missing_and_superseded_checkpoints() {
    let env = TestEnv::with_asset();

    let mut changed = env.old_asset();
    transition_asset(&mut changed, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Changed, fixed_time()).unwrap();

    let mut missing = env.old_asset();
    transition_asset(&mut missing, AssetStatus::Missing, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Missing, fixed_time()).unwrap();

    let mut superseded = env.old_asset();
    transition_asset(&mut superseded, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Superseded, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Superseded, fixed_time()).unwrap();
}

#[test]
fn failed_recovery_pops_its_checkpoint_before_restoring_changed_missing_or_superseded() {
    let env = TestEnv::with_asset();

    let mut changed = env.old_asset();
    transition_asset(&mut changed, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut changed, AssetStatus::Registered, fixed_time()).unwrap();
    assert_eq!(changed.asset_status, AssetStatus::Registered);
    assert!(changed.durable_state_history.is_empty());

    let mut missing = env.old_asset();
    transition_asset(&mut missing, AssetStatus::Missing, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Missing, fixed_time()).unwrap();
    transition_asset(&mut missing, AssetStatus::Registered, fixed_time()).unwrap();
    assert_eq!(missing.asset_status, AssetStatus::Registered);
    assert!(missing.durable_state_history.is_empty());

    let mut superseded = env.old_asset();
    transition_asset(&mut superseded, AssetStatus::Changed, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Superseded, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Failed, fixed_time()).unwrap();
    transition_asset(&mut superseded, AssetStatus::Superseded, fixed_time()).unwrap();
    assert_eq!(superseded.asset_status, AssetStatus::Superseded);
    assert_eq!(
        superseded.durable_state_history,
        vec![AssetStatus::Registered]
    );

    for history in [
        &changed.durable_state_history,
        &missing.durable_state_history,
        &superseded.durable_state_history,
    ] {
        assert!(!history.contains(&AssetStatus::Failed));
    }
}

#[test]
fn changed_asset_is_superseded_by_new_content_record() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    assert_eq!(env.old_asset().asset_status, AssetStatus::Changed);
    let new_asset = env.accept_change().unwrap();
    assert_eq!(new_asset.supersedes.as_deref(), Some(env.old_asset_id()));
    assert_eq!(env.old_asset().asset_status, AssetStatus::Superseded);
}

#[test]
fn inspecting_a_superseded_asset_preserves_its_terminal_state() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    env.accept_change().unwrap();

    env.inspect_asset().unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Superseded);
}

#[test]
fn inspect_marks_missing_assets_and_restores_the_original_state_when_bytes_return() {
    let env = TestEnv::with_asset();
    fs::remove_file(env.provider.join("paper.pdf")).unwrap();
    env.inspect_asset().unwrap();
    assert_eq!(env.old_asset().asset_status, AssetStatus::Missing);

    env.replace_provider_bytes(b"%PDF-1.7\nold-content");
    env.inspect_asset().unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Registered);
}

#[test]
fn inspect_restores_changed_assets_when_the_original_fingerprint_returns() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    assert_eq!(env.old_asset().asset_status, AssetStatus::Changed);

    env.replace_provider_bytes(b"%PDF-1.7\nold-content");
    env.inspect_asset().unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Registered);
}

#[test]
fn accept_change_rejects_replacement_without_a_pdf_signature() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"not a PDF");
    env.inspect_asset().unwrap();

    let error = env.accept_change().unwrap_err();

    assert_eq!(error.code(), "invalid_pdf");
    assert_eq!(env.old_asset().asset_status, AssetStatus::Changed);
}

#[test]
fn interrupted_acceptance_is_reported_and_repaired_idempotently() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    let publication_lock = env
        .old_registry_path()
        .with_file_name(format!(".{}.md.publish.lock", env.old_asset_id()));
    fs::write(&publication_lock, b"interrupted old-record update").unwrap();

    let error = env.accept_change().unwrap_err();

    assert_eq!(error.code(), "registry_locked");
    assert_eq!(env.old_asset().asset_status, AssetStatus::Changed);
    assert_eq!(
        lineage_repair_needed(&env.repository).unwrap(),
        vec![env.old_asset_id().to_owned()]
    );
    fs::remove_file(publication_lock).unwrap();

    repair_lineage_with_clock(env.operation(), &fixed_clock()).unwrap();
    repair_lineage_with_clock(env.operation(), &fixed_clock()).unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Superseded);
    assert!(lineage_repair_needed(&env.repository).unwrap().is_empty());
}

#[test]
fn lineage_repair_is_idempotent_after_a_new_asset_is_created() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    let new_asset = env.accept_change().unwrap();

    repair_lineage_with_clock(env.operation(), &fixed_clock()).unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Superseded);
    assert_eq!(new_asset.supersedes.as_deref(), Some(env.old_asset_id()));
}
