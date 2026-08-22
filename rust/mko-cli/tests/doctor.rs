use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use mko_core::{
    context::Scope,
    hooks::install_hooks,
    json_v1::JsonV1Success,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
};
use serde_json::Value;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";
const INBOX_SUFFIX: &str = "My-Knowledge-OS-Assets/personal/inbox";

#[test]
#[allow(deprecated)]
fn doctor_human_output_is_korean_first_without_leaking_stable_codes() {
    let fixture = Fixture::new();
    fixture.write_unreadable_profile();

    let human = fixture
        .command()
        .args(["doctor", "--repo"])
        .arg(&fixture.repository)
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();

    assert!(human.contains("설정"), "{human}");
    assert!(!human.contains("profile_unreadable"), "{human}");
}

#[test]
#[allow(deprecated)]
fn healthy_and_blocked_json_match_normalized_full_goldens_and_schema() {
    let healthy = Fixture::new();
    healthy.make_healthy();
    let healthy_output = healthy.json_output();
    assert_json_golden(
        &healthy,
        &healthy_output,
        include_str!("../../../tests/fixtures/json-v1/doctor-healthy.json"),
    );

    let blocked = Fixture::new();
    blocked.write_unreadable_profile();
    blocked.use_account_root_as_provider();
    let blocked_output = blocked.json_output();
    assert_json_golden(
        &blocked,
        &blocked_output,
        include_str!("../../../tests/fixtures/json-v1/doctor-blocked.json"),
    );
}

#[test]
fn path_normalization_only_changes_structural_check_paths() {
    let windows_path = r#"C:\Users\A "quoted"\My-Knowledge-OS-Assets\personal\inbox"#;
    let mut value = serde_json::json!({
        "data": {
            "checks": [{
                "path": windows_path,
                "message": format!("keep literal {windows_path}")
            }]
        }
    });

    normalize_check_paths(&mut value, &[(windows_path, "<PROVIDER>")]);

    assert_eq!(value["data"]["checks"][0]["path"], "<PROVIDER>");
    assert_eq!(
        value["data"]["checks"][0]["message"],
        format!("keep literal {windows_path}")
    );
}

struct Fixture {
    root: PathBuf,
    repository: PathBuf,
    account_root: PathBuf,
    provider: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mko-cli-doctor-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repository = root.join("repository");
        let account_root = root.join("provider-account");
        let provider = account_root.join(INBOX_SUFFIX);
        let home = root.join("home");
        let config_home = platform_config_home(&root, &home);
        for path in [&repository, &provider, &home, &config_home] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(repository.join("knowledge-os.yaml"), CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        Self {
            root,
            repository,
            account_root,
            provider,
            home,
            config_home,
        }
    }

    fn profile_store(&self) -> ProfileStore {
        ProfileStore::at(self.config_home.join("mko/profiles.yaml"))
    }

    fn make_healthy(&self) {
        self.profile_store()
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
        install_hooks(&self.repository).unwrap();
    }

    fn write_unreadable_profile(&self) {
        let store = self.profile_store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "schema_version: nope\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                store.path().parent().unwrap(),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn use_account_root_as_provider(&self) {
        fs::write(self.root.join("use-account-root"), []).unwrap();
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command.env("HOME", &self.home);
        command.env("MKO_PERSONAL_PROVIDER_ROOT", self.command_provider());
        #[cfg(target_os = "linux")]
        command.env("XDG_CONFIG_HOME", &self.config_home);
        #[cfg(windows)]
        {
            command.env("APPDATA", &self.config_home);
            command.env("USERPROFILE", &self.home);
        }
        command
    }

    fn command_provider(&self) -> &Path {
        if self.root.join("use-account-root").exists() {
            &self.account_root
        } else {
            &self.provider
        }
    }

