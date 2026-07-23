use assert_cmd::Command;

#[allow(deprecated)]
#[test]
fn review_requires_a_human_tty_and_has_no_json_mode() {
    Command::cargo_bin("mko")
        .unwrap()
        .args(["review", "--repo", "."])
        .assert()
        .failure()
        .stderr(predicates::str::contains("human_confirmation_required"));

    Command::cargo_bin("mko")
        .unwrap()
        .args(["review", "--repo", ".", "--format", "json-v1"])
        .assert()
        .failure();
}
