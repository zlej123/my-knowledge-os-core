#[cfg(target_os = "macos")]
mod macos {
    use std::{collections::BTreeMap, fs, path::Path, process::Command};

    use mko_core::scaffold_v2::scaffold_personal_kb_v2;
    use tempfile::tempdir;

    // Opening material and being told what was asked last time is the whole
    // reason the log exists: a new session continues a study instead of
    // restarting it. Verified on a machine configured by profile with no
    // provider-root variable — the machine the owner actually has.
    #[test]
    #[allow(deprecated)]
    fn a_new_session_can_read_what_was_asked_before() {
        let fixture = Fixture::new();

        fixture
            .run(&[
                "ask",
                "--asset",
                &fixture.asset_id,
                "--text",
                "클럭 도메인 분리 이유",
                "--became-unit",
                "--format",
                "json-v2",
            ])
            .expect_success();
        fixture
            .run(&[
                "ask",
                "--asset",
                &fixture.asset_id,
                "--text",
                "ADC 샘플링 레이트가 왜 이 값인지",
            ])
            .expect_success();

        let listed = fixture
            .run(&[
                "ask",
                "--asset",
                &fixture.asset_id,
                "--list",
                "--format",
                "json-v2",
            ])
            .expect_success();
        let report: serde_json::Value = serde_json::from_str(&listed).unwrap();

        assert_eq!(report["command"], "questions.list");
        assert_eq!(report["data"]["asset_id"], fixture.asset_id);
        let items = report["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["text"], "클럭 도메인 분리 이유");
        assert_eq!(items[0]["became_unit"], true);
        assert_eq!(items[1]["became_unit"], false);

        let human = fixture
            .run(&["ask", "--asset", &fixture.asset_id, "--list"])
            .expect_success();
        assert!(human.contains("클럭 도메인 분리 이유"), "{human}");
        assert!(human.contains("노트에 반영됨"), "{human}");
        assert!(
            !human.contains("personal-question-"),
            "the owner reads questions, not identifiers: {human}"
        );
    }

    // Material nobody has asked about answers with an empty list, not an error:
    // "nothing yet" is an ordinary state on the first session.
    #[test]
    #[allow(deprecated)]
    fn material_nobody_asked_about_answers_with_an_empty_list() {
        let fixture = Fixture::new();

        let listed = fixture
            .run(&[
                "ask",
                "--asset",
                &fixture.asset_id,
                "--list",
                "--format",
                "json-v2",
            ])
            .expect_success();

        let report: serde_json::Value = serde_json::from_str(&listed).unwrap();
        assert_eq!(report["result"], "ok");
        assert_eq!(report["data"]["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    #[allow(deprecated)]
    fn a_question_about_nothing_identifiable_is_refused() {
        let fixture = Fixture::new();

        let output = fixture
            .run(&[
                "ask",
                "--asset",
                "not-an-asset",
                "--text",
                "물어본 것",
                "--format",
                "json-v2",
            ])
            .expect_failure();

        let report: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["error"]["code"], "question_invalid");
    }

    #[allow(deprecated)]
    struct Fixture {
        _root: tempfile::TempDir,
        repository: std::path::PathBuf,
        home: std::path::PathBuf,
        asset_id: String,
    }

    struct Run {
        stdout: String,
        stderr: String,
        success: bool,
    }

    impl Run {
        fn expect_success(self) -> String {
            assert!(
                self.success,
                "command failed: {}{}",
                self.stdout, self.stderr
            );
            self.stdout
        }

        fn expect_failure(self) -> String {
            assert!(
                !self.success,
                "command unexpectedly succeeded: {}",
                self.stdout
            );
            self.stdout
        }
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempdir().unwrap();
            let repository = root.path().join("kb");
            let provider = root.path().join("My-Knowledge-OS-Assets/personal/inbox");
            let home = root.path().join("home");
            scaffold_personal_kb_v2(&repository).unwrap();
            fs::create_dir_all(&provider).unwrap();
            fs::create_dir(&home).unwrap();
            write_machine_profile(&home, &repository, &provider);
            Self {
                _root: root,
                repository,
                home,
                asset_id: format!("personal-asset-{}", "a".repeat(64)),
            }
        }

        #[allow(deprecated)]
        fn run(&self, arguments: &[&str]) -> Run {
            let output = Command::new(assert_cmd::cargo::cargo_bin("mko"))
                .args(arguments)
                .env_remove("MKO_PERSONAL_PROVIDER_ROOT")
                .env("HOME", &self.home)
                .current_dir(&self.repository)
                .output()
                .unwrap();
            Run {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                success: output.status.success(),
            }
        }
    }

    fn write_machine_profile(home: &Path, repository: &Path, provider: &Path) {
        use mko_core::{
            context::Scope,
            profile::{MachineProfileFile, PersonalProfile, ProfileStore},
        };

        ProfileStore::at(home.join("Library/Application Support/mko/profiles.yaml"))
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: repository.to_path_buf(),
                        provider_root: provider.to_path_buf(),
                        scope: Scope::Personal,
                    },
                )]),
            })
            .unwrap();
    }
}
