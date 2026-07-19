use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use mko_core::{
    context::{PlatformEnvironment, Scope},
    error::MkoError,
    hooks::{HookState, PRE_COMMIT_SCRIPT, inspect_hook},
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
    setup::{
        SetupRequest, SetupStep, SetupWriter, SystemSetupWriter, apply_setup,
        detect_google_drive_roots, preflight_setup,
    },
};
use tempfile::TempDir;

const KNOWLEDGE_CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";
const INBOX_SUFFIX: &str = "My-Knowledge-OS-Assets/personal/inbox";

#[derive(Clone)]
struct FakePlatform {
    config_home: PathBuf,
    home: PathBuf,
    current_dir: PathBuf,
    environment: HashMap<OsString, OsString>,
}

impl PlatformEnvironment for FakePlatform {
    fn config_home(&self) -> Result<PathBuf, MkoError> {
        Ok(self.config_home.clone())
    }

    fn home_dir(&self) -> Result<PathBuf, MkoError> {
        Ok(self.home.clone())
    }

    fn current_dir(&self) -> Result<PathBuf, MkoError> {
        Ok(self.current_dir.clone())
    }

    fn environment_value(&self, name: &OsStr) -> Option<OsString> {
        self.environment.get(name).cloned()
    }
}

struct Fixture {
    _root: TempDir,
    root: PathBuf,
    repository: PathBuf,
    platform: FakePlatform,
}

impl Fixture {
    fn new(windows: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("Personal KB 저장소");
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let current_dir = root.path().join("outside");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&current_dir).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), KNOWLEDGE_CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        let mut environment = HashMap::new();
        if windows {
            environment.insert(OsString::from("USERPROFILE"), home.as_os_str().to_owned());
        }
        Self {
            root: root.path().to_path_buf(),
            _root: root,
            repository,
            platform: FakePlatform {
                config_home,
                home,
                current_dir,
                environment,
            },
        }
    }

    fn mac_drive(&self, account: &str) -> PathBuf {
        let root = self
            .platform
            .home
            .join("Library/CloudStorage")
            .join(format!("GoogleDrive-{account}"))
            .join("My Drive");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn windows_drive(&self) -> PathBuf {
        let root = self.platform.home.join("Google Drive/My Drive");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn request(&self, drive_root: &Path) -> SetupRequest {
        SetupRequest::new(&self.repository).with_drive_root(drive_root)
    }

    fn store(&self) -> ProfileStore {
        ProfileStore::from_platform(&self.platform).unwrap()
    }

    fn inbox(&self, drive_root: &Path) -> PathBuf {
        drive_root.join(INBOX_SUFFIX)
    }
}

#[test]
fn detects_only_bounded_macos_google_drive_account_roots() {
    let fixture = Fixture::new(false);
    let alice = fixture.mac_drive("alice@example.com");
    let bob = fixture.mac_drive("bob@example.com");
    let decoy = fixture
        .platform
        .home
        .join("unrelated/deep/GoogleDrive-mallory/My Drive");
    fs::create_dir_all(&decoy).unwrap();

    let roots = detect_google_drive_roots(&fixture.platform).unwrap();

    assert_eq!(
        roots
            .iter()
            .map(|root| root.path.clone())
            .collect::<Vec<_>>(),
        vec![alice, bob]
    );
    assert!(roots.iter().all(|root| root.account_label.contains('@')));
    assert!(!roots.iter().any(|root| root.path == decoy));
}

#[test]
fn detects_only_bounded_windows_google_drive_roots() {
    let fixture = Fixture::new(true);
    let mirrored = fixture.windows_drive();
    let decoy = fixture.platform.home.join("nested/Google Drive/My Drive");
    fs::create_dir_all(&decoy).unwrap();

    let roots = detect_google_drive_roots(&fixture.platform).unwrap();

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, mirrored);
    assert_ne!(roots[0].path, decoy);
}

