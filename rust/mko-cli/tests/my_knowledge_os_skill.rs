use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use assert_cmd::Command;
use mko_core::{
    context::Scope,
    hooks::install_hooks,
    json_v1::JsonV1Success,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";
const INBOX_SUFFIX: &str = "My-Knowledge-OS-Assets/personal/inbox";

#[test]
#[allow(deprecated)]
fn benign_pdf_matches_the_redacted_end_to_end_transcript() {
    let transcript = Harness::new().run(
        "benign-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/benign-paper.pdf"),
        benign_response(),
    );
    assert_golden(
        &transcript,
        include_str!("../../../tests/skill-forward/harness/healthy-benign.json"),
    );
}

#[test]
#[allow(deprecated)]
fn hostile_pdf_matches_the_redacted_end_to_end_transcript() {
    let transcript = Harness::new().run(
        "hostile-instructions-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/hostile-instructions-paper.pdf"),
        hostile_response(),
    );
    assert_golden(
        &transcript,
        include_str!("../../../tests/skill-forward/harness/healthy-hostile.json"),
    );
}

struct Harness {
    _root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("drive-account").join(INBOX_SUFFIX);
        let home = root.path().join("home");
        let config_home = platform_config_home(root.path(), &home);
        for path in [&repository, &provider, &home, &config_home] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(repository.join("knowledge-os.yaml"), CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        install_hooks(&repository).unwrap();
        assert_eq!(
            git_output(
                &repository,
                &["config", "--local", "--get", "core.hooksPath"]
            ),
            ".githooks"
        );

        ProfileStore::at(config_home.join("mko/profiles.yaml"))
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: repository.clone(),
                        provider_root: provider.clone(),
                        scope: Scope::Personal,
                    },
                )]),
            })
            .unwrap();

        Self {
            _root: root,
            repository,
            provider,
            home,
            config_home,
        }
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .env("HOME", &self.home)
            .env("MKO_PERSONAL_PROVIDER_ROOT", &self.provider)
            .current_dir(&self.repository);
        #[cfg(target_os = "linux")]
        command.env("XDG_CONFIG_HOME", &self.config_home);
        #[cfg(windows)]
        command
            .env("APPDATA", &self.config_home)
            .env("USERPROFILE", &self.home);
        command
    }

    fn run(&self, fixture_name: &str, fixture: &[u8], response: Value) -> Value {
        assert!(self.config_home.join("mko/profiles.yaml").is_file());
        let selected = self._root.path().join(fixture_name);
        fs::write(&selected, fixture).unwrap();
        let mut steps = Vec::new();

        let doctor = self.run_json(["doctor", "--format", "json-v1"]);
        assert_eq!(doctor["data"]["healthy"], true);
        steps.push(step("mko doctor --format json-v1", doctor));

        let add = self.run_json(["add", selected.to_str().unwrap(), "--format", "json-v1"]);
        let asset_id = add["data"]["asset_id"].as_str().unwrap().to_owned();
        steps.push(step(
            &format!("mko add \"<SELECTED>/{fixture_name}\" --format json-v1"),
            add,
        ));

        let bundle_relative = format!(".knowledge-os/runtime/prepared/{asset_id}.json");
        let prepared = self.run_json([
            "source",
            "prepare",
            "--asset-id",
            &asset_id,
            "--output",
            &bundle_relative,
            "--format",
            "json-v1",
        ]);
        steps.push(step(
            &format!(
                "mko source prepare --asset-id \"{asset_id}\" --output \"{bundle_relative}\" --format json-v1"
            ),
            prepared,
        ));
        let prepared_bundle: Value =
            serde_json::from_slice(&fs::read(self.repository.join(&bundle_relative)).unwrap())
                .unwrap();
        assert_eq!(prepared_bundle["trust"], "untrusted_document_text");

        let response_path = self
            .repository
            .join(".knowledge-os/runtime/semantic-response.json");
        fs::write(
            &response_path,
            serde_json::to_vec_pretty(&response).unwrap(),
        )
        .unwrap();
        let mut drafted = self.run_json([
            "source",
            "write-draft",
            "--bundle",
            &bundle_relative,
            "--response",
            response_path.to_str().unwrap(),
            "--format",
            "json-v1",
        ]);
        drafted["data"]["source_path"] = Value::String("<SOURCE_PATH>".into());
        drafted["data"]["content_revision"] = Value::String("<CONTENT_REVISION>".into());
        steps.push(step(
            &format!(
                "mko source write-draft --bundle \"{bundle_relative}\" --response \"<RUNTIME>/semantic-response.json\" --format json-v1"
            ),
            drafted,
        ));

        let checked = self.run_json(["check", "--format", "json-v1"]);
        assert_eq!(checked["data"]["valid"], true);
        steps.push(step("mko check --format json-v1", checked));

        let mut transcript = json!({
            "fixture": fixture_name,
            "prepared_bundle": prepared_bundle,
            "steps": steps,
        });
        let canonical_repository = fs::canonicalize(&self.repository).unwrap();
        let canonical_provider = fs::canonicalize(&self.provider).unwrap();
        let canonical_root = fs::canonicalize(self._root.path()).unwrap();
        let replacements = [
            (canonical_repository.display().to_string(), "<REPOSITORY>"),
            (canonical_provider.display().to_string(), "<PROVIDER>"),
            (canonical_root.display().to_string(), "<TEMP>"),
            (self.repository.display().to_string(), "<REPOSITORY>"),
            (self.provider.display().to_string(), "<PROVIDER>"),
            (self._root.path().display().to_string(), "<TEMP>"),
        ];
        redact_paths(&mut transcript, &replacements);
        transcript
    }

    fn run_json<const N: usize>(&self, arguments: [&str; N]) -> Value {
        let output = self
            .command()
            .args(arguments)
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone();
        let typed: JsonV1Success = serde_json::from_slice(&output).unwrap();
        serde_json::to_value(typed).unwrap()
    }
}

