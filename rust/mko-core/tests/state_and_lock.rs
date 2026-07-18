use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    lock::{AssetLock, LockRecord},
    model::AssetStatus,
    registry::{
        AssetOperationRequest, CaptureRequest, accept_changed_asset_with_clock, capture_asset,
        inspect_asset_with_clock, read_asset, repair_lineage_with_clock,
    },
    state::{transition_allowed, transition_asset},
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

#[test]
fn second_process_cannot_acquire_asset_lock() {
    let env = TestEnv::new();
    let first = env.lock("personal-asset-deadbeef").unwrap();
    let error = env.lock("personal-asset-deadbeef").unwrap_err();
    assert_eq!(error.code(), "lock_held");
    drop(first);
    assert!(env.lock("personal-asset-deadbeef").is_ok());
}

#[test]
fn stale_asset_lock_requires_an_explicit_clear_request() {
    let env = TestEnv::new();
    let asset_id = "personal-asset-deadbeef";
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
        asset_id: asset_id.into(),
    };
    fs::write(&lock_path, serde_json::to_vec(&record).unwrap()).unwrap();

    let later_clock = FixedClock {
        now: DateTime::parse_from_rfc3339("2026-07-18T00:16:00Z")
            .unwrap()
            .with_timezone(&Utc),
    };
    let error =
        AssetLock::acquire(&env.repository, asset_id, "retry", &later_clock, false).unwrap_err();
    assert_eq!(error.code(), "lock_held");
    assert!(lock_path.exists());

    let recovered = AssetLock::acquire(&env.repository, asset_id, "retry", &later_clock, true);
    assert!(recovered.is_ok());
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
fn lineage_repair_is_idempotent_after_a_new_asset_is_created() {
    let env = TestEnv::with_asset();
    env.replace_provider_bytes(b"%PDF-1.7\nnew-content");
    env.inspect_asset().unwrap();
    let new_asset = env.accept_change().unwrap();

    repair_lineage_with_clock(env.operation(), &fixed_clock()).unwrap();

    assert_eq!(env.old_asset().asset_status, AssetStatus::Superseded);
    assert_eq!(new_asset.supersedes.as_deref(), Some(env.old_asset_id()));
}