#[test]
fn ambiguous_accounts_require_an_explicit_known_selection() {
    let fixture = Fixture::new(false);
    fixture.mac_drive("alice@example.com");
    let bob = fixture.mac_drive("bob@example.com");
    let before = snapshot_tree(&fixture.root);

    let error =
        preflight_setup(SetupRequest::new(&fixture.repository), &fixture.platform).unwrap_err();
    assert_eq!(error.code(), "drive_root_ambiguous");
    assert_eq!(snapshot_tree(&fixture.root), before);

    let preflight = preflight_setup(fixture.request(&bob), &fixture.platform).unwrap();
    assert_eq!(preflight.context().provider_root, fixture.inbox(&bob));
}

#[test]
fn unknown_explicit_account_is_rejected_without_a_home_scan_or_mutation() {
    let fixture = Fixture::new(false);
    fixture.mac_drive("alice@example.com");
    let unknown = fixture.platform.home.join("other account/My Drive");
    fs::create_dir_all(&unknown).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&unknown), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "drive_root_unknown");
    assert_eq!(snapshot_tree(&fixture.root), before);
}

#[test]
fn custom_hook_path_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    git(
        &fixture.repository,
        &["config", "--local", "core.hooksPath", "custom-hooks"],
    );
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_conflict");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[test]
fn included_effective_hook_path_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let isolated_config = fixture.root.join("isolated-global.gitconfig");
    fs::write(&isolated_config, "[core]\n\thooksPath = inherited-hooks\n").unwrap();
    git(
        &fixture.repository,
        &[
            "config",
            "--local",
            "include.path",
            isolated_config.to_str().unwrap(),
        ],
    );
    assert!(
        git_output(
            &fixture.repository,
            &["config", "--local", "--get", "core.hooksPath"]
        )
        .is_empty()
    );
    assert_eq!(
        git_output(&fixture.repository, &["config", "--get", "core.hooksPath"]),
        "inherited-hooks"
    );
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_conflict");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[test]
fn worktree_hook_path_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    git(
        &fixture.repository,
        &["config", "extensions.worktreeConfig", "true"],
    );
    git(
        &fixture.repository,
        &["config", "--worktree", "core.hooksPath", "worktree-hooks"],
    );
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_conflict");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[test]
fn unmanaged_hook_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    fs::create_dir(fixture.repository.join(".githooks")).unwrap();
    fs::write(
        fixture.repository.join(".githooks/pre-commit"),
        "#!/bin/sh\necho user-owned\n",
    )
    .unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_conflict");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[cfg(unix)]
#[test]
fn symlinked_hook_directory_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let outside = fixture.root.join("outside-hooks");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.repository.join(".githooks")).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[cfg(unix)]
#[test]
fn symlinked_hook_file_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let outside = fixture.root.join("outside-pre-commit");
    fs::write(&outside, PRE_COMMIT_SCRIPT).unwrap();
    fs::create_dir(fixture.repository.join(".githooks")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.repository.join(".githooks/pre-commit")).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[test]
fn non_regular_hook_file_conflict_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    fs::create_dir_all(fixture.repository.join(".githooks/pre-commit")).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "hook_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[cfg(unix)]
#[test]
fn managed_hook_with_non_executable_mode_is_repaired() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let hook = fixture.repository.join(".githooks/pre-commit");
    fs::create_dir(hook.parent().unwrap()).unwrap();
    fs::write(&hook, PRE_COMMIT_SCRIPT).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o644)).unwrap();
    git(
        &fixture.repository,
        &["config", "--local", "core.hooksPath", ".githooks"],
    );

    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Missing
    );
    let outcome = apply_setup(
        preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
        &SystemSetupWriter,
    )
    .unwrap();

    assert!(outcome.is_complete());
    assert!(outcome.changed_steps.contains(&SetupStep::Hook));
    assert_ne!(fs::metadata(&hook).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Managed
    );
}