    fn json_output(&self) -> Vec<u8> {
        self.command()
            .args(["doctor", "--repo"])
            .arg(&self.repository)
            .args(["--format", "json-v1"])
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_json_golden(fixture: &Fixture, output: &[u8], golden: &str) {
    let typed: JsonV1Success = serde_json::from_slice(output).unwrap();
    let actual_schema_value = serde_json::to_value(&typed).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/machine-output-v1.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    if let Err(error) = validator.validate(&actual_schema_value) {
        panic!("doctor JSON failed schema validation: {error}");
    }

    let normalized = normalize_paths(fixture, output);
    let expected: Value = serde_json::from_str(golden).unwrap();
    assert_eq!(normalized, expected);
}

fn normalize_paths(fixture: &Fixture, output: &[u8]) -> Value {
    let mut value: Value = serde_json::from_slice(output).unwrap();
    let repository = fs::canonicalize(&fixture.repository).unwrap();
    let repository = repository.display().to_string();
    let provider = fixture.provider.display().to_string();
    let account_root = fixture.account_root.display().to_string();
    normalize_profile_check_path(&mut value);
    normalize_check_paths(
        &mut value,
        &[
            (&repository, "<REPOSITORY>"),
            (&provider, "<PROVIDER>"),
            (&account_root, "<ACCOUNT_ROOT>"),
        ],
    );
    value
}

fn normalize_profile_check_path(value: &mut Value) {
    let Some(checks) = value
        .get_mut("data")
        .and_then(|data| data.get_mut("checks"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for check in checks {
        if check
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| code.starts_with("profile_"))
            && check.get("path").is_some_and(Value::is_string)
        {
            check["path"] = Value::String("<PROFILE>".into());
        }
    }
}

fn normalize_check_paths(value: &mut Value, replacements: &[(&str, &str)]) {
    let Some(checks) = value
        .get_mut("data")
        .and_then(|data| data.get_mut("checks"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for check in checks {
        let Some(path) = check.get_mut("path") else {
            continue;
        };
        let Some(path_text) = path.as_str() else {
            continue;
        };
        if let Some((_, replacement)) = replacements
            .iter()
            .find(|(candidate, _)| *candidate == path_text)
        {
            *path = Value::String((*replacement).into());
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_config_home(_: &Path, home: &Path) -> PathBuf {
    home.join("Library/Application Support")
}

#[cfg(target_os = "linux")]
fn platform_config_home(root: &Path, _: &Path) -> PathBuf {
    root.join("config")
}

#[cfg(windows)]
fn platform_config_home(root: &Path, _: &Path) -> PathBuf {
    root.join("config")
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = ProcessCommand::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success());
}

// Doctor is the tool an owner reaches for when something is wrong. Reading only
// the v0.1 configuration made it call a healthy current repository incompatible
// and send them back to setup — the opposite of recovery.
#[test]
#[allow(deprecated)]
fn a_current_generation_repository_is_diagnosed_as_compatible() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["doctor", "--repo"])
        .arg(&repository)
        .args(["--format", "json-v1"])
        .assert()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&output).unwrap();
    let repository_check = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| {
            check["code"] == "repository_access" || check["code"] == "repository_incompatible"
        })
        .expect("doctor must report on the repository");
    assert_eq!(
        repository_check["code"], "repository_access",
        "a scaffolded current-generation KB must not be reported incompatible"
    );
    assert_eq!(repository_check["status"], "healthy");
}

// Accepting the flag and answering with a human line at exit 0 left an agent
// parsing prose or, worse, treating a diagnosis as success.
#[test]
#[allow(deprecated)]
fn doctor_answers_json_v2_with_a_typed_envelope() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["doctor", "--repo"])
        .arg(&repository)
        .args(["--format", "json-v2"])
        .assert()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&output)
        .expect("json-v2 doctor output must be JSON, not a human line");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "doctor");
    assert_eq!(report["result"], "ok");
    assert!(report["data"]["healthy"].is_boolean());
    let checks = report["data"]["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    for check in checks {
        assert!(check["code"].is_string());
        assert!(["healthy", "warning", "blocked"].contains(&check["status"].as_str().unwrap()));
        assert!(check.get("path").is_some(), "path must be present or null");
        assert!(
            check.get("next_action").is_some(),
            "next_action must be present or null"
        );
    }
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&report));
}

