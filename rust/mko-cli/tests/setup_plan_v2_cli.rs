use std::{fs, path::PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    repository: PathBuf,
    drive: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repository_parent = root.path().join("Knowledge/Personal Engineering Vault");
        let repository = repository_parent.join("personal-kb");
        let drive = root.path().join("Google Drive/My Drive");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repository_parent).unwrap();
        fs::create_dir_all(&drive).unwrap();
        Self {
            _root: root,
            home,
            repository,
            drive,
        }
    }

    #[allow(deprecated)]
    fn plan(&self) -> serde_json::Value {
        let output = Command::cargo_bin("mko")
            .unwrap()
            .args(["setup", "plan", "--repo"])
            .arg(&self.repository)
            .arg("--drive-root")
            .arg(&self.drive)
            .args(["--format", "json-v2"])
            .env("HOME", &self.home)
            .current_dir(self._root.path())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }

    fn assert_targets_unchanged(&self) {
        assert!(!self.repository.exists());
        assert!(!self.drive.join("My-Knowledge-OS-Assets").exists());
        assert!(!self.home.join(".config/mko/profiles.yaml").exists());
    }
}

#[test]
#[allow(deprecated)]
fn machine_setup_apply_displays_exact_effects_but_non_tty_cannot_mutate() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    assert_eq!(plan["command"], "setup.plan");
    assert_eq!(plan["data"]["single_use"], true);
    assert_eq!(plan["data"]["approval_mode"], "tty");
    assert_eq!(plan["data"]["next_action"], "approve_plan");
    fixture.assert_targets_unchanged();

    let plan_id = plan["data"]["plan_id"].as_str().unwrap();
    let assertion = Command::cargo_bin("mko")
        .unwrap()
        .args(["setup", "apply", "--plan", plan_id, "--format", "json-v2"])
        .env("HOME", &fixture.home)
        .current_dir(fixture._root.path())
        .assert()
        .code(1);
    let output = assertion.get_output();
    let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failure["command"], "setup.apply");
    assert_eq!(failure["error"]["code"], "setup_tty_required");
    let display = String::from_utf8_lossy(&output.stderr);
    assert!(display.contains("# My Knowledge OS setup approval"));
    assert!(display.contains(fixture.repository.to_str().unwrap()));
    assert!(display.contains(fixture.drive.to_str().unwrap()));
    assert!(display.contains("My-Knowledge-OS-Assets/personal/inbox"));
    assert!(display.contains("profiles.yaml"));
    assert!(display.contains("Approval effect digest: sha256:"));
    fixture.assert_targets_unchanged();
}

#[test]
#[allow(deprecated)]
fn stale_setup_plan_is_rejected_without_target_mutation() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    fs::create_dir(&fixture.repository).unwrap();
    fs::write(fixture.repository.join("unplanned.txt"), b"changed").unwrap();
    let plan_id = plan["data"]["plan_id"].as_str().unwrap();
    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["setup", "apply", "--plan", plan_id, "--format", "json-v2"])
        .env("HOME", &fixture.home)
        .current_dir(fixture._root.path())
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let failure: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(failure["error"]["code"], "setup_plan_stale");
    assert_eq!(
        fs::read(fixture.repository.join("unplanned.txt")).unwrap(),
        b"changed"
    );
    assert!(!fixture.repository.join("knowledge-os.yaml").exists());
    assert!(!fixture.drive.join("My-Knowledge-OS-Assets").exists());
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn real_tty_setup_apply_requires_the_exact_phrase_and_is_single_use() {
    use std::{
        io::{Read, Write},
        process::{Command as ProcessCommand, Stdio},
    };

    let fixture = Fixture::new();
    let plan = fixture.plan();
    let plan_id = plan["data"]["plan_id"].as_str().unwrap();

    let shell =
        "stty -echo; exec \"$MKO_TEST_BIN\" setup apply --plan \"$MKO_TEST_PLAN\" --format json-v2";
    let mut wrong = ProcessCommand::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sh", "-c", shell])
        .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
        .env("MKO_TEST_PLAN", plan_id)
        .env("HOME", &fixture.home)
        .current_dir(fixture._root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut wrong_stdout = wrong.stdout.take().unwrap();
    let mut wrong_transcript = Vec::new();
    read_through_prompt(&mut wrong_stdout, &mut wrong_transcript);
    let mut wrong_stdin = wrong.stdin.take().unwrap();
    wrong_stdin
        .write_all(b"approve-setup wrong wrong\n")
        .unwrap();
    wrong_stdin.flush().unwrap();
    let wrong_status = wrong.wait().unwrap();
    wrong_stdout.read_to_end(&mut wrong_transcript).unwrap();
    assert!(!wrong_status.success());
    assert!(String::from_utf8_lossy(&wrong_transcript).contains("setup_confirmation_mismatch"));
    fixture.assert_targets_unchanged();

    let mut approved = ProcessCommand::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sh", "-c", shell])
        .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
        .env("MKO_TEST_PLAN", plan_id)
        .env("HOME", &fixture.home)
        .current_dir(fixture._root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut approved_stdout = approved.stdout.take().unwrap();
    let mut transcript = Vec::new();
    read_through_prompt(&mut approved_stdout, &mut transcript);
    let prompt = String::from_utf8(transcript.clone()).unwrap();
    let phrase = prompt
        .split("Type exactly:\r\n")
        .nth(1)
        .or_else(|| prompt.split("Type exactly:\n").nth(1))
        .and_then(|tail| tail.lines().next())
        .unwrap()
        .trim_end_matches('\r')
        .to_owned();
    assert!(phrase.starts_with("approve-setup sha256:"));
    let mut approved_stdin = approved.stdin.take().unwrap();
    approved_stdin
        .write_all(format!("{phrase}\n").as_bytes())
        .unwrap();
    approved_stdin.flush().unwrap();
    let approved_status = approved.wait().unwrap();
    approved_stdout.read_to_end(&mut transcript).unwrap();
    assert!(
        approved_status.success(),
        "transcript={}",
        String::from_utf8_lossy(&transcript)
    );
    assert!(fixture.repository.join("knowledge-os.yaml").is_file());
    assert!(
        fixture
            .drive
            .join("My-Knowledge-OS-Assets/personal/inbox")
            .is_dir()
    );
    assert!(
        fixture
            .home
            .join("Library/Application Support/mko/profiles.yaml")
            .is_file()
    );

    let replay = Command::cargo_bin("mko")
        .unwrap()
        .args(["setup", "apply", "--plan", plan_id, "--format", "json-v2"])
        .env("HOME", &fixture.home)
        .current_dir(fixture._root.path())
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let replay: serde_json::Value = serde_json::from_slice(&replay).unwrap();
    assert_eq!(replay["error"]["code"], "setup_plan_consumed");
}

#[cfg(target_os = "macos")]
fn read_through_prompt(reader: &mut impl std::io::Read, output: &mut Vec<u8>) {
    let mut byte = [0_u8; 1];
    while !output.ends_with(b"> ") {
        assert_eq!(reader.read(&mut byte).unwrap(), 1);
        output.push(byte[0]);
    }
}