#[cfg(unix)]
#[test]
fn managed_hook_without_owner_execute_permission_is_repaired_on_rerun() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let hook = fixture.repository.join(".githooks/pre-commit");
    let first = apply_setup(
        preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
        &SystemSetupWriter,
    )
    .unwrap();
    assert!(first.is_complete());

    fs::set_permissions(&hook, fs::Permissions::from_mode(0o601)).unwrap();
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Missing
    );

    let rerun = apply_setup(
        preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
        &SystemSetupWriter,
    )
    .unwrap();

    assert!(rerun.is_complete());
    assert_ne!(fs::metadata(&hook).unwrap().permissions().mode() & 0o100, 0);
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Managed
    );
}

#[cfg(unix)]
#[test]
fn symlinked_profile_path_causes_zero_setup_mutations() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let store = fixture.store();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    let outside = fixture.root.join("outside-profile.yaml");
    fs::write(&outside, "preserve me").unwrap();
    std::os::unix::fs::symlink(&outside, store.path()).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert_eq!(fs::read_to_string(outside).unwrap(), "preserve me");
    assert!(!fixture.inbox(&drive).exists());
}

#[cfg(unix)]
#[test]
fn non_private_profile_permissions_cause_zero_setup_mutations() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let store = fixture.store();
    store
        .write(&profile(
            &fixture.repository,
            &fixture.root.join("old provider"),
        ))
        .unwrap();
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "profile_permissions_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[cfg(unix)]
#[test]
fn non_writable_drive_root_is_rejected_before_mutation() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    fs::set_permissions(&drive, fs::Permissions::from_mode(0o500)).unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "provider_permissions_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
}

#[cfg(unix)]
#[test]
fn inbox_symlink_escaping_selected_account_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let outside = fixture.root.join("outside-inbox");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(drive.join("My-Knowledge-OS-Assets/personal")).unwrap();
    std::os::unix::fs::symlink(
        &outside,
        drive.join("My-Knowledge-OS-Assets/personal/inbox"),
    )
    .unwrap();
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap_err();

    assert_eq!(error.code(), "provider_root_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.store().path().exists());
}

#[cfg(unix)]
#[test]
fn fresh_inbox_can_be_created_through_selected_account_symlink_alias() {
    let fixture = Fixture::new(false);
    let account_parent = fixture
        .platform
        .home
        .join("Library/CloudStorage/GoogleDrive-alice@example.com");
    let selected_alias = account_parent.join("My Drive");
    let actual_account = fixture.root.join("actual-drive-account");
    fs::create_dir_all(&account_parent).unwrap();
    fs::create_dir(&actual_account).unwrap();
    std::os::unix::fs::symlink(&actual_account, &selected_alias).unwrap();

    let preflight = preflight_setup(fixture.request(&selected_alias), &fixture.platform).unwrap();
    assert_eq!(
        preflight.context().provider_root,
        fixture.inbox(&selected_alias)
    );
    let outcome = apply_setup(preflight, &SystemSetupWriter).unwrap();

    assert!(outcome.is_complete());
    assert!(actual_account.join(INBOX_SUFFIX).is_dir());
    assert_eq!(
        fixture.store().read().unwrap().unwrap().profiles["personal"].provider_root,
        fixture.inbox(&selected_alias)
    );
}

