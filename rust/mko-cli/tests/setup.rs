use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

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
