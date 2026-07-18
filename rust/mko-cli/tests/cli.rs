use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_exposes_v01_command_groups() {
    Command::cargo_bin("mko")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("asset"))
        .stdout(predicate::str::contains("source"))
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("human"))
        .stdout(predicate::str::contains("hooks"));
}