#[test]
fn relative_profile_destination_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let mut platform = fixture.platform.clone();
    platform.config_home = PathBuf::from("relative-config-home");
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn profile_destination_inside_repository_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let mut platform = fixture.platform.clone();
    platform.config_home = fixture.repository.join("machine-config");
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn profile_destination_inside_synchronized_provider_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let mut platform = fixture.platform.clone();
    platform.config_home = drive.join("machine-config");
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn profile_destination_below_a_non_directory_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let config_file = fixture.root.join("not-a-config-directory");
    fs::write(&config_file, "preserve me").unwrap();
    let mut platform = fixture.platform.clone();
    platform.config_home = config_file;
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[cfg(unix)]
#[test]
fn profile_destination_symlinked_into_repository_is_rejected_before_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let config_alias = fixture.root.join("machine-config-alias");
    std::os::unix::fs::symlink(&fixture.repository, &config_alias).unwrap();
    let mut platform = fixture.platform.clone();
    platform.config_home = config_alias;
    let before = snapshot_tree(&fixture.root);

    let error = preflight_setup(fixture.request(&drive), &platform).unwrap_err();

    assert_eq!(error.code(), "profile_path_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn stale_hook_conflict_is_detected_before_first_setup_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    fs::create_dir(fixture.repository.join(".githooks")).unwrap();
    fs::write(
        fixture.repository.join(".githooks/pre-commit"),
        "#!/bin/sh\necho newly-owned\n",
    )
    .unwrap();
    let before_apply = snapshot_tree(&fixture.root);

    let error = apply_setup(preflight, &SystemSetupWriter).unwrap_err();

    assert_eq!(error.code(), "hook_conflict");
    assert_eq!(snapshot_tree(&fixture.root), before_apply);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[test]
fn stale_profile_drift_is_detected_before_first_setup_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    let other_provider = fixture.root.join("other-provider");
    fs::create_dir(&other_provider).unwrap();
    fixture
        .store()
        .write(&profile(&fixture.repository, &other_provider))
        .unwrap();
    let before_apply = snapshot_tree(&fixture.root);

    let error = apply_setup(preflight, &SystemSetupWriter).unwrap_err();

    assert_eq!(error.code(), "setup_stale");
    assert_eq!(snapshot_tree(&fixture.root), before_apply);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn stale_profile_byte_drift_is_detected_before_first_setup_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let other_provider = fixture.root.join("other-provider");
    fs::create_dir(&other_provider).unwrap();
    fixture
        .store()
        .write(&profile(&fixture.repository, &other_provider))
        .unwrap();
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    let original = fs::read_to_string(fixture.store().path()).unwrap();
    fs::write(
        fixture.store().path(),
        format!("# concurrent semantic-preserving rewrite\n{original}"),
    )
    .unwrap();
    let before_apply = snapshot_tree(&fixture.root);

    let error = apply_setup(preflight, &SystemSetupWriter).unwrap_err();

    assert_eq!(error.code(), "setup_stale");
    assert_eq!(snapshot_tree(&fixture.root), before_apply);
    assert!(!fixture.inbox(&drive).exists());
}

#[test]
fn stale_repository_drift_is_detected_before_first_setup_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    fs::write(
        fixture.repository.join("knowledge-os.yaml"),
        KNOWLEDGE_CONFIG.replace("scope: personal", "scope: team"),
    )
    .unwrap();
    let before_apply = snapshot_tree(&fixture.root);

    let error = apply_setup(preflight, &SystemSetupWriter).unwrap_err();

    assert_eq!(error.code(), "config_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before_apply);
    assert!(!fixture.inbox(&drive).exists());
    assert!(!fixture.store().path().exists());
}

#[cfg(unix)]
#[test]
fn stale_provider_escape_is_detected_before_first_setup_mutation() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    let outside = fixture.root.join("late-outside-inbox");
    fs::create_dir(&outside).unwrap();
    fs::create_dir_all(drive.join("My-Knowledge-OS-Assets/personal")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.inbox(&drive)).unwrap();
    let before_apply = snapshot_tree(&fixture.root);

    let error = apply_setup(preflight, &SystemSetupWriter).unwrap_err();

    assert_eq!(error.code(), "provider_root_invalid");
    assert_eq!(snapshot_tree(&fixture.root), before_apply);
    assert!(!fixture.store().path().exists());
}

