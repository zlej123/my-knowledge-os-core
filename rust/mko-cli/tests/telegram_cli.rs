use std::{fs, path::PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        Self { _root: root, home }
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .env("HOME", &self.home)
            .env("APPDATA", self.home.join("AppData/Roaming"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .current_dir(self._root.path());
        command
    }

    fn assert_no_telegram_state(&self) {
        for path in [
            self.home.join(".config/mko/telegram"),
            self.home.join("Library/Application Support/mko/telegram"),
            self.home.join("AppData/Roaming/mko/telegram"),
        ] {
            assert!(
                !path.exists(),
                "a non-interactive command must not create Telegram state at {}",
                path.display()
            );
        }
    }
}

#[test]
#[allow(deprecated)]
fn telegram_help_exposes_only_the_safe_onboarding_commands() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["telegram", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("connect"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("disconnect"))
        .stdout(predicate::str::contains("--token").not());
}

#[test]
#[allow(deprecated)]
fn telegram_connect_rejects_token_arguments_without_echoing_the_value() {
    let fixture = Fixture::new();
    let secret = "telegram-test-token-must-never-appear";
    fixture
        .command()
        .args([
            "telegram",
            "connect",
            "--profile",
            "personal",
            "--token",
            secret,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--token"))
        .stdout(predicate::str::contains(secret).not())
        .stderr(predicate::str::contains(secret).not());
    fixture.assert_no_telegram_state();
}

#[test]
#[allow(deprecated)]
fn telegram_connect_fails_closed_without_a_real_tty_and_does_not_mutate() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["telegram", "connect", "--profile", "personal"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("TTY"));
    fixture.assert_no_telegram_state();
}

#[test]
#[allow(deprecated)]
fn telegram_disconnect_fails_closed_without_a_real_tty_and_does_not_mutate() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["telegram", "disconnect", "--profile", "personal"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("TTY"));
    fixture.assert_no_telegram_state();
}

#[test]
#[allow(deprecated)]
fn telegram_status_human_surface_is_non_mutating() {
    let fixture = Fixture::new();
    let _ = fixture
        .command()
        .args([
            "telegram",
            "status",
            "--profile",
            "personal",
            "--format",
            "human",
        ])
        .output()
        .unwrap();
    fixture.assert_no_telegram_state();
}
