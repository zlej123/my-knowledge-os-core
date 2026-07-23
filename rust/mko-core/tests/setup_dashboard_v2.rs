use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
};

use mko_core::{
    context::{PlatformEnvironment, Scope},
    dashboard_v2::{DashboardOutcomeV2, ensure_dashboard_v2},
    profile::{MachineProfileFile, PROFILE_SCHEMA_VERSION, PersonalProfile, ProfileStore},
    scaffold_v2::{ScaffoldOutcomeV2, scaffold_personal_kb_v2},
    setup_v2::{SetupPersonalV2Request, setup_personal_v2},
};
use tempfile::TempDir;

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

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
    platform: FakePlatform,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        let config_home = root.join("config");
        let home = root.join("home");
        let current_dir = root.join("workspace");
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&current_dir).unwrap();
        Self {
            _temporary: temporary,
            root,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment: HashMap::new(),
            },
        }
    }

    fn drive_account(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn profile_store(&self) -> ProfileStore {
        ProfileStore::from_platform(&self.platform).unwrap()
    }
}

#[test]
fn setup_creates_exact_provider_profile_and_dashboard_and_is_idempotent() {
    let fixture = Fixture::new();
    let repository = fixture.root.join("Personal Vault/personal-kb");
    fs::create_dir_all(repository.parent().unwrap()).unwrap();
    let drive_account = fixture.drive_account("Google Drive/My Drive");
    let provider = drive_account.join("My-Knowledge-OS-Assets/personal/inbox");

    let first = setup_personal_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_account,
            replace_profile: false,
        },
        &fixture.platform,
    )
    .unwrap();

    assert_eq!(first.scaffold, ScaffoldOutcomeV2::Created);
    assert_eq!(first.dashboard.outcome, DashboardOutcomeV2::Created);
    assert!(first.profile_changed);
    assert_eq!(first.repository_root, repository.canonicalize().unwrap());
    assert_eq!(first.provider_root, provider.canonicalize().unwrap());
    assert!(provider.is_dir());
    assert!(repository.join("HOME.md").is_file());
    assert!(repository.join("views/review-queue.base").is_file());
    assert!(repository.join("views/knowledge-library.base").is_file());
    let profiles = fixture.profile_store().read().unwrap().unwrap();
    assert_eq!(profiles.schema_version, PROFILE_SCHEMA_VERSION);
    assert_eq!(profiles.default_profile, "personal");
    assert_eq!(
        profiles.profiles.get("personal"),
        Some(&PersonalProfile {
            repository_root: repository.canonicalize().unwrap(),
            provider_root: provider.canonicalize().unwrap(),
            scope: Scope::Personal,
        })
    );

    let home_before = fs::read(repository.join("HOME.md")).unwrap();
    let profile_before = fs::read(fixture.profile_store().path()).unwrap();
    let second = setup_personal_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_account,
            replace_profile: false,
        },
        &fixture.platform,
    )
    .unwrap();

    assert_eq!(second.scaffold, ScaffoldOutcomeV2::Existing);
    assert_eq!(second.dashboard.outcome, DashboardOutcomeV2::Existing);
    assert!(!second.profile_changed);
    assert_eq!(fs::read(repository.join("HOME.md")).unwrap(), home_before);
    assert_eq!(
        fs::read(fixture.profile_store().path()).unwrap(),
        profile_before
    );
}

#[test]
fn profile_conflict_is_rejected_before_creating_destination_or_provider_inbox() {
    let fixture = Fixture::new();
    let existing_repository = fixture.root.join("existing-kb");
    scaffold_personal_kb_v2(&existing_repository).unwrap();
    let existing_provider = fixture.root.join("existing-provider");
    fs::create_dir_all(&existing_provider).unwrap();
    let existing = MachineProfileFile {
        schema_version: PROFILE_SCHEMA_VERSION,
        default_profile: "personal".into(),
        profiles: BTreeMap::from([(
            "personal".into(),
            PersonalProfile {
                repository_root: existing_repository.canonicalize().unwrap(),
                provider_root: existing_provider.canonicalize().unwrap(),
                scope: Scope::Personal,
            },
        )]),
    };
    fixture.profile_store().write(&existing).unwrap();
    let profile_before = fs::read(fixture.profile_store().path()).unwrap();
    let repository = fixture.root.join("new-parent/new-kb");
    fs::create_dir_all(repository.parent().unwrap()).unwrap();
    let drive_account = fixture.drive_account("new-drive");
    let provider = drive_account.join("My-Knowledge-OS-Assets/personal/inbox");

    let error = setup_personal_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_account,
            replace_profile: false,
        },
        &fixture.platform,
    )
    .unwrap_err();

    assert_eq!(error.code(), "profile_conflict");
    assert!(!repository.exists());
    assert!(!provider.exists());
    assert_eq!(
        fs::read(fixture.profile_store().path()).unwrap(),
        profile_before
    );
}

#[test]
fn storage_overlap_is_rejected_before_creating_the_kb_or_provider_inbox() {
    let fixture = Fixture::new();
    let drive_account = fixture.drive_account("Drive account");
    let repository = drive_account.join("personal-kb");
    let provider = drive_account.join("My-Knowledge-OS-Assets/personal/inbox");

    let error = setup_personal_v2(
        SetupPersonalV2Request {
            repository_root: &repository,
            drive_account_root: &drive_account,
            replace_profile: false,
        },
        &fixture.platform,
    )
    .unwrap_err();

    assert_eq!(error.code(), "storage_roots_overlap");
    assert!(!repository.exists());
    assert!(!provider.exists());
    assert!(!fixture.profile_store().path().exists());
}

#[test]
fn dashboard_drift_refuses_overwrite_and_preserves_edited_bytes() {
    let fixture = Fixture::new();
    let repository = fixture.root.join("personal-kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let first = ensure_dashboard_v2(&repository).unwrap();
    assert_eq!(first.outcome, DashboardOutcomeV2::Created);
    let edited = b"---\ntype: dashboard\n---\n\n# My edited home\n";
    fs::write(repository.join("HOME.md"), edited).unwrap();

    let error = ensure_dashboard_v2(&repository).unwrap_err();

    assert_eq!(error.code(), "dashboard_user_modified");
    assert_eq!(fs::read(repository.join("HOME.md")).unwrap(), edited);
}
