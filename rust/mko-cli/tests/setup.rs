use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(target_os = "macos")]
use std::{path::PathBuf, process::Command as ProcessCommand};

use assert_cmd::Command;

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(deprecated)]
fn setup_rejects_non_interactive_execution_before_mutating_machine_state() {
    let root = std::env::temp_dir().join(format!(
        "mko-cli-setup-{}-{}",
        std::process::id(),
        NEXT_ENV.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("setup")
        .env("HOME", root.join("home"))
        .current_dir(&root)
        .assert()
        .code(1)
        .get_output()
        .clone();

    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "tty_required: setup requires an interactive terminal\n"
    );
    assert!(
        !root
            .join("home/Library/Application Support/mko/profiles.yaml")
            .exists()
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn setup_pty_uses_the_single_known_drive_root() {
    let env = SetupEnv::new();
    let drive = env.drive("alice");

    let output = env.run_pty(None);

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("setup complete"));
    assert!(drive.join("My-Knowledge-OS-Assets/personal/inbox").is_dir());
    assert!(env.profile().is_file());
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn setup_pty_selects_one_of_multiple_known_drive_roots() {
    let env = SetupEnv::new();
    let alice = env.drive("alice");
    let bob = env.drive("bob");

    let output = env.run_pty(Some("2"));

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(bob.join("My-Knowledge-OS-Assets/personal/inbox").is_dir());
    assert!(!alice.join("My-Knowledge-OS-Assets/personal/inbox").exists());
    assert!(
        fs::read_to_string(env.profile())
            .unwrap()
            .contains(&bob.display().to_string())
    );
}

#[cfg(target_os = "macos")]
#[test]
#[allow(deprecated)]
fn setup_pty_rejects_an_invalid_selection_without_mutation() {
    let env = SetupEnv::new();
    let alice = env.drive("alice");
    let bob = env.drive("bob");

    let output = env.run_pty(Some("3"));

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("drive_root_ambiguous")
            || String::from_utf8_lossy(&output.stderr).contains("drive_root_ambiguous"),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for root in [alice, bob] {
        assert!(!root.join("My-Knowledge-OS-Assets/personal/inbox").exists());
    }
    assert!(!env.profile().exists());
}

#[cfg(target_os = "macos")]
struct SetupEnv {
    root: PathBuf,
    repository: PathBuf,
    home: PathBuf,
}

#[cfg(target_os = "macos")]
impl SetupEnv {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mko-cli-setup-pty-{}-{}",
            std::process::id(),
            NEXT_ENV.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = root.join("repository");
        let home = root.join("home");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), knowledge_config()).unwrap();
        let initialized = ProcessCommand::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap();
        assert!(initialized.success());
        Self {
            root,
            repository,
            home,
        }
    }

    fn drive(&self, name: &str) -> PathBuf {
        let drive = self
            .home
            .join("Library/CloudStorage")
            .join(format!("GoogleDrive-{name}"))
            .join("My Drive");
        fs::create_dir_all(&drive).unwrap();
        drive
    }

    fn profile(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/mko/profiles.yaml")
    }

    #[allow(deprecated)]
    fn run_pty(&self, selection: Option<&str>) -> std::process::Output {
        let script = if selection.is_some() {
            "set timeout 10\nset bin $env(MKO_TEST_BIN)\nset repo $env(MKO_TEST_REPO)\nset selection $env(MKO_TEST_SELECTION)\nspawn -noecho $bin setup --repo $repo\nexpect {\n  \"Select Google Drive account:\" { send -- \"$selection\\r\"; exp_continue }\n  eof {}\n}\nset status [wait]\nexit [lindex $status 3]\n"
        } else {
            "set timeout 10\nset bin $env(MKO_TEST_BIN)\nset repo $env(MKO_TEST_REPO)\nspawn -noecho $bin setup --repo $repo\nexpect eof\nset status [wait]\nexit [lindex $status 3]\n"
        };
        let mut command = ProcessCommand::new("/usr/bin/expect");
        command
            .args(["-c", script])
            .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
            .env("MKO_TEST_REPO", &self.repository)
            .env("HOME", &self.home)
            .current_dir(&self.root);
        if let Some(selection) = selection {
            command.env("MKO_TEST_SELECTION", selection);
        }
        command.output().unwrap()
    }
}

#[cfg(target_os = "macos")]
impl Drop for SetupEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(target_os = "macos")]
fn knowledge_config() -> &'static str {
    "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n"
}