#[test]
fn inbox_creation_survives_an_injected_profile_write_failure() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let previous_provider = fixture.root.join("previous provider");
    fs::create_dir(&previous_provider).unwrap();
    fixture
        .store()
        .write(&profile(&fixture.repository, &previous_provider))
        .unwrap();
    let profile_before = fs::read(fixture.store().path()).unwrap();
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    let writer = FailingWriter::at(SetupStep::Profile);

    let outcome = apply_setup(preflight, &writer).unwrap();

    assert_eq!(outcome.completed_steps, vec![SetupStep::Inbox]);
    assert_eq!(
        outcome.incomplete_steps,
        vec![SetupStep::Profile, SetupStep::Hook]
    );
    assert_eq!(outcome.failure.as_ref().unwrap().step, SetupStep::Profile);
    assert_eq!(outcome.failure.as_ref().unwrap().code, "injected_failure");
    assert!(fixture.inbox(&drive).is_dir());
    assert_eq!(fs::read(fixture.store().path()).unwrap(), profile_before);
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Missing
    );
}

#[test]
fn profile_success_survives_an_injected_hook_failure() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
    let writer = FailingWriter::at(SetupStep::Hook);

    let outcome = apply_setup(preflight, &writer).unwrap();

    assert_eq!(
        outcome.completed_steps,
        vec![SetupStep::Inbox, SetupStep::Profile]
    );
    assert_eq!(outcome.incomplete_steps, vec![SetupStep::Hook]);
    assert_eq!(outcome.failure.as_ref().unwrap().step, SetupStep::Hook);
    let stored = fixture.store().read().unwrap().unwrap();
    assert_eq!(
        stored.profiles["personal"].provider_root,
        fixture.inbox(&drive)
    );
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Missing
    );
}

#[test]
fn outcome_reports_partial_and_postcommit_mutations_and_rerun_converges() {
    for point in [
        MutationPoint::InboxParentCreated,
        MutationPoint::InboxCreated,
        MutationPoint::ProfileDirectoryCreated,
        MutationPoint::ProfileCommitted,
        MutationPoint::HookDirectoryCreated,
        MutationPoint::HookFileWritten,
        MutationPoint::HookMadeExecutable,
        MutationPoint::HookConfigured,
    ] {
        let fixture = Fixture::new(false);
        let drive = fixture.mac_drive("alice@example.com");
        let preflight = preflight_setup(fixture.request(&drive), &fixture.platform).unwrap();
        let outcome = apply_setup(preflight, &MutatingThenFailingWriter::at(point)).unwrap();

        let expected_failure_step = match point {
            MutationPoint::InboxParentCreated | MutationPoint::InboxCreated => SetupStep::Inbox,
            MutationPoint::ProfileDirectoryCreated | MutationPoint::ProfileCommitted => {
                SetupStep::Profile
            }
            MutationPoint::HookDirectoryCreated
            | MutationPoint::HookFileWritten
            | MutationPoint::HookMadeExecutable
            | MutationPoint::HookConfigured => SetupStep::Hook,
        };
        assert_eq!(
            outcome.failure.as_ref().map(|failure| failure.step),
            Some(expected_failure_step),
            "{point:?}"
        );
        assert!(
            outcome.changed_steps.contains(&expected_failure_step),
            "{point:?}: changed_steps={:?}",
            outcome.changed_steps
        );

        match point {
            MutationPoint::InboxParentCreated => {
                assert!(!outcome.completed_steps.contains(&SetupStep::Inbox));
                assert!(outcome.incomplete_steps.contains(&SetupStep::Inbox));
            }
            MutationPoint::InboxCreated => {
                assert!(outcome.completed_steps.contains(&SetupStep::Inbox));
                assert!(!outcome.incomplete_steps.contains(&SetupStep::Inbox));
            }
            MutationPoint::ProfileDirectoryCreated => {
                assert!(!outcome.completed_steps.contains(&SetupStep::Profile));
                assert!(outcome.incomplete_steps.contains(&SetupStep::Profile));
            }
            MutationPoint::ProfileCommitted => {
                assert!(outcome.completed_steps.contains(&SetupStep::Profile));
                assert!(!outcome.incomplete_steps.contains(&SetupStep::Profile));
            }
            MutationPoint::HookDirectoryCreated
            | MutationPoint::HookFileWritten
            | MutationPoint::HookMadeExecutable => {
                assert!(!outcome.completed_steps.contains(&SetupStep::Hook));
                assert!(outcome.incomplete_steps.contains(&SetupStep::Hook));
            }
            MutationPoint::HookConfigured => {
                assert!(outcome.completed_steps.contains(&SetupStep::Hook));
                assert!(!outcome.incomplete_steps.contains(&SetupStep::Hook));
            }
        }

        let rerun = apply_setup(
            preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
            &SystemSetupWriter,
        )
        .unwrap();
        assert!(rerun.is_complete(), "{point:?}: {rerun:?}");
        assert!(fixture.inbox(&drive).is_dir(), "{point:?}");
        assert_eq!(
            fixture.store().read().unwrap().unwrap().profiles["personal"].provider_root,
            fixture.inbox(&drive),
            "{point:?}"
        );
        assert_eq!(
            inspect_hook(&fixture.repository).unwrap().state,
            HookState::Managed,
            "{point:?}"
        );
    }
}

