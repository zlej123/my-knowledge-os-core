use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(target_os = "macos")]
use std::{
    io::Write,
    process::{Command as ProcessCommand, Stdio},
};

use assert_cmd::Command;
use mko_core::{
    knowledge::approve_knowledge,
    pdf::{EXTRACTOR_NAME, EXTRACTOR_VERSION},
    prepare::{PROCESSOR_VERSION, PROMPT_VERSION, PreparedSourceBundle, TRUST, VersionedComponent},
    registry::read_asset,
    version::KNOWLEDGE_CONTRACT_VERSION,
};
use serde_json::Value;

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

const VALID_RESPONSE: &str = r#"{
  "synthesis": "A signals-and-systems text covering LTI systems and transforms.",
  "concepts": [
    {"name": "Convolution", "kind": "formula", "body": "x*h(t)=integral of x(tau)h(t-tau)dtau", "tags": ["LTI"], "locator": "4.2"},
    {"name": "Causal signal", "kind": "definition", "body": "x(t)=0 for t<0", "tags": [], "locator": null}
  ]
}"#;

#[test]
#[allow(deprecated)]
fn knowledge_write_creates_an_unreviewed_note_and_emits_json_v1() {
    let env = Env::new();
    let asset_id = env.capture_asset("paper.pdf", b"%PDF-1.7\nfixture");
    let response = env.write_response("response.json", VALID_RESPONSE);

    let output = env
        .command([
            "knowledge",
            "write",
            "--repo",
            &env.repository.display().to_string(),
            "--asset-id",
            &asset_id,
            "--bundle",
            &env.bundle_path(&asset_id).display().to_string(),
            "--response",
            &response.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();

    let value = one_json(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "knowledge.write");
    assert_eq!(value["result"], "ok");
    assert_eq!(value["data"]["write_outcome"], "created");
    assert_eq!(value["data"]["asset_id"], asset_id);
    assert!(
        value["data"]["knowledge_path"]
            .as_str()
            .unwrap()
            .starts_with("knowledge/")
    );
    assert!(
        value["data"]["content_revision"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[test]
#[allow(deprecated)]
fn knowledge_write_is_idempotent_then_requires_replace_to_regenerate() {
    let env = Env::new();
    let asset_id = env.capture_asset("paper.pdf", b"%PDF-1.7\nfixture");
    let response = env.write_response("response.json", VALID_RESPONSE);

    let first = one_json(
        &env.command(env.write_arguments(&asset_id, &response, false))
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(first["data"]["write_outcome"], "created");

    let second = one_json(
        &env.command(env.write_arguments(&asset_id, &response, false))
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(second["data"]["write_outcome"], "existing");

    let changed = env.write_response(
        "response-2.json",
        &VALID_RESPONSE.replace(
            "LTI systems and transforms",
            "LTI systems, transforms, and sampling",
        ),
    );

    let rejected = one_json(
        &env.command(env.write_arguments(&asset_id, &changed, false))
            .assert()
            .code(1)
            .get_output()
            .stdout,
    );
    assert_eq!(rejected["result"], "error");
    assert_eq!(rejected["error"]["code"], "replace_required");

    let replaced = one_json(
        &env.command(env.write_arguments(&asset_id, &changed, true))
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(replaced["data"]["write_outcome"], "replaced");
}

#[test]
#[allow(deprecated)]
fn knowledge_write_rejects_an_unknown_asset_with_a_path_free_golden() {
    let env = Env::new();
    let response = env.write_response("response.json", VALID_RESPONSE);

    let output = env
        .command([
            "knowledge",
            "write",
            "--repo",
            &env.repository.display().to_string(),
            "--asset-id",
            "personal-asset-deadbeef",
            "--bundle",
            &env.bundle_path("personal-asset-deadbeef")
                .display()
                .to_string(),
            "--response",
            &response.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .code(1)
        .stderr("")
        .get_output()
        .stdout
        .clone();

    let expected = include_bytes!("../../../tests/fixtures/json-v1/knowledge-asset-missing.json")
        .iter()
        .copied()
        .filter(|byte| *byte != b'\r')
        .collect::<Vec<_>>();
    assert_eq!(output, expected);
    assert!(!String::from_utf8_lossy(&output).contains(&env.repository.display().to_string()));
}

#[test]
#[allow(deprecated)]
fn knowledge_write_requires_the_prepared_bundle_argument() {
    let env = Env::new();
    let asset_id = env.capture_asset("paper.pdf", b"%PDF-1.7\nfixture");
    let response = env.write_response("response.json", VALID_RESPONSE);

    let output = env
        .command([
            "knowledge",
            "write",
            "--repo",
            &env.repository.display().to_string(),
            "--asset-id",
            &asset_id,
            "--response",
            &response.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let value = one_json(&output);
    assert_eq!(value["command"], "knowledge.write");
    assert_eq!(value["result"], "error");
    assert_eq!(value["error"]["code"], "usage");
}

#[test]
#[allow(deprecated)]
fn knowledge_search_finds_cross_document_matches_and_supports_filters() {
    let env = Env::new();
    let asset_id = env.capture_asset("paper.pdf", b"%PDF-1.7\nfixture");
    let response = env.write_response("response.json", VALID_RESPONSE);
    env.command(env.write_arguments(&asset_id, &response, false))
        .assert()
        .success();

    let second_asset_id = env.capture_asset("second.pdf", b"%PDF-1.7\nsecond fixture");
    let second_response = env.write_response(
        "second-response.json",
        r#"{"synthesis":"A different fixture text.","concepts":[{"name":"Entropy","kind":"concept","body":"A measure of disorder.","tags":["thermo"],"locator":null}]}"#,
    );
    env.command(env.write_arguments(&second_asset_id, &second_response, false))
        .assert()
        .success();

    let output = env
        .command([
            "knowledge",
            "search",
            "convolution",
            "--repo",
            &env.repository.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let value = one_json(&output);
    assert_eq!(value["command"], "knowledge.search");
    let matches = value["data"]["matches"].as_array().unwrap();
    assert!(matches.iter().any(|m| m["name"] == "Convolution"));
    assert!(matches.iter().all(|m| m["asset_id"] == asset_id));

    let filtered = one_json(
        &env.command([
            "knowledge",
            "search",
            "",
            "--repo",
            &env.repository.display().to_string(),
            "--kind",
            "concept",
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout,
    );
    let filtered_matches = filtered["data"]["matches"].as_array().unwrap();
    assert!(!filtered_matches.is_empty());
    assert!(filtered_matches.iter().all(|m| m["kind"] == "concept"));
    assert!(filtered_matches.iter().any(|m| m["name"] == "Entropy"));
}

#[test]
#[allow(deprecated)]
fn knowledge_show_and_list_report_the_written_note() {
    let env = Env::new();
    let asset_id = env.capture_asset("paper.pdf", b"%PDF-1.7\nfixture");
    let response = env.write_response("response.json", VALID_RESPONSE);
    env.command(env.write_arguments(&asset_id, &response, false))
        .assert()
        .success();

    let shown = one_json(
        &env.command([
            "knowledge",
            "show",
            "--repo",
            &env.repository.display().to_string(),
            "--asset-id",
            &asset_id,
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout,
    );
    assert_eq!(shown["command"], "knowledge.show");
    assert_eq!(shown["data"]["asset_id"], asset_id);
    assert_eq!(shown["data"]["review_status"], "unreviewed");
    assert!(
        shown["data"]["concepts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Convolution")
    );

    let listed = one_json(
        &env.command([
            "knowledge",
            "list",
            "--repo",
            &env.repository.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout,
    );
    assert_eq!(listed["command"], "knowledge.list");
    let items = listed["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["asset_id"], asset_id);
}

#[test]
#[allow(deprecated)]
fn knowledge_show_and_list_keep_a_reviewed_note_with_no_concepts() {
    let env = Env::new();
    let asset_id = env.capture_asset("empty.pdf", b"%PDF-1.7\nempty concepts fixture");
    let response = env.write_response(
        "empty-response.json",
        r#"{"synthesis":"Useful synthesis without separate concepts.","concepts":[]}"#,
    );
    let written = one_json(
        &env.command(env.write_arguments(&asset_id, &response, false))
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    approve_knowledge(
        &env.repository,
        written["data"]["knowledge_id"].as_str().unwrap(),
        written["data"]["content_revision"].as_str().unwrap(),
    )
    .unwrap();

    let shown = one_json(
        &env.command([
            "knowledge",
            "show",
            "--repo",
            &env.repository.display().to_string(),
            "--asset-id",
            &asset_id,
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout,
    );
    assert_eq!(shown["data"]["review_status"], "reviewed");
    assert_eq!(
        shown["data"]["knowledge_id"],
        written["data"]["knowledge_id"]
    );
    assert_eq!(shown["data"]["concepts"], serde_json::json!([]));

    let listed = one_json(
        &env.command([
            "knowledge",
            "list",
            "--repo",
            &env.repository.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout,
    );
    let items = listed["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["asset_id"], asset_id);
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn interactive_knowledge_review_json_stdout_is_one_object() {
    let env = Env::new();
    let asset_id = env.capture_asset("review.pdf", b"%PDF-1.7\nreview fixture");
    let response = env.write_response("review-response.json", VALID_RESPONSE);
    env.command(env.write_arguments(&asset_id, &response, false))
        .assert()
        .success();
    let stderr_path = env.root.join("review-stderr.log");
    let shell_command = "stty -echo; exec \"$MKO_TEST_BIN\" knowledge review --repo \"$MKO_TEST_REPO\" --asset-id \"$MKO_TEST_ASSET\" --format json-v1 2>\"$MKO_TEST_STDERR\"";
    let mut child = ProcessCommand::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sh", "-c", shell_command])
        .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
        .env("MKO_TEST_REPO", &env.repository)
        .env("MKO_TEST_ASSET", &asset_id)
        .env("MKO_TEST_STDERR", &stderr_path)
        .env("MKO_PERSONAL_PROVIDER_ROOT", &env.provider)
        .current_dir(&env.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    stdin.write_all(b"approve\n").unwrap();
    stdin.flush().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "script stderr={} child stderr={} transcript={}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(&stderr_path).unwrap_or_default(),
        String::from_utf8_lossy(&output.stdout),
    );

    let value = one_json(&output.stdout);
    assert_eq!(value["command"], "knowledge.review");
    assert_eq!(value["data"]["items"][0]["decision"], "approved");
    let prompts = fs::read_to_string(stderr_path).unwrap();
    assert!(prompts.contains("approve/defer"));
    assert!(prompts.contains("## Synthesis"));
}

#[test]
#[allow(deprecated)]
fn knowledge_review_requires_a_human_tty_and_has_no_json_bypass() {
    let env = Env::new();

    env.command([
        "knowledge",
        "review",
        "--repo",
        &env.repository.display().to_string(),
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains("human_confirmation_required"));

    let output = env
        .command([
            "knowledge",
            "review",
            "--repo",
            &env.repository.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let value = one_json(&output);
    assert_eq!(value["command"], "knowledge.review");
    assert_eq!(value["result"], "error");
    assert_eq!(value["error"]["code"], "human_confirmation_required");
}

struct Env {
    root: PathBuf,
    repository: PathBuf,
    provider: PathBuf,
    local_config: PathBuf,
}

impl Env {
    fn new() -> Self {
        let unique = NEXT_ENV.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mko-knowledge-cli-{}-{unique}", std::process::id()));
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
        }
    }

    #[allow(deprecated)]
    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .args(args)
            .env("MKO_PERSONAL_PROVIDER_ROOT", &self.provider);
        command
    }

    #[allow(deprecated)]
    fn capture_asset(&self, name: &str, bytes: &[u8]) -> String {
        let pdf = self.provider.join(name);
        fs::write(&pdf, bytes).unwrap();
        let output = self
            .command([
                "asset".into(),
                "capture".into(),
                "--repo".into(),
                self.repository.display().to_string(),
                "--local-config".into(),
                self.local_config.display().to_string(),
                "--file".into(),
                pdf.display().to_string(),
                "--json".into(),
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value = parse_json(&output);
        let asset_id = value["asset_id"].as_str().unwrap().to_owned();
        self.publish_bundle(&asset_id);
        asset_id
    }

    fn bundle_path(&self, asset_id: &str) -> PathBuf {
        self.repository
            .join(".knowledge-os/runtime/prepared")
            .join(format!("{asset_id}.json"))
    }

    fn publish_bundle(&self, asset_id: &str) {
        let asset = read_asset(&self.repository, asset_id).unwrap();
        let bundle = PreparedSourceBundle {
            schema_version: 1,
            asset_id: asset.id.clone(),
            source_id: asset.id.replacen("asset", "source", 1),
            fingerprint: asset.fingerprint,
            title_hint: asset.title,
            logical_path: asset.provider.locator,
            pages: vec!["Fixture page".into()],
            trust: TRUST.into(),
            extractor: VersionedComponent {
                name: EXTRACTOR_NAME.into(),
                version: EXTRACTOR_VERSION.into(),
            },
            core_version: KNOWLEDGE_CONTRACT_VERSION.into(),
            processor_version: PROCESSOR_VERSION.into(),
            prompt_version: PROMPT_VERSION.into(),
        };
        let path = self.bundle_path(asset_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    }

    fn write_response(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn write_arguments(
        &self,
        asset_id: &str,
        response: &std::path::Path,
        replace: bool,
    ) -> Vec<String> {
        let mut arguments = vec![
            "knowledge".into(),
            "write".into(),
            "--repo".into(),
            self.repository.display().to_string(),
            "--asset-id".into(),
            asset_id.into(),
            "--bundle".into(),
            self.bundle_path(asset_id).display().to_string(),
            "--response".into(),
            response.display().to_string(),
            "--format".into(),
            "json-v1".into(),
        ];
        if replace {
            arguments.push("--replace".into());
        }
        arguments
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn one_json(stdout: &[u8]) -> Value {
    assert_eq!(
        stdout.iter().filter(|&&byte| byte == b'\n').count(),
        1,
        "stdout must contain one JSON object: {}",
        String::from_utf8_lossy(stdout)
    );
    serde_json::from_slice(stdout).unwrap()
}

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).unwrap()
}
