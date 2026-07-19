use std::{
    cell::Cell,
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
    doctor::{
        DiagnosticArea, DoctorEnvironment, DoctorRequest, DoctorStatus, ProviderAccessInspection,
        ProviderEntryInspection, ProviderEntryState, SystemDoctorEnvironment, diagnose,
    },
    hooks::install_hooks,
    json_v1::NextAction,
    lock::LockRecord,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
    version::{KNOWLEDGE_CONTRACT_VERSION, PRODUCT_VERSION},
};
use tempfile::TempDir;

const CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";
const INBOX_SUFFIX: &str = "My-Knowledge-OS-Assets/personal/inbox";

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
    root: PathBuf,
    repository: PathBuf,
    provider: PathBuf,
    account_root: PathBuf,
    platform: FakePlatform,
    clock: FixedClock,
    readable: ProviderAccessInspection,
    writable: ProviderAccessInspection,
    provider_entries: Result<Vec<ProviderEntryInspection>, mko_core::error::MkoError>,
    read_calls: Cell<u32>,
    write_calls: Cell<u32>,
    entry_calls: Cell<u32>,
}

impl DoctorEnvironment for Fixture {
    fn platform(&self) -> &dyn PlatformEnvironment {
        &self.platform
    }

    fn clock(&self) -> &dyn Clock {
        &self.clock
    }

    fn inspect_provider_read_access(&self, _: &Path) -> ProviderAccessInspection {
        self.read_calls.set(self.read_calls.get() + 1);
        self.readable.clone()
    }

    fn inspect_provider_write_access(&self, _: &Path) -> ProviderAccessInspection {
        self.write_calls.set(self.write_calls.get() + 1);
        self.writable.clone()
    }