#[test]
fn setup_rerun_is_safe_and_performs_no_redundant_writes() {
    let fixture = Fixture::new(false);
    let drive = fixture.mac_drive("alice@example.com");
    let writer = CountingWriter::default();

    let first = apply_setup(
        preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
        &writer,
    )
    .unwrap();
    assert_eq!(
        first.changed_steps,
        vec![SetupStep::Inbox, SetupStep::Profile, SetupStep::Hook]
    );
    assert_eq!(
        writer.calls(),
        vec![SetupStep::Inbox, SetupStep::Profile, SetupStep::Hook]
    );
    assert_eq!(
        inspect_hook(&fixture.repository).unwrap().state,
        HookState::Managed
    );
    assert_eq!(
        fs::read_to_string(fixture.repository.join(".githooks/pre-commit")).unwrap(),
        PRE_COMMIT_SCRIPT
    );

    writer.clear();
    let before = snapshot_tree(&fixture.root);
    let second = apply_setup(
        preflight_setup(fixture.request(&drive), &fixture.platform).unwrap(),
        &writer,
    )
    .unwrap();

    assert_eq!(
        second.completed_steps,
        vec![SetupStep::Inbox, SetupStep::Profile, SetupStep::Hook]
    );
    assert!(second.changed_steps.is_empty());
    assert!(second.incomplete_steps.is_empty());
    assert!(second.failure.is_none());
    assert!(writer.calls().is_empty());
    assert_eq!(snapshot_tree(&fixture.root), before);
}

struct FailingWriter {
    fail_at: SetupStep,
    system: SystemSetupWriter,
}

impl FailingWriter {
    fn at(step: SetupStep) -> Self {
        Self {
            fail_at: step,
            system: SystemSetupWriter,
        }
    }

    fn check(&self, step: SetupStep) -> Result<(), MkoError> {
        if self.fail_at == step {
            Err(MkoError::new("injected_failure", "test failure"))
        } else {
            Ok(())
        }
    }
}

impl SetupWriter for FailingWriter {
    fn create_inbox(&self, path: &Path) -> Result<(), MkoError> {
        self.check(SetupStep::Inbox)?;
        self.system.create_inbox(path)
    }

    fn write_profile(
        &self,
        store: &ProfileStore,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError> {
        self.check(SetupStep::Profile)?;
        self.system.write_profile(store, profile)
    }

    fn install_hook(&self, repository_root: &Path) -> Result<(), MkoError> {
        self.check(SetupStep::Hook)?;
        self.system.install_hook(repository_root)
    }
}

#[derive(Clone, Copy, Debug)]
enum MutationPoint {
    InboxParentCreated,
    InboxCreated,
    ProfileDirectoryCreated,
    ProfileCommitted,
    HookDirectoryCreated,
    HookFileWritten,
    HookMadeExecutable,
    HookConfigured,
}

struct MutatingThenFailingWriter {
    point: MutationPoint,
    system: SystemSetupWriter,
}

impl MutatingThenFailingWriter {
    fn at(point: MutationPoint) -> Self {
        Self {
            point,
            system: SystemSetupWriter,
        }
    }

