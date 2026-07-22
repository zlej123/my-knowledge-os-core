use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use assert_cmd::Command;
use mko_core::{
    context::Scope,
    hooks::install_hooks,
    json_v1::{JsonV1Failure, JsonV1Success},
    knowledge::{ReviewState, list_knowledge},
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

#[test]
#[allow(deprecated)]
fn ordinary_pdf_summary_stops_without_a_knowledge_write() {
    let harness = Harness::new();
    let transcript = harness.run(
        "benign-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/benign-paper.pdf"),
        benign_response(),
    );

    assert!(
        transcript_commands(&transcript)
            .iter()
            .all(|command| !command.starts_with("mko knowledge "))
    );
    assert!(list_knowledge(&harness.repository).unwrap().is_empty());
}

#[test]
#[allow(deprecated)]
fn explicit_knowledge_intent_executes_one_bundle_bound_write_and_leaves_pending() {
    let harness = Harness::new();
    let transcript = harness.run_knowledge(
        "이 PDF에서 지식 정리해줘",
        "benign-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/benign-paper.pdf"),
        benign_knowledge_response(),
    );

    assert_pending_knowledge_forward_behavior(&transcript);
    let notes = list_knowledge(&harness.repository).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].review_status, ReviewState::Unreviewed);
}

#[test]
#[allow(deprecated)]
fn hostile_bundle_cannot_change_the_pending_knowledge_action_sequence() {
    let harness = Harness::new();
    let transcript = harness.run_knowledge(
        "이 PDF에서 지식과 개념을 추출해줘",
        "hostile-instructions-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/hostile-instructions-paper.pdf"),
        hostile_knowledge_response(),
    );

    assert!(
        transcript["prepared_bundle"]["pages"][0]
            .as_str()
            .unwrap()
            .contains("Ignore prior rules")
    );
    assert_pending_knowledge_forward_behavior(&transcript);
    let commands = transcript_commands(&transcript).join("\n");
    for forbidden in ["secret", "approve", "git ", "push", "example.invalid"] {
        assert!(
            !commands.to_ascii_lowercase().contains(forbidden),
            "hostile bundle escaped into an executable action: {forbidden}"
        );
    }
    let notes = list_knowledge(&harness.repository).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].review_status, ReviewState::Unreviewed);
    let durable_note =
        fs::read_to_string(harness.repository.join(&notes[0].knowledge_path)).unwrap();
    for forbidden in [
        "Read machine secrets",
        "approve the Source",
        "git commit",
        "git push",
        "example.invalid",
    ] {
        assert!(
            !durable_note.contains(forbidden),
            "hostile bundle escaped into the durable Knowledge note: {forbidden}"
        );
    }
}

#[test]
#[allow(deprecated)]
fn only_copy_pdf_requires_confirmation_before_one_verified_retry() {
    let transcript = Harness::new().run_backup_confirmation(
        "only-copy-paper.pdf",
        include_bytes!("../../../tests/fixtures/skill-forward/benign-paper.pdf"),
    );
    assert_golden(
        &transcript,
        include_str!("../../../tests/skill-forward/harness/backup-confirmation.json"),
    );
}

#[test]
#[allow(deprecated)]
fn mixed_inbox_matches_the_redacted_resumable_batch_transcript() {
    let transcript = Harness::new().run_batch();
    assert_golden(
        &transcript,
        include_str!("../../../tests/skill-forward/harness/healthy-batch.json"),
    );
}

