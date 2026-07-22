use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    context::PlatformEnvironment,
    profile::ProfileStore,
    setup_plan_v2::{
        SetupPlanApprovalModeV2, SetupPlanEffectV2, SetupPlanStepIdV2, apply_setup_plan_v2_tty,
        create_setup_plan_v2,
    },
    setup_v2::SetupPersonalV2Request,
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

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct Fixture {
    _temporary: TempDir,
    platform: FakePlatform,
    repository: PathBuf,
    drive_account: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        let config_home = root.join("config");
        let home = root.join("home");
        let current_dir = root.join("workspace");
        let repository_parent = root.join("Knowledge/Personal Engineering Vault");
        let drive_account = root.join("Google Drive/My Drive");
        for path in [
            &config_home,
            &home,
            &current_dir,
            &repository_parent,
            &drive_account,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let repository = repository_parent.join("personal-kb");
        Self {
            _temporary: temporary,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment: HashMap::new(),
            },
            repository,
            drive_account,
        }
    }

    fn request(&self) -> SetupPersonalV2Request<'_> {
        SetupPersonalV2Request {
            repository_root: &self.repository,
            drive_account_root: &self.drive_account,
            replace_profile: false,
        }
    }

    fn provider(&self) -> PathBuf {
        self.drive_account
            .join("My-Knowledge-OS-Assets/personal/inbox")
    }

    fn plan_path(&self, bucket: &str, plan_id: &str) -> PathBuf {
        self.platform
            .config_home
            .join("mko/setup-plans")
            .join(bucket)
            .join(format!("{plan_id}.json"))
    }
}

fn at(input: &str) -> FixedClock {
    FixedClock(
        DateTime::parse_from_rfc3339(input)
            .unwrap()
            .with_timezone(&Utc),
    )
}

#[test]
fn plan_is_non_mutating_exact_and_non_tty_apply_is_fail_closed() {
    let fixture = Fixture::new();
    let plan = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();

    assert!(plan.single_use);
    assert_eq!(plan.approval_mode, SetupPlanApprovalModeV2::Tty);
    assert_eq!(plan.steps.len(), 4);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| (step.step_id, step.effect, step.requires_human_approval))
            .collect::<Vec<_>>(),
        vec![
            (
                SetupPlanStepIdV2::ScaffoldRepository,
                SetupPlanEffectV2::Create,
                true,
            ),
            (
                SetupPlanStepIdV2::EnsureProviderInbox,
                SetupPlanEffectV2::Create,
                true,
            ),
            (
                SetupPlanStepIdV2::EnsureDashboard,
                SetupPlanEffectV2::Create,
                true,
            ),
            (
                SetupPlanStepIdV2::ConfigureProfile,
                SetupPlanEffectV2::Create,
                true,
            ),
        ]
    );
    assert!(plan.precondition_digest.starts_with("sha256:"));
    assert!(plan.effect_digest.starts_with("sha256:"));
    assert!(!fixture.repository.exists());
    assert!(!fixture.provider().exists());
    assert!(
        !ProfileStore::from_platform(&fixture.platform)
            .unwrap()
            .path()
            .exists()
    );
    assert!(fixture.plan_path("open", &plan.plan_id).is_file());

    let error = apply_setup_plan_v2_tty(
        &plan.plan_id,
        &fixture.platform,
        &at("2026-07-23T00:01:00Z"),
    )
    .unwrap_err();
    assert_eq!(error.code(), "setup_tty_required");
    assert!(!fixture.repository.exists());
    assert!(!fixture.provider().exists());
    assert!(
        ProfileStore::from_platform(&fixture.platform)
            .unwrap()
            .path()
            .parent()
            .unwrap()
            .join("setup-profile.lock")
            .is_file()
    );
    assert!(
        !ProfileStore::from_platform(&fixture.platform)
            .unwrap()
            .path()
            .exists()
    );
    assert!(fixture.plan_path("open", &plan.plan_id).is_file());
    assert!(!fixture.plan_path("consumed", &plan.plan_id).exists());
}

#[test]
fn changed_precondition_is_rejected_before_setup_mutation_and_invalidates_plan() {
    let fixture = Fixture::new();
    let plan = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();
    fs::create_dir(&fixture.repository).unwrap();
    fs::write(fixture.repository.join("unplanned.txt"), b"changed").unwrap();

    let error = apply_setup_plan_v2_tty(
        &plan.plan_id,
        &fixture.platform,
        &at("2026-07-23T00:01:00Z"),
    )
    .unwrap_err();

    assert_eq!(error.code(), "setup_plan_stale");
    assert_eq!(
        fs::read(fixture.repository.join("unplanned.txt")).unwrap(),
        b"changed"
    );
    assert!(!fixture.repository.join("knowledge-os.yaml").exists());
    assert!(!fixture.provider().exists());
    assert!(
        !ProfileStore::from_platform(&fixture.platform)
            .unwrap()
            .path()
            .exists()
    );
    assert!(fixture.plan_path("open", &plan.plan_id).is_file());
    assert!(!fixture.plan_path("consumed", &plan.plan_id).exists());
}

#[test]
fn expired_plan_is_consumed_without_target_mutation() {
    let fixture = Fixture::new();
    let plan = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();

    let error = apply_setup_plan_v2_tty(
        &plan.plan_id,
        &fixture.platform,
        &at("2026-07-23T00:15:00Z"),
    )
    .unwrap_err();

    assert_eq!(error.code(), "setup_plan_expired");
    assert!(!fixture.repository.exists());
    assert!(!fixture.provider().exists());
    assert!(
        !ProfileStore::from_platform(&fixture.platform)
            .unwrap()
            .path()
            .exists()
    );
    assert!(fixture.plan_path("open", &plan.plan_id).is_file());
    assert!(!fixture.plan_path("consumed", &plan.plan_id).exists());
}

#[test]
fn creating_a_plan_boundedly_reclaims_expired_open_plans() {
    let fixture = Fixture::new();
    let expired = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();

    let current = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:16:00Z"),
    )
    .unwrap();

    assert!(!fixture.plan_path("open", &expired.plan_id).exists());
    assert!(fixture.plan_path("consumed", &expired.plan_id).is_file());
    assert!(fixture.plan_path("open", &current.plan_id).is_file());
}

#[cfg(unix)]
#[test]
fn open_plan_cleanup_fails_closed_on_unmanaged_links() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();
    let open = fixture.platform.config_home.join("mko/setup-plans/open");
    symlink("missing-target", open.join("unmanaged.json")).unwrap();

    let error = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:16:00Z"),
    )
    .unwrap_err();

    assert!(matches!(
        error.code(),
        "setup_plan_id_invalid" | "setup_plan_permissions_invalid"
    ));
}

#[cfg(unix)]
#[test]
fn plan_file_and_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let plan = create_setup_plan_v2(
        fixture.request(),
        &fixture.platform,
        &at("2026-07-23T00:00:00Z"),
    )
    .unwrap();
    let plan_path = fixture.plan_path("open", &plan.plan_id);

    assert_eq!(
        fs::metadata(&plan_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(plan_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