    fn failure(&self) -> MkoError {
        MkoError::new("injected_postmutation_failure", format!("{:?}", self.point))
    }
}

impl SetupWriter for MutatingThenFailingWriter {
    fn create_inbox(&self, path: &Path) -> Result<(), MkoError> {
        match self.point {
            MutationPoint::InboxParentCreated => {
                fs::create_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
                Err(self.failure())
            }
            MutationPoint::InboxCreated => {
                fs::create_dir_all(path).unwrap();
                Err(self.failure())
            }
            _ => self.system.create_inbox(path),
        }
    }

    fn write_profile(
        &self,
        store: &ProfileStore,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError> {
        match self.point {
            MutationPoint::ProfileDirectoryCreated => {
                let parent = store.path().parent().unwrap();
                fs::create_dir_all(parent).unwrap();
                set_private_directory(parent);
                Err(self.failure())
            }
            MutationPoint::ProfileCommitted => {
                self.system.write_profile(store, profile)?;
                Err(self.failure())
            }
            _ => self.system.write_profile(store, profile),
        }
    }

    fn install_hook(&self, repository_root: &Path) -> Result<(), MkoError> {
        let directory = repository_root.join(".githooks");
        let hook = directory.join("pre-commit");
        match self.point {
            MutationPoint::HookDirectoryCreated => {
                fs::create_dir(&directory).unwrap();
                Err(self.failure())
            }
            MutationPoint::HookFileWritten => {
                fs::create_dir(&directory).unwrap();
                fs::write(&hook, PRE_COMMIT_SCRIPT).unwrap();
                Err(self.failure())
            }
            MutationPoint::HookMadeExecutable => {
                fs::create_dir(&directory).unwrap();
                fs::write(&hook, PRE_COMMIT_SCRIPT).unwrap();
                set_executable(&hook);
                Err(self.failure())
            }
            MutationPoint::HookConfigured => {
                self.system.install_hook(repository_root)?;
                Err(self.failure())
            }
            _ => self.system.install_hook(repository_root),
        }
    }
}

#[derive(Default)]
struct CountingWriter {
    calls: Mutex<Vec<SetupStep>>,
    system: SystemSetupWriter,
}

impl CountingWriter {
    fn calls(&self) -> Vec<SetupStep> {
        self.calls.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }

    fn record(&self, step: SetupStep) {
        self.calls.lock().unwrap().push(step);
    }
}

impl SetupWriter for CountingWriter {
    fn create_inbox(&self, path: &Path) -> Result<(), MkoError> {
        self.record(SetupStep::Inbox);
        self.system.create_inbox(path)
    }

    fn write_profile(
        &self,
        store: &ProfileStore,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError> {
        self.record(SetupStep::Profile);
        self.system.write_profile(store, profile)
    }

    fn install_hook(&self, repository_root: &Path) -> Result<(), MkoError> {
        self.record(SetupStep::Hook);
        self.system.install_hook(repository_root)
    }
}

fn profile(repository_root: &Path, provider_root: &Path) -> MachineProfileFile {
    MachineProfileFile {
        schema_version: 1,
        default_profile: "personal".into(),
        profiles: BTreeMap::from([(
            "personal".into(),
            PersonalProfile {
                repository_root: repository_root.to_path_buf(),
                provider_root: provider_root.to_path_buf(),
                scope: Scope::Personal,
            },
        )]),
    }
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let entry_path = entry.path();
            let relative = entry_path.strip_prefix(root).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&entry_path).unwrap();
            if metadata.file_type().is_symlink() {
                output.insert(
                    relative,
                    fs::read_link(&entry_path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                );
            } else if metadata.is_dir() {
                output.insert(relative, Vec::new());
                visit(root, &entry_path, output);
            } else {
                output.insert(relative, fs::read(&entry_path).unwrap());
            }
        }
    }

    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}

#[cfg(unix)]
fn set_private_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) {}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

fn git_output(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    if output.status.code() == Some(1) {
        return String::new();
    }
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success());
}
