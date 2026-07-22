use std::fs;

#[cfg(target_os = "macos")]
use std::{
    io::Write,
    process::{Command as ProcessCommand, Stdio},
};

use assert_cmd::Command;
use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    model_v2::ReviewTargetTypeV2,
    model_v2::{PreparedContentV2, SourceResponseV2},
    queue_v2::{derive_queue_v2, show_review_card_v2},
    records_v2::{AssetRecordV2, WriteSourceRecordRequestV2, write_source_record_v2},
    review_v2::{ReviewDerivedStateV2, derive_review_state_v2},
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct Environment {
    root: tempfile::TempDir,
    source_id: String,
}

#[test]
#[allow(deprecated)]
fn queue_and_show_render_stable_core_owned_human_views() {
    let environment = environment();
    let core_queue = derive_queue_v2(environment.root.path()).unwrap();
    let item = &core_queue.items[0];

    Command::cargo_bin("mko")
        .unwrap()
        .args(["queue", "--repo"])
        .arg(environment.root.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("검토 대기열 (1개)"))
        .stdout(predicates::str::contains("Example paper"))
        .stdout(predicates::str::contains(&item.item_id));

    let expected = show_review_card_v2(environment.root.path(), &environment.source_id)
        .unwrap()
        .card_bytes;
    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("show")
        .arg(&environment.source_id)
        .arg("--repo")
        .arg(environment.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(output, expected);
}

#[test]
#[allow(deprecated)]
fn json_v2_queue_show_and_feedback_are_typed_and_session_bound() {
    let environment = environment();
    let queue_output = Command::cargo_bin("mko")
        .unwrap()
        .args(["queue", "--repo"])
        .arg(environment.root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let queue: serde_json::Value = serde_json::from_slice(&queue_output).unwrap();
    assert_eq!(queue["schema_version"], 2);
    assert_eq!(queue["command"], "queue");
    assert_eq!(queue["data"]["items"][0]["next_action"], "display");

    let show_output = Command::cargo_bin("mko")
        .unwrap()
        .arg("show")
        .arg(&environment.source_id)
        .arg("--repo")
        .arg(environment.root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let show: serde_json::Value = serde_json::from_slice(&show_output).unwrap();
    assert_eq!(show["command"], "show");
    assert_eq!(
        show["data"]["targets"][0]["record_id"],
        environment.source_id
    );

    let open_output = Command::cargo_bin("mko")
        .unwrap()
        .arg("review-open")
        .arg(&environment.source_id)
        .arg("--repo")
        .arg(environment.root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let opened: serde_json::Value = serde_json::from_slice(&open_output).unwrap();
    assert_eq!(opened["command"], "review.open");
    assert_eq!(opened["data"]["approval_mode"], "tty");
    let decision = serde_json::json!({
        "session_id": opened["data"]["session_id"],
        "card_digest": opened["data"]["card_digest"],
        "target_decisions": [{
            "record_id": environment.source_id,
            "decision": "request_changes",
            "feedback": "Explain the limitation more precisely."
        }]
    });
    let input = environment.root.path().join("decision.json");
    fs::write(&input, serde_json::to_vec(&decision).unwrap()).unwrap();
    let feedback_output = Command::cargo_bin("mko")
        .unwrap()
        .arg("review-feedback")
        .args(["--input"])
        .arg(&input)
        .arg("--repo")
        .arg(environment.root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let feedback: serde_json::Value = serde_json::from_slice(&feedback_output).unwrap();
    assert_eq!(feedback["command"], "review.feedback");
    assert_eq!(feedback["data"]["target_ids"][0], environment.source_id);
}

#[test]
#[allow(deprecated)]
fn json_v2_failure_is_one_typed_envelope() {
    let environment = environment();
    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["show", "not-a-stable-id", "--repo"])
        .arg(environment.root.path())
        .args(["--format", "json-v2"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let failure: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(failure["schema_version"], 2);
    assert_eq!(failure["command"], "show");
    assert_eq!(failure["result"], "error");
    assert_eq!(failure["error"]["code"], "review_card_id_invalid");
}

#[test]
#[allow(deprecated)]
fn non_tty_review_displays_the_card_but_cannot_publish_approval() {
    let environment = environment();
    Command::cargo_bin("mko")
        .unwrap()
        .arg("review")
        .arg(&environment.source_id)
        .arg("--repo")
        .arg(environment.root.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("# Review card"))
        .stderr(predicates::str::contains("review_tty_required"));

    assert_eq!(
        fs::read_dir(environment.root.path().join("reviews"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
#[allow(deprecated)]
fn valid_v1_repository_keeps_the_frozen_legacy_review_route() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("knowledge-os.yaml"),
        "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
    )
    .unwrap();

    Command::cargo_bin("mko")
        .unwrap()
        .args(["review", "--repo"])
        .arg(root.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("human_confirmation_required"));
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn real_tty_review_approves_only_the_exact_displayed_revision() {
    let environment = environment();
    let card = show_review_card_v2(environment.root.path(), &environment.source_id).unwrap();
    let targets = card
        .targets
        .iter()
        .map(|target| target.snapshot.clone())
        .collect::<Vec<_>>();
    let effect_digest = canonical_json_sha256(&serde_json::json!({
        "operation": "approve",
        "targets": targets,
    }))
    .unwrap();
    let confirmation = format!("approve {effect_digest}\n");
    let shell_command =
        "stty -echo; exec \"$MKO_TEST_BIN\" review \"$MKO_TEST_ID\" --repo \"$MKO_TEST_REPO\"";
    let mut child = ProcessCommand::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sh", "-c", shell_command])
        .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
        .env("MKO_TEST_REPO", environment.root.path())
        .env("MKO_TEST_ID", &environment.source_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    stdin.write_all(confirmation.as_bytes()).unwrap();
    stdin.flush().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "script stderr={} transcript={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let transcript = String::from_utf8_lossy(&output.stdout);
    assert!(transcript.contains("# Review card"));
    assert!(transcript.contains(&environment.source_id));
    assert!(transcript.contains(&effect_digest));
    assert!(transcript.contains("approved personal-review-"));
    assert_eq!(
        fs::read_dir(environment.root.path().join("reviews"))
            .unwrap()
            .count(),
        1
    );
    let state = derive_review_state_v2(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &environment.source_id,
    )
    .unwrap();
    assert_eq!(state.state, ReviewDerivedStateV2::Approved);
}

fn environment() -> Environment {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let asset: AssetRecordV2 =
        serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
            .unwrap();
    fs::write(
        root.path()
            .join("assets/registry")
            .join(format!("{}.json", asset.id)),
        canonical_json_bytes(&asset).unwrap(),
    )
    .unwrap();
    let mut bundle: PreparedContentV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/prepared-content.json"
    ))
    .unwrap();
    seal_bundle(&mut bundle);
    let response: SourceResponseV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-response.json"
    ))
    .unwrap();
    let source = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision: None,
        },
        &FixedClock("2026-07-23T00:00:00Z".parse().unwrap()),
    )
    .unwrap();
    Environment {
        root,
        source_id: source.record_id,
    }
}

fn seal_bundle(bundle: &mut PreparedContentV2) {
    let mut value = serde_json::to_value(&*bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
}
