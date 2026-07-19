use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    context::{PlatformEnvironment, Scope},
    doctor::{DoctorEnvironment, DoctorRequest, DoctorStatus, diagnose},
    hooks::install_hooks,
    json_v1::{NextAction, RecoveryKind},
    lock::LockRecord,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
    version::{KNOWLEDGE_CONTRACT_VERSION, PRODUCT_VERSION},
};
use tempfile::TempDir;

const CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";

#[derive(Clone)]
struct FakePlatform {
    config_home: PathBuf,
    home: PathBuf,
    current_dir: PathBuf,
    environment: HashMap<OsString, OsString>,
}

impl PlatformEnvironment for FakePlatform {
    fn config_home(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.config_home.clone())
    }

    fn home_dir(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.home.clone())
    }

    fn current_dir(&self) -> Result<PathBuf, mko_core::error::MkoError> {
        Ok(self.current_dir.clone())
    }

    fn environment_value(&self, name: &OsStr) -> Option<OsString> {
        self.environment.get(name).cloned()
    }
}

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct Fixture {
    _root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
    platform: FakePlatform,
    clock: FixedClock,
}

impl DoctorEnvironment for Fixture {
    fn platform(&self) -> &dyn PlatformEnvironment {
        &self.platform
    }

    fn clock(&self) -> &dyn Clock {
        &self.clock
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let current_dir = root.path().join("outside");
        for path in [&repository, &provider, &home, &config_home, &current_dir] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(repository.join("knowledge-os.yaml"), CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        Self {
            _root: root,
            repository,
            provider,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment: HashMap::new(),
            },
            clock: FixedClock(
                DateTime::parse_from_rfc3339("2026-07-19T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
        }
    }

    fn configure(&self) {
        let store = ProfileStore::from_platform(&self.platform).unwrap();
        store
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: self.repository.clone(),
                        provider_root: self.provider.clone(),
                        scope: Scope::Personal,
                    },
                )]),
            })
            .unwrap();
    }

    fn request(&self) -> DoctorRequest {
        DoctorRequest::new().with_repository(&self.repository)
    }

    fn report(&self) -> mko_core::doctor::DoctorReport {
        diagnose(self.request(), self)
    }
}

#[test]
fn reports_versions_and_does_not_mutate_knowledge_records() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.repository);

    let report = fixture.report();

    assert_eq!(check(&report, "product_version").message, PRODUCT_VERSION);
    assert_eq!(
        check(&report, "contract_version").message,
        KNOWLEDGE_CONTRACT_VERSION
    );
    assert_eq!(
        check(&report, "profile_missing").status,
        DoctorStatus::Blocked
    );
    assert_eq!(report.next_action, NextAction::Configure);
    assert_eq!(snapshot(&fixture.repository), before);
}

#[test]
fn reports_incompatible_repository_without_hiding_the_missing_profile() {
    let fixture = Fixture::new();
    fs::write(
        fixture.repository.join("knowledge-os.yaml"),
        "scope: shared\n",
    )
    .unwrap();

    let report = fixture.report();

    assert_eq!(
        check(&report, "repository_incompatible").status,
        DoctorStatus::Blocked
    );
    assert_eq!(
        check(&report, "profile_missing").status,
        DoctorStatus::Blocked
    );
    assert_eq!(report.next_action, NextAction::Configure);
}

#[test]
fn reports_provider_missing_hydration_failure_and_writable_state_independently() {
    let fixture = Fixture::new();
    fixture.configure();
    fs::remove_dir(&fixture.provider).unwrap();

    let missing = fixture.report();
    assert_eq!(
        check(&missing, "provider_missing").status,
        DoctorStatus::Blocked
    );
    assert_eq!(missing.next_action, NextAction::Configure);

    fs::create_dir(&fixture.provider).unwrap();
    fs::write(fixture.provider.join("unhydrated.pdf"), []).unwrap();
    let hydration = fixture.report();
    assert_eq!(
        check(&hydration, "provider_hydration_failed").recovery,
        Some(RecoveryKind::Hydrate)
    );
    assert_eq!(hydration.next_action, NextAction::Hydrate);
}

#[cfg(unix)]
#[test]
fn reports_unreadable_and_unwritable_provider_without_writing_to_it() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.configure();
    fs::set_permissions(&fixture.provider, fs::Permissions::from_mode(0o200)).unwrap();
    let unreadable = fixture.report();
    assert_eq!(
        check(&unreadable, "provider_unreadable").status,
        DoctorStatus::Blocked
    );

    fs::set_permissions(&fixture.provider, fs::Permissions::from_mode(0o500)).unwrap();
    let unwritable = fixture.report();
    assert_eq!(
        check(&unwritable, "provider_unwritable").status,
        DoctorStatus::Blocked
    );
    fs::set_permissions(&fixture.provider, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn reports_missing_custom_and_managed_hooks() {
    let fixture = Fixture::new();
    fixture.configure();

    let missing = fixture.report();
    assert_eq!(
        check(&missing, "hook_missing").status,
        DoctorStatus::Blocked
    );

    git(
        &fixture.repository,
        &["config", "--local", "core.hooksPath", "custom-hooks"],
    );
    let custom = fixture.report();
    assert_eq!(
        check(&custom, "hook_conflict").status,
        DoctorStatus::Blocked
    );
    git(
        &fixture.repository,
        &["config", "--local", "--unset", "core.hooksPath"],
    );
    install_hooks(&fixture.repository).unwrap();

    assert_eq!(
        check(&fixture.report(), "hook_managed").status,
        DoctorStatus::Healthy
    );
}

#[test]
fn stale_lock_is_reported_and_has_lower_priority_than_configuration() {
    let fixture = Fixture::new();
    let asset_id = format!("personal-asset-{}", "a".repeat(64));
    let lock = fixture
        .repository
        .join(".knowledge-os/runtime/locks")
        .join(format!("{asset_id}.lock"));
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(
        &lock,
        serde_json::to_vec(&LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            command: "prepare".into(),
            asset_id,
            owner_token: "crashed".into(),
        })
        .unwrap(),
    )
    .unwrap();

    let report = fixture.report();

    assert_eq!(check(&report, "stale_lock").status, DoctorStatus::Warning);
    assert_eq!(report.next_action, NextAction::Configure);
}

#[test]
fn healthy_setup_has_no_recovery_action() {
    let fixture = Fixture::new();
    fixture.configure();
    install_hooks(&fixture.repository).unwrap();

    let report = fixture.report();

    assert!(report.healthy);
    assert_eq!(report.next_action, NextAction::None);
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status == DoctorStatus::Healthy)
    );
}

fn check<'a>(
    report: &'a mko_core::doctor::DoctorReport,
    code: &str,
) -> &'a mko_core::doctor::DoctorCheck {
    report
        .checks
        .iter()
        .find(|check| check.code == code)
        .unwrap_or_else(|| panic!("missing doctor check {code}: {:#?}", report.checks))
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success());
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    snapshot_into(root, root, &mut entries);
    entries
}

fn snapshot_into(root: &Path, path: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
    let mut children = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        if child.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if child.is_dir() {
            snapshot_into(root, &child, entries);
        } else {
            entries.push((
                child.strip_prefix(root).unwrap().to_path_buf(),
                fs::read(&child).unwrap(),
            ));
        }
    }
}
