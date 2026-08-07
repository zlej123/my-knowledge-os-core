use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use mko_core::{
    approve::{ApprovalTerminal, ApproveSourceRequest, approve_source_with_terminal_and_clock},
    check::{CheckRequest, check_repository},
    clock::Clock,
    front_matter::parse_markdown,
    hooks::install_hooks,
    model::SourceRecord,
};

const ASSET_ID: &str =
    "personal-asset-efbd75ae8676bc6e1309288d66146d3ac02d16ae971fe6e3e677e702ba936de0";
const SOURCE_FILE: &str = "2026-07-18-paper-efbd75ae8676.md";

#[test]
fn product_version_does_not_change_the_knowledge_contract() {
    assert_eq!(mko_core::version::PRODUCT_VERSION, "0.3.15");
    assert_eq!(mko_core::version::KNOWLEDGE_CONTRACT_VERSION, "0.1.0");
    assert!(mko_core::version::supports_contract("0.1.0"));
    assert!(!mko_core::version::supports_contract("0.2.0"));
}

#[test]
fn approved_v01_knowledge_base_remains_valid_and_byte_identical() {
    let fixture = FixtureRepository::copy("v0.1-kb-approved");
    let source_path = fixture.repository.join("sources").join(SOURCE_FILE);
    let source_before = fs::read(&source_path).unwrap();
    let parsed_before =
        parse_markdown::<SourceRecord>(std::str::from_utf8(&source_before).unwrap()).unwrap();

    let report = check_repository(CheckRequest::new(&fixture.repository)).unwrap();

    assert!(
        report.is_ok(),
        "unexpected check issues: {:?}",
        report.issues
    );
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    let parsed_after = parse_markdown::<SourceRecord>(
        std::str::from_utf8(&fs::read(&source_path).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(parsed_after.body, parsed_before.body);
    assert_eq!(
        parsed_after.metadata.content_revision,
        parsed_before.metadata.content_revision
    );
    assert_eq!(
        parsed_after.metadata.generation,
        parsed_before.metadata.generation
    );
    fixture.assert_committed_bytes_unchanged();
}

#[test]
fn pending_v01_knowledge_base_remains_valid_and_approvable() {
    let fixture = FixtureRepository::copy("v0.1-kb-pending");

    let report = check_repository(CheckRequest::new(&fixture.repository)).unwrap();

    assert!(
        report.is_ok(),
        "unexpected check issues: {:?}",
        report.issues
    );
    fixture.assert_committed_bytes_unchanged();
    let mut terminal = FakeTerminal {
        input: format!("APPROVE personal-source-{}\n", &ASSET_ID[15..]),
    };
    let clock = FixedClock(
        DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    );

    approve_source_with_terminal_and_clock(
        ApproveSourceRequest::new(
            &fixture.repository,
            format!("personal-source-{}", &ASSET_ID[15..]),
        ),
        &mut terminal,
        &clock,
    )
    .unwrap();

    assert!(
        check_repository(CheckRequest::new(&fixture.repository))
            .unwrap()
            .is_ok()
    );
}

struct FixtureRepository {
    _root: tempfile::TempDir,
    repository: PathBuf,
    _provider: PathBuf,
}

impl FixtureRepository {
    fn copy(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        copy_tree(&fixtures.join(name), &repository).unwrap();
        copy_tree(&fixtures.join("v0.1-provider"), &provider).unwrap();
        git(&repository, ["init", "--quiet"]);
        git(
            &repository,
            ["config", "user.email", "fixture@example.invalid"],
        );
        git(&repository, ["config", "user.name", "Fixture Test"]);
        git(&repository, ["add", "."]);
        git(&repository, ["commit", "--quiet", "-m", "freeze fixture"]);
        install_hooks(&repository).unwrap();
        assert_eq!(
            git_output(
                &repository,
                ["config", "--local", "--get", "core.hooksPath"]
            ),
            ".githooks"
        );
        Self {
            _root: root,
            repository,
            _provider: provider,
        }
    }

    fn assert_committed_bytes_unchanged(&self) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.repository)
            .args(["diff", "--exit-code", "--", "."])
            .status()
            .unwrap();
        assert!(status.success(), "fixture bytes changed after validation");
    }
}

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct FakeTerminal {
    input: String,
}

impl ApprovalTerminal for FakeTerminal {
    fn stdin_is_terminal(&self) -> bool {
        true
    }

    fn stdout_is_terminal(&self) -> bool {
        true
    }

    fn write_all(&mut self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn read_line(&mut self, output: &mut String) -> io::Result<usize> {
        output.push_str(&self.input);
        Ok(self.input.len())
    }
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn git<const N: usize>(repository: &Path, arguments: [&str; N]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success());
}

fn git_output<const N: usize>(repository: &Path, arguments: [&str; N]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}
