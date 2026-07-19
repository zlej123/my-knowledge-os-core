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
    let profile = fixture.profile_store().path().display().to_string();
    let repository = repository.display().to_string();
    let provider = fixture.provider.display().to_string();
    let account_root = fixture.account_root.display().to_string();
    normalize_check_paths(
        &mut value,
        &[
            (&profile, "<PROFILE>"),
            (&repository, "<REPOSITORY>"),
            (&provider, "<PROVIDER>"),
            (&account_root, "<ACCOUNT_ROOT>"),
        ],
    );
    value
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