    fn inspect_provider_entries(
        &self,
        _: &Path,
    ) -> Result<Vec<ProviderEntryInspection>, mko_core::error::MkoError> {
        self.entry_calls.set(self.entry_calls.get() + 1);
        self.provider_entries.clone()
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let account_root = root.path().join("provider-account");
        let provider = account_root.join(INBOX_SUFFIX);
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let current_dir = root.path().join("outside");
        for path in [&repository, &provider, &home, &config_home, &current_dir] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(repository.join("knowledge-os.yaml"), CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        Self {
            root: root.path().to_path_buf(),
            _root: root,
            repository,
            provider: provider.clone(),
            account_root,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment: HashMap::from([(
                    OsString::from("MKO_PERSONAL_PROVIDER_ROOT"),
                    provider.into_os_string(),
                )]),
            },
            clock: FixedClock(
                DateTime::parse_from_rfc3339("2026-07-19T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            readable: ProviderAccessInspection::Allowed,
            writable: ProviderAccessInspection::Allowed,
            provider_entries: Ok(Vec::new()),
            read_calls: Cell::new(0),
            write_calls: Cell::new(0),
            entry_calls: Cell::new(0),
        }
    }

    fn configure(&self) {
        self.configure_paths(&self.repository, &self.provider);
    }

    fn configure_paths(&self, repository: &Path, provider: &Path) {
        let store = ProfileStore::from_platform(&self.platform).unwrap();
        store
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: repository.to_path_buf(),
                        provider_root: provider.to_path_buf(),
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

    fn make_healthy(&self) {
        self.configure();
        install_hooks(&self.repository).unwrap();
    }

    fn write_lock(&self, suffix: &str, pid: u32) -> PathBuf {
        let asset_id = format!("personal-asset-{}", "a".repeat(64));
        let path = self
            .repository
            .join(".knowledge-os/runtime/locks")
            .join(format!("{asset_id}.{suffix}"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&LockRecord {
                pid,
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
        path
    }
}

#[test]
fn reports_versions_profile_schema_and_does_not_mutate_any_diagnostic_surface() {
    let fixture = Fixture::new();
    fixture.make_healthy();
    let before = snapshot(&fixture.root);

    let report = fixture.report();

    assert_eq!(check(&report, "product_version").message, PRODUCT_VERSION);
    assert_eq!(
        check(&report, "contract_version").message,
        KNOWLEDGE_CONTRACT_VERSION
    );
    assert_eq!(
        check(&report, "profile_valid").status,
        DoctorStatus::Healthy
    );
    assert!(report.healthy);
    assert_eq!(report.next_action, NextAction::None);
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn explicit_repository_uses_its_environment_provider_not_a_mismatched_profile() {
    let fixture = Fixture::new();
    let other_repository = fixture.root.join("other-repository");
    let other_provider = fixture.root.join("other-account").join(INBOX_SUFFIX);
    fs::create_dir_all(&other_repository).unwrap();
    fs::create_dir_all(&other_provider).unwrap();
    fs::write(other_repository.join("knowledge-os.yaml"), CONFIG).unwrap();
    fixture.configure_paths(&other_repository, &other_provider);

    let report = fixture.report();

    assert_eq!(
        check(&report, "repository_access").path.as_deref(),
        Some(fs::canonicalize(&fixture.repository).unwrap().as_path())
    );
    assert_eq!(
        check(&report, "provider_inbox").path.as_deref(),
        Some(fixture.provider.as_path())
    );
    assert_eq!(
        check(&report, "profile_valid").status,
        DoctorStatus::Healthy
    );
}

#[test]
fn ancestor_repository_wins_and_uses_its_environment_provider() {
    let mut fixture = Fixture::new();
    let nested = fixture.repository.join("sources/nested");
    fs::create_dir_all(&nested).unwrap();
    fixture.platform.current_dir = nested;
    let other_repository = fixture.root.join("other-repository");
    let other_provider = fixture.root.join("other-account").join(INBOX_SUFFIX);
    fs::create_dir_all(&other_repository).unwrap();
    fs::create_dir_all(&other_provider).unwrap();
    fs::write(other_repository.join("knowledge-os.yaml"), CONFIG).unwrap();
    fixture.configure_paths(&other_repository, &other_provider);

    let report = diagnose(DoctorRequest::new(), &fixture);

    assert_eq!(
        check(&report, "repository_access").path.as_deref(),
        Some(fs::canonicalize(&fixture.repository).unwrap().as_path())
    );
    assert_eq!(
        check(&report, "provider_inbox").path.as_deref(),
        Some(fixture.provider.as_path())
    );
}

#[test]
fn account_root_is_not_accepted_as_the_personal_inbox() {
    let mut fixture = Fixture::new();
    fixture.platform.environment.insert(
        OsString::from("MKO_PERSONAL_PROVIDER_ROOT"),
        fixture.account_root.as_os_str().to_owned(),
    );

    let report = fixture.report();

    assert_eq!(
        check(&report, "provider_root_invalid").status,
        DoctorStatus::Blocked
    );
    assert_eq!(report.next_action, NextAction::Configure);
    assert_eq!(fixture.read_calls.get(), 0);
    assert_eq!(fixture.write_calls.get(), 0);
    assert_eq!(fixture.entry_calls.get(), 0);
}

#[cfg(unix)]
#[test]
fn symlink_provider_root_is_rejected_without_access_or_entry_inspection() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = fixture.root.join("outside-provider");
    fs::create_dir(&outside).unwrap();
    fs::remove_dir(&fixture.provider).unwrap();
    symlink(&outside, &fixture.provider).unwrap();

    let report = fixture.report();

    assert_eq!(
        check(&report, "provider_root_invalid").status,
        DoctorStatus::Blocked
    );
    assert_eq!(fixture.read_calls.get(), 0);
    assert_eq!(fixture.write_calls.get(), 0);
    assert_eq!(fixture.entry_calls.get(), 0);
}

#[test]
fn explicit_repository_reports_a_missing_environment_provider() {
    let mut fixture = Fixture::new();
    fixture
        .platform
        .environment
        .remove(OsStr::new("MKO_PERSONAL_PROVIDER_ROOT"));

    let report = fixture.report();

    assert_eq!(
        check(&report, "profile_missing").status,
        DoctorStatus::Blocked
    );
    assert_eq!(
        check(&report, "provider_missing").status,
        DoctorStatus::Blocked
    );
    assert_eq!(report.next_action, NextAction::Configure);
}

#[test]
fn profile_health_is_reported_independently_and_has_first_priority() {
    let fixture = Fixture::new();
    let store = ProfileStore::from_platform(&fixture.platform).unwrap();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), "schema_version: nope\n").unwrap();
    fs::write(
        fixture.repository.join("knowledge-os.yaml"),
        "scope: shared\n",
    )
    .unwrap();

    let report = fixture.report();

    assert_eq!(
        check(&report, "profile_unreadable").status,
        DoctorStatus::Blocked
    );
    assert_eq!(
        check(&report, "repository_incompatible").status,
        DoctorStatus::Blocked
    );
    assert_eq!(report.next_action, NextAction::Configure);
    assert_eq!(primary_issue(&report).code, "profile_unreadable");
}

#[test]
fn provider_access_and_entry_states_accumulate_without_short_circuiting() {
    let mut fixture = Fixture::new();
    fixture.configure();
    fixture.readable = ProviderAccessInspection::Denied;
    fixture.writable = ProviderAccessInspection::Denied;
    fixture.provider_entries = Ok(vec![
        ProviderEntryInspection::new(
            fixture.provider.join("offline.pdf"),
            ProviderEntryState::NotHydrated,
        ),
        ProviderEntryInspection::new(
            fixture.provider.join("broken.pdf"),
            ProviderEntryState::Corrupt,
        ),
        ProviderEntryInspection::with_detail(
            fixture.provider.join("secret.pdf"),
            ProviderEntryState::Unreadable,
            "entry metadata denied",
        ),
    ]);

    let report = fixture.report();

    for code in [
        "provider_unreadable",
        "provider_unwritable",
        "provider_hydration_failed",
        "provider_pdf_corrupt",
        "provider_pdf_unreadable",
    ] {
        assert_eq!(check(&report, code).status, DoctorStatus::Blocked, "{code}");
        assert_eq!(check(&report, code).area, DiagnosticArea::Provider);
    }
    assert_eq!(report.next_action, NextAction::Repair);
    assert_eq!(primary_issue(&report).code, "provider_unreadable");
    assert!(
        check(&report, "provider_pdf_unreadable")
            .message
            .contains("entry metadata denied")
    );
}

#[test]
fn indeterminate_access_and_hydration_are_not_reported_as_healthy() {
    let mut fixture = Fixture::new();
    fixture.configure();
    fixture.readable = ProviderAccessInspection::Indeterminate("read ACL unavailable".into());
    fixture.writable = ProviderAccessInspection::Indeterminate("write ACL unavailable".into());
    fixture.provider_entries = Ok(vec![ProviderEntryInspection::new(
        fixture.provider.join("unknown.pdf"),
        ProviderEntryState::Unknown,
    )]);

    let report = fixture.report();

    assert_eq!(
        check(&report, "provider_read_inspection_failed").status,
        DoctorStatus::Blocked
    );
    assert_eq!(
        check(&report, "provider_write_inspection_failed").status,
        DoctorStatus::Blocked
    );
    assert_eq!(
        check(&report, "provider_inspection_failed").status,
        DoctorStatus::Blocked
    );
    assert!(!report.healthy);
}

#[test]
fn zero_byte_pdf_is_not_a_hydration_signal() {
    let fixture = Fixture::new();
    fixture.configure();
    fs::write(fixture.provider.join("empty.pdf"), []).unwrap();

    let entries = SystemDoctorEnvironment::default()
        .inspect_provider_entries(&fixture.provider)
        .unwrap();
    let report = fixture.report();

    assert!(
        entries
            .iter()
            .all(|entry| entry.state != ProviderEntryState::NotHydrated)
    );
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.code != "provider_hydration_failed")
    );
    assert_eq!(
        check(&report, "provider_hydration").status,
        DoctorStatus::Healthy
    );
}

#[test]
fn system_provider_metadata_walk_excludes_hidden_and_temporary_entries() {
    let fixture = Fixture::new();
    let hidden = fixture.provider.join(".hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("secret.pdf"), b"hidden").unwrap();
    fs::write(fixture.provider.join("~$temporary.pdf"), b"temporary").unwrap();
    fs::write(fixture.provider.join("draft.pdf.partial"), b"partial").unwrap();
    fs::write(fixture.provider.join("visible.pdf"), b"visible").unwrap();

    let entries = SystemDoctorEnvironment::default()
        .inspect_provider_entries(&fixture.provider)
        .unwrap();
    let names = entries
        .iter()
        .map(|entry| {
            entry
                .path
                .strip_prefix(&fixture.provider)
                .unwrap()
                .to_path_buf()
        })
        .collect::<Vec<_>>();

    assert_eq!(names, vec![PathBuf::from("visible.pdf")]);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn unsupported_hydration_detection_is_explicit_and_nonblocking() {
    let mut fixture = Fixture::new();
    fixture.configure();
    fs::write(fixture.provider.join("visible.pdf"), b"visible").unwrap();
    fixture.provider_entries =
        SystemDoctorEnvironment::default().inspect_provider_entries(&fixture.provider);

    let report = fixture.report();

    assert_eq!(
        check(&report, "provider_hydration_unsupported").status,
        DoctorStatus::Healthy
    );
    assert!(report.healthy);
}

#[test]
fn crashed_takeover_lock_is_inspected_as_stale() {
    let fixture = Fixture::new();
    fixture.make_healthy();
    let takeover = fixture.write_lock("lock.takeover", u32::MAX);

    let report = fixture.report();

    assert_eq!(
        check(&report, "stale_lock").path,
        Some(fs::canonicalize(takeover).unwrap())
    );
    assert!(!report.healthy);
    assert_eq!(report.next_action, NextAction::Repair);
}

#[test]
fn active_and_stale_locks_share_the_central_health_and_priority_rules() {
    let fixture = Fixture::new();
    fixture.make_healthy();
    fixture.write_lock("lock", std::process::id());
    fixture.write_lock("lock.takeover", u32::MAX);

    let report = fixture.report();

    assert_eq!(check(&report, "lock_active").status, DoctorStatus::Warning);
    assert_eq!(check(&report, "stale_lock").status, DoctorStatus::Warning);
    assert_eq!(check(&report, "stale_lock").area, DiagnosticArea::Lock);
    assert!(!report.healthy);
    assert_eq!(report.next_action, NextAction::Repair);
    assert_eq!(primary_issue(&report).code, "stale_lock");
}

#[test]
fn healthy_report_has_the_complete_stable_check_order() {
    let fixture = Fixture::new();
    fixture.make_healthy();

    let report = fixture.report();

    assert_eq!(
        report
            .checks
            .iter()
            .map(|check| check.code.as_str())
            .collect::<Vec<_>>(),
        vec![
            "product_version",
            "contract_version",
            "profile_valid",
            "repository_access",
            "provider_inbox",
            "provider_readable",
            "provider_writable",
            "provider_hydration",
            "hook_managed",
            "locks_clear",
        ]
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

fn primary_issue(report: &mko_core::doctor::DoctorReport) -> &mko_core::doctor::DoctorCheck {
    report
        .primary_issue()
        .expect("blocked report has a primary issue")
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
            snapshot_selected_git_files(root, &child, entries);
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

fn snapshot_selected_git_files(
    root: &Path,
    git_directory: &Path,
    entries: &mut Vec<(PathBuf, Vec<u8>)>,
) {
    for relative in ["HEAD", "config", "index", "packed-refs"] {
        let path = git_directory.join(relative);
        if path.is_file() {
            entries.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                fs::read(path).unwrap(),
            ));
        }
    }
}