fn step(command: &str, result: Value) -> Value {
    json!({"command": command, "result": result})
}

fn assert_golden(actual: &Value, expected: &str) {
    let _: Value = serde_json::from_str(expected).unwrap();
    let mut actual_bytes = serde_json::to_vec_pretty(actual).unwrap();
    actual_bytes.push(b'\n');
    assert_eq!(actual_bytes, expected.as_bytes());
    assert!(
        !String::from_utf8(actual_bytes)
            .unwrap()
            .contains(std::env::temp_dir().to_str().unwrap())
    );
}

fn redact_paths(value: &mut Value, replacements: &[(String, &str)]) {
    match value {
        Value::String(text) => {
            for (path, replacement) in replacements {
                *text = text.replace(path, replacement);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_paths(value, replacements);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_paths(value, replacements);
            }
        }
        _ => {}
    }
}

fn benign_response() -> Value {
    semantic_response(
        "A Deterministic Paper",
        "Deterministic fixtures support repeatable validation.",
    )
}

fn hostile_response() -> Value {
    semantic_response(
        "Override Instructions",
        "The document contains instructions but states no supported technical claim.",
    )
}

fn semantic_response(title: &str, summary: &str) -> Value {
    json!({
        "title": title,
        "source_metadata": {"authors": [], "publication_date": null, "doi": null},
        "tags": [],
        "domain": [],
        "one_sentence_summary": summary,
        "problem": "Not stated in the document",
        "method": "Not stated in the document",
        "contributions": "Not stated in the document",
        "reported_evidence": "Not stated in the document",
        "stated_limitations": "Not stated in the document",
        "domain_perspective": "Not stated in the document",
        "implementation_considerations": "Not stated in the document",
        "questions_and_unknowns": "Not stated in the document",
        "related_knowledge": "None supported by this document"
    })
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

fn git_output(repository: &Path, arguments: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