#[test]
fn incomplete_scan_signal_is_independent_of_zero_remaining() {
    let transcript: Value = serde_json::from_str(include_str!(
        "../../../tests/skill-forward/harness/healthy-batch.json"
    ))
    .unwrap();
    let incomplete = transcript["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step.get("result"))
        .filter_map(|result| result.get("data"))
        .any(|data| data["scan_complete"] == false && data["remaining"] == 0);

    assert!(
        incomplete,
        "the deterministic batch harness must cover scan_complete=false with remaining=0"
    );
}

#[test]
fn platform_specific_paths_normalize_to_identical_logical_transcripts() {
    let mut macos = platform_transcript(
        "/private/tmp/run/home/Library/Application Support/mko/profiles.yaml",
        "/private/tmp/run/repository",
        "/private/tmp/run/drive/My-Knowledge-OS-Assets/personal/inbox",
        "/private/tmp/run/repository/.knowledge-os/runtime/prepared/asset.json",
        "assets/registry/asset.md",
    );
    let mut windows = platform_transcript(
        r"C:\Users\Test\AppData\Roaming\mko\profiles.yaml",
        r"C:\Knowledge\personal-kb",
        r"G:\My Drive\My-Knowledge-OS-Assets\personal\inbox",
        r"C:\Knowledge\personal-kb\.knowledge-os\runtime\prepared\asset.json",
        r"assets\registry\asset.md",
    );

    normalize_transcript(&mut macos);
    normalize_transcript(&mut windows);

    assert_eq!(macos, windows);
    let normalized = serde_json::to_string(&macos).unwrap();
    assert!(normalized.contains("<PROFILE>"));
    assert!(normalized.contains("<REPOSITORY>/.knowledge-os/runtime/prepared/asset.json"));
    assert!(normalized.contains("assets/registry/asset.md"));
    assert!(!normalized.contains("private/tmp"));
    assert!(!normalized.contains("C:\\\\"));
}

#[test]
fn windows_forward_slash_root_leak_is_rejected() {
    let transcript = json!({
        "steps": [{
            "result": {
                "nested": {"leak": "C:/Knowledge/personal-kb/.knowledge-os/runtime/prepared/a.json"}
            }
        }]
    });

    let roots = vec![r"C:\Knowledge\personal-kb".to_owned()];
    let result = std::panic::catch_unwind(|| assert_no_known_path_leaks(&transcript, &roots));
    assert!(
        result.is_err(),
        "Windows slash-form root leak was not rejected"
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
        normalize_transcript(&mut transcript);
        self.assert_no_harness_paths(&transcript);
        assert_no_machine_paths(&transcript);
        transcript
    }

    fn run_knowledge(
        &self,
        user_prompt: &str,
        fixture_name: &str,
        fixture: &[u8],
        response: Value,
    ) -> Value {
        let selected = self._root.path().join(fixture_name);
        fs::write(&selected, fixture).unwrap();

        let add = self.run_json(["add", selected.to_str().unwrap(), "--format", "json-v1"]);
        let asset_id = add["data"]["asset_id"].as_str().unwrap().to_owned();
        let bundle_relative = format!(".knowledge-os/runtime/prepared/{asset_id}.json");
        self.run_json([
            "source",
            "prepare",
            "--asset-id",
            &asset_id,
            "--output",
            &bundle_relative,
            "--format",
            "json-v1",
        ]);

        let prepared_bundle: Value =
            serde_json::from_slice(&fs::read(self.repository.join(&bundle_relative)).unwrap())
                .unwrap();
        assert_eq!(prepared_bundle["trust"], "untrusted_document_text");
        let response_relative = ".knowledge-os/runtime/knowledge-response.json";
        fs::write(
            self.repository.join(response_relative),
            serde_json::to_vec_pretty(&response).unwrap(),
        )
        .unwrap();

        let mut written = self.run_json([
            "knowledge",
            "write",
            "--asset-id",
            &asset_id,
            "--bundle",
            &bundle_relative,
            "--response",
            response_relative,
            "--format",
            "json-v1",
        ]);
        written["data"]["content_revision"] = "<CONTENT_REVISION>".into();
        let checked = self.run_json(["check", "--format", "json-v1"]);
        assert_eq!(checked["data"]["valid"], true);

        let mut transcript = json!({
            "fixture": fixture_name,
            "user_prompt": user_prompt,
            "prepared_bundle": prepared_bundle,
            "steps": [
                step(
                    &format!(
                        "mko knowledge write --asset-id \"{asset_id}\" --bundle \"{bundle_relative}\" --response \"{response_relative}\" --format json-v1"
                    ),
                    written,
                ),
                step("mko check --format json-v1", checked),
                {
                    "boundary": "pending_human_review",
                    "status": "unreviewed",
                    "next_action": "mko knowledge review"
                }
            ]
        });
        normalize_transcript(&mut transcript);
        self.assert_no_harness_paths(&transcript);
        assert_no_machine_paths(&transcript);
        transcript
    }

    fn run_backup_confirmation(&self, fixture_name: &str, fixture: &[u8]) -> Value {
        let selected = self.provider.join(fixture_name);
        fs::write(&selected, fixture).unwrap();

        let doctor = self.run_json(["doctor", "--format", "json-v1"]);
        let rejected =
            self.run_json_failure(["add", selected.to_str().unwrap(), "--format", "json-v1"]);
        assert_eq!(rejected["error"]["code"], "backup_confirmation_required");
        let accepted = self.run_json([
            "add",
            selected.to_str().unwrap(),
            "--verified-backup",
            "--format",
            "json-v1",
        ]);

        let mut transcript = json!({
            "fixture": fixture_name,
            "steps": [
                step("mko doctor --format json-v1", doctor),
                step(
                    &format!("mko add \"<PROVIDER>/{fixture_name}\" --format json-v1"),
                    rejected,
                ),
                {
                    "boundary": "user_confirmation",
                    "prompt": "Confirm a verified second copy exists",
                    "result": "verified_second_copy_confirmed"
                },
                step(
                    &format!(
                        "mko add \"<PROVIDER>/{fixture_name}\" --verified-backup --format json-v1"
                    ),
                    accepted,
                ),
            ]
        });
        normalize_transcript(&mut transcript);
        self.assert_no_harness_paths(&transcript);
        assert_no_machine_paths(&transcript);
        let commands = transcript["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|step| step["command"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.contains("--verified-backup"))
                .count(),
            1
        );
        let confirmation_index = transcript["steps"]
            .as_array()
            .unwrap()
            .iter()
            .position(|step| step["boundary"] == "user_confirmation")
            .unwrap();
        let retry_index = transcript["steps"]
            .as_array()
            .unwrap()
            .iter()
            .position(|step| {
                step["command"]
                    .as_str()
                    .is_some_and(|command| command.contains("--verified-backup"))
            })
            .unwrap();
        assert!(confirmation_index < retry_index);
        assert!(
            transcript["steps"].as_array().unwrap()[..confirmation_index]
                .iter()
                .filter_map(|step| step["command"].as_str())
                .all(|command| !command.contains("--verified-backup"))
        );
        transcript
    }

    fn run_batch(&self) -> Value {
        fs::write(
            self.provider.join("a-benign.pdf"),
            include_bytes!("../../../tests/fixtures/skill-forward/benign-paper.pdf"),
        )
        .unwrap();
        fs::write(
            self.provider.join("b-hostile-name-$(git push).pdf"),
            include_bytes!("../../../tests/fixtures/skill-forward/hostile-instructions-paper.pdf"),
        )
        .unwrap();
        fs::write(self.provider.join("c-invalid.pdf"), b"not a PDF").unwrap();

        let mut steps = Vec::new();
        let doctor = self.run_json(["doctor", "--format", "json-v1"]);
        steps.push(step("mko doctor --format json-v1", doctor));

        let rejected = self.run_json(["add", "--inbox", "--format", "json-v1"]);
        assert_eq!(
            rejected["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["error"]["code"] == "backup_confirmation_required")
                .count(),
            2
        );
        steps.push(step("mko add --inbox --format json-v1", rejected));
        steps.push(json!({
            "boundary": "user_confirmation",
            "prompt": "Confirm a verified second copy exists for Inbox registration",
            "result": "verified_second_copy_confirmed"
        }));

        let accepted =
            self.run_json(["add", "--inbox", "--verified-backup", "--format", "json-v1"]);
        let mut seen_assets = HashSet::new();
        let actionable = accepted["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["next_action"] == "prepare")
            .map(|item| item["asset_id"].as_str().unwrap().to_owned())
            .filter(|asset_id| seen_assets.insert(asset_id.clone()))
            .collect::<Vec<_>>();
        assert_eq!(actionable.len(), 2);
        steps.push(step(
            "mko add --inbox --verified-backup --format json-v1",
            accepted,
        ));

        for (index, asset_id) in actionable.iter().enumerate() {
            let bundle = format!(".knowledge-os/runtime/prepared/{asset_id}.json");
            let prepared = self.run_json([
                "source",
                "prepare",
                "--asset-id",
                asset_id,
                "--output",
                &bundle,
                "--format",
                "json-v1",
            ]);
            steps.push(step(
                &format!(
                    "mko source prepare --asset-id \"{asset_id}\" --output \"{bundle}\" --format json-v1"
                ),
                prepared,
            ));
            let response_path = self.repository.join(format!(
                ".knowledge-os/runtime/semantic-response-{index}.json"
            ));
            fs::write(
                &response_path,
                serde_json::to_vec(&semantic_response(
                    &format!("Batch paper {}", index + 1),
                    "A bounded batch fixture.",
                ))
                .unwrap(),
            )
            .unwrap();
            let drafted = self.run_json([
                "source",
                "write-draft",
                "--bundle",
                &bundle,
                "--response",
                response_path.to_str().unwrap(),
                "--format",
                "json-v1",
            ]);
            steps.push(step(
                &format!(
                    "mko source write-draft --bundle \"{bundle}\" --response \"<RUNTIME>/semantic-response-{index}.json\" --format json-v1"
                ),
                drafted,
            ));
        }

        let checked = self.run_json(["check", "--format", "json-v1"]);
        steps.push(step("mko check --format json-v1", checked));
        let mut transcript = json!({"fixture": "mixed-inbox", "steps": steps});
        normalize_transcript(&mut transcript);
        self.assert_no_harness_paths(&transcript);
        assert_no_machine_paths(&transcript);
        transcript
    }

    fn assert_no_harness_paths(&self, transcript: &Value) {
        let profile = self.config_home.join("mko/profiles.yaml");
        let roots = [
            self._root.path(),
            &self.repository,
            &self.provider,
            &self.home,
            &self.config_home,
            &profile,
        ]
        .into_iter()
        .map(|path| path.to_str().expect("fixture paths are UTF-8").to_owned())
        .collect::<Vec<_>>();
        assert_no_known_path_leaks(transcript, &roots);
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

    fn run_json_failure<const N: usize>(&self, arguments: [&str; N]) -> Value {
        let output = self
            .command()
            .args(arguments)
            .assert()
            .code(1)
            .stderr("")
            .get_output()
            .stdout
            .clone();
        let typed: JsonV1Failure = serde_json::from_slice(&output).unwrap();
        serde_json::to_value(typed).unwrap()
    }
}

fn step(command: &str, result: Value) -> Value {
    json!({"command": command, "result": result})
}

fn transcript_commands(transcript: &Value) -> Vec<&str> {
    transcript["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step["command"].as_str())
        .collect()
}

fn assert_pending_knowledge_forward_behavior(transcript: &Value) {
    let commands = transcript_commands(transcript);
    let writes = commands
        .iter()
        .filter(|command| command.starts_with("mko knowledge write "))
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1, "Knowledge must be written exactly once");
    assert!(
        writes[0].contains("--bundle \".knowledge-os/runtime/prepared/personal-asset-")
            && writes[0].contains("--response \".knowledge-os/runtime/knowledge-response.json\"")
            && writes[0].ends_with("--format json-v1"),
        "Knowledge write must use the canonical bundle and response paths: {}",
        writes[0]
    );
    assert!(commands.contains(&"mko check --format json-v1"));
    assert!(
        commands
            .iter()
            .all(|command| !command.starts_with("mko knowledge review"))
    );
    let final_boundary = transcript["steps"].as_array().unwrap().last().unwrap();
    assert_eq!(final_boundary["boundary"], "pending_human_review");
    assert_eq!(final_boundary["status"], "unreviewed");
    assert_eq!(final_boundary["next_action"], "mko knowledge review");
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

fn platform_transcript(
    profile: &str,
    repository: &str,
    provider: &str,
    bundle: &str,
    registry: &str,
) -> Value {
    json!({
        "steps": [
            {"result": {"command": "doctor", "data": {"checks": [
                {"code": "profile_valid", "path": profile},
                {"code": "repository_access", "path": repository},
                {"code": "provider_inbox", "path": provider}
            ]}}},
            {"result": {"command": "add", "data": {
                "repository": repository,
                "registry_path": registry
            }}},
            {"result": {"command": "source.prepare", "data": {
                "bundle_path": bundle
            }}}
        ]
    })
}

fn normalize_transcript(transcript: &mut Value) {
    let Some(steps) = transcript["steps"].as_array_mut() else {
        return;
    };
    for step in steps {
        let Some(command) = step["result"]["command"].as_str() else {
            continue;
        };
        match command {
            "doctor" => {
                if let Some(checks) = step["result"]["data"]["checks"].as_array_mut() {
                    for check in checks {
                        let placeholder = match check["code"].as_str().unwrap_or_default() {
                            "profile_valid" => Some("<PROFILE>"),
                            code if code.starts_with("provider_") => Some("<PROVIDER>"),
                            "repository_access" | "hook_managed" | "locks_clear" => {
                                Some("<REPOSITORY>")
                            }
                            _ => None,
                        };
                        if check["path"].is_string() {
                            check["path"] = placeholder.map_or(Value::Null, |value| value.into());
                        }
                    }
                }
            }
            "add" => {
                if let Some(data) = step["result"]["data"].as_object_mut() {
                    if data.contains_key("repository") {
                        data.insert("repository".into(), "<REPOSITORY>".into());
                    }
                    if let Some(registry_path) = data.get_mut("registry_path") {
                        normalize_path_field(registry_path);
                    }
                }
            }
            "source.prepare" => {
                let path = step["result"]["data"]["bundle_path"]
                    .as_str()
                    .unwrap_or_default()
                    .replace('\\', "/");
                let suffix = path
                    .find("/.knowledge-os/")
                    .map(|index| &path[index..])
                    .unwrap_or("/.knowledge-os/runtime/prepared/asset.json");
                step["result"]["data"]["bundle_path"] =
                    Value::String(format!("<REPOSITORY>{suffix}"));
            }
            "source.write_draft" => {
                step["result"]["data"]["source_path"] = "<SOURCE_PATH>".into();
                step["result"]["data"]["content_revision"] = "<CONTENT_REVISION>".into();
            }
            _ => {}
        }
    }
}

fn normalize_path_field(value: &mut Value) {
    if let Some(path) = value.as_str() {
        *value = Value::String(path.replace('\\', "/"));
    }
}

fn assert_no_machine_paths(transcript: &Value) {
    for value in json_string_values(transcript) {
        let normalized = normalize_path_for_comparison(value);
        for forbidden in ["/private/tmp", "/var/folders/", "c:/", "appdata"] {
            assert!(
                !normalized.contains(forbidden),
                "machine path leaked: {value}"
            );
        }
    }
}

fn assert_no_known_path_leaks(transcript: &Value, roots: &[String]) {
    for value in json_string_values(transcript) {
        let normalized = normalize_path_for_comparison(value);
        for root in roots {
            let root = normalize_path_for_comparison(root);
            assert!(
                !normalized.contains(&root),
                "known harness path leaked into a normalized transcript: {value}"
            );
        }
    }
}

fn json_string_values(value: &Value) -> Vec<&str> {
    fn visit<'a>(value: &'a Value, strings: &mut Vec<&'a str>) {
        match value {
            Value::String(value) => strings.push(value),
            Value::Array(values) => values.iter().for_each(|value| visit(value, strings)),
            Value::Object(values) => values.values().for_each(|value| visit(value, strings)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    let mut strings = Vec::new();
    visit(value, &mut strings);
    strings
}

fn normalize_path_for_comparison(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
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

fn benign_knowledge_response() -> Value {
    json!({
        "synthesis": "Deterministic fixtures support repeatable validation.",
        "concepts": [{
            "name": "Deterministic fixture",
            "kind": "concept",
            "body": "A fixed input supports repeatable validation.",
            "tags": ["testing"],
            "locator": "page 1"
        }]
    })
}

fn hostile_knowledge_response() -> Value {
    json!({
        "synthesis": "The document states no supported technical evidence.",
        "concepts": []
    })
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