// Setup completes a local KB without Git on purpose — a remote is offered
// afterwards and never required. Diagnosing that supported shape as a broken
// hook made the owner's healthy repository report unhealthy, with a repair
// there was nothing to repair.
#[test]
#[allow(deprecated)]
fn a_knowledge_base_without_git_is_not_reported_as_damaged() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();
    assert!(!repository.join(".git").exists());

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["doctor", "--repo"])
        .arg(&repository)
        .args(["--format", "json-v2"])
        .assert()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&output).unwrap();
    let hook = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["code"].as_str().unwrap().starts_with("hook_"))
        .expect("doctor must say something about hooks");
    assert_eq!(hook["code"], "hook_not_applicable");
    assert_eq!(hook["status"], "healthy");
    assert_eq!(hook["next_action"], Value::Null);
}

// A v0.3 knowledge base under Git, with no hook, is the sound state: the
// managed hook runs `mko check`, which reads v0.1 records only. Doctor used to
// demand the hook here and call it a repair. The owner followed that advice on
// the live knowledge base, and every commit then failed front_matter_invalid
// on revisions the Core itself had written.
#[test]
#[allow(deprecated)]
fn a_v03_knowledge_base_under_git_is_not_told_to_install_the_v01_hook() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);

    let report: Value = serde_json::from_slice(
        &Command::cargo_bin("mko")
            .unwrap()
            .args(["doctor", "--repo"])
            .arg(&repository)
            .args(["--format", "json-v2"])
            .assert()
            .get_output()
            .stdout,
    )
    .unwrap();

    let hook = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["code"].as_str().unwrap().starts_with("hook_"))
        .expect("doctor must say something about hooks");
    assert_eq!(hook["code"], "hook_not_applicable", "{report}");
    assert_eq!(hook["status"], "healthy", "{report}");
    assert_eq!(hook["next_action"], Value::Null, "{report}");
}

// The opposite case is the one that actually bit: the v0.1 hook installed in a
// v0.3 knowledge base. That is not "managed, healthy" — it rejects every
// revision, so it must be reported as the defect it is, with the way out.
#[test]
#[allow(deprecated)]
fn a_v01_hook_inside_a_v03_knowledge_base_is_reported_as_incompatible() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    let hooks = repository.join(".githooks");
    fs::create_dir(&hooks).unwrap();
    fs::write(hooks.join("pre-commit"), mko_core::hooks::PRE_COMMIT_SCRIPT).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &repository,
        &["config", "--local", "core.hooksPath", ".githooks"],
    );

    let report: Value = serde_json::from_slice(
        &Command::cargo_bin("mko")
            .unwrap()
            .args(["doctor", "--repo"])
            .arg(&repository)
            .args(["--format", "json-v2"])
            .assert()
            .get_output()
            .stdout,
    )
    .unwrap();

    let hook = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["code"].as_str().unwrap().starts_with("hook_"))
        .expect("doctor must say something about hooks");
    assert_eq!(hook["code"], "hook_incompatible", "{report}");
    assert_eq!(hook["status"], "blocked", "{report}");
    assert_eq!(hook["next_action"], "repair", "{report}");
    assert_eq!(report["data"]["healthy"], false, "{report}");
}

// And the installer must refuse to create that state in the first place,
// leaving nothing behind: no .githooks, no core.hooksPath.
#[test]
#[allow(deprecated)]
fn hooks_install_refuses_a_v03_knowledge_base_and_leaves_nothing_behind() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["hooks", "install", "--repo"])
        .arg(&repository)
        .assert()
        .failure()
        .get_output()
        .clone();

    let reported = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(reported.contains("hook_not_supported"), "{reported}");
    assert!(
        !repository.join(".githooks").exists(),
        "a refused install must not leave a hook directory behind"
    );
    let hooks_path = std::process::Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert!(
        !hooks_path.status.success(),
        "a refused install must not set core.hooksPath"
    );
}
