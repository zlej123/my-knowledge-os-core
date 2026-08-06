#[cfg(target_os = "macos")]
mod macos {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use mko_core::scaffold_v2::scaffold_personal_kb_v2;
    use tempfile::tempdir;

    #[test]
    #[allow(deprecated)]
    fn legacy_home_quit_is_read_only_and_hides_v3_parser_errors() {
        let root = tempdir().unwrap();
        let repository = root.path().join("legacy-kb");
        let provider = root.path().join("provider");
        fs::create_dir_all(repository.join("assets/registry")).unwrap();
        fs::create_dir(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        let before = snapshot(&repository);

        let output = run_home_and_quit(&repository, &provider, root.path());

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(screen.contains("기존 지식베이스를 읽기 전용으로 열었습니다."));
        assert!(!screen.contains("unknown field"));
        assert_eq!(snapshot(&repository), before);
    }

    #[test]
    #[allow(deprecated)]
    fn healthy_v3_home_quit_is_read_only_and_shows_primary_actions() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        let before = snapshot(&repository);

        let output = run_home_and_quit(&repository, &provider, root.path());

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        for expected in [
            "새 자료 정리",
            "검토 계속",
            "지식 찾기",
            "빠른 메모",
            "다시 볼 지식",
        ] {
            assert!(screen.contains(expected), "missing {expected}: {screen}");
        }
        assert_eq!(snapshot(&repository), before);
    }

    // The count is useless if it stops at the report: this is the screen the
    // owner actually reads when they come back to unfinished work.
    #[test]
    #[allow(deprecated)]
    fn home_shows_material_that_was_registered_but_never_finished() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        fs::write(provider.join("stuck.pdf"), b"%PDF-1.7\nfixture").unwrap();
        mko_core::asset_v2::register_pdf_asset_v2(mko_core::asset_v2::RegisterAssetRequestV2 {
            repository_root: &repository,
            provider_root: &provider,
            logical_locator: "stuck.pdf",
            hydration_confirmation: mko_core::asset_v2::HydrationConfirmationV2::NotConfirmed,
        })
        .unwrap();
        let before = snapshot(&repository);

        let output = run_home_and_quit(&repository, &provider, root.path());

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(screen.contains("정리 중 1"), "missing the count: {screen}");
        assert!(
            screen.contains("멈춘 자료 계속 정리"),
            "home must recommend the unfinished material: {screen}"
        );
        assert!(
            screen.contains("정리하다 멈춘 자료 1"),
            "the action must say how much is waiting: {screen}"
        );
        assert!(
            screen.contains("아직 정리하지 않았습니다"),
            "registered but untouched is not a failure: {screen}"
        );
        assert_eq!(snapshot(&repository), before);
    }

    // A count leaves the owner guessing. When a failure is on file, home has to
    // name it and the one action that would move the item.
    #[test]
    #[allow(deprecated)]
    fn home_says_why_material_stopped_when_a_failure_is_on_file() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        fs::write(provider.join("stuck.pdf"), b"%PDF-1.7\nfixture").unwrap();
        let asset =
            mko_core::asset_v2::register_pdf_asset_v2(mko_core::asset_v2::RegisterAssetRequestV2 {
                repository_root: &repository,
                provider_root: &provider,
                logical_locator: "stuck.pdf",
                hydration_confirmation: mko_core::asset_v2::HydrationConfirmationV2::NotConfirmed,
            })
            .unwrap()
            .asset;
        mko_core::attempt_v2::record_preparation_attempt_v2(
            &repository,
            &asset.id,
            mko_core::attempt_v2::PreparationOutcomeV2::Failed,
            Some("pdf_text_unreadable"),
            &mko_core::clock::SystemClock,
        )
        .unwrap();

        let output = run_home_and_quit(&repository, &provider, root.path());

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(
            screen.contains("이 PDF의 텍스트를 읽을 수 없습니다"),
            "the reason must be named: {screen}"
        );
        assert!(
            screen.contains("새 사본을 Inbox에 넣고 등록"),
            "retrying would fail the same way, so say what would not: {screen}"
        );
        assert!(
            !screen.contains("아직 정리하지 않았습니다"),
            "a recorded failure must not read as untouched material: {screen}"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn empty_resurface_filter_is_id_free_and_read_only() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        let before = snapshot(&repository);

        let script = "set timeout 10\nset bin $env(MKO_TEST_BIN)\nspawn -noecho $bin\nexpect {\n  \"선택 ›\" { send -- \"5\\r\"; exp_continue }\n  \"관점 필터 ›\" { send -- \"\\r\"; exp_continue }\n  eof {}\n}\nset status [wait]\nexit [lindex $status 3]\n";
        let output = Command::new("/usr/bin/expect")
            .args(["-c", script])
            .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
            .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
            .env("HOME", root.path())
            .current_dir(&repository)
            .output()
            .unwrap();

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(screen.contains("관점으로 좁혀볼 수 있습니다."));
        assert!(screen.contains("이 관점으로 다시 볼 지식이 아직 없습니다."));
        assert!(!screen.contains("Knowledge ID"));
        assert_eq!(snapshot(&repository), before);
    }

    #[test]
    #[allow(deprecated)]
    fn quick_note_requires_unambiguous_confirmation_and_is_searchable_as_user_thought() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();

        let cancelled = run_remember(
            &repository,
            &provider,
            root.path(),
            "ADC sampling rate",
            "later",
        );
        assert!(cancelled.status.success());
        assert!(
            repository
                .join("notes")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );

        let saved = run_remember(
            &repository,
            &provider,
            root.path(),
            "ADC sampling rate",
            "y",
        );
        assert!(saved.status.success());
        assert!(String::from_utf8_lossy(&saved.stdout).contains("메모를 저장했습니다."));
        assert_eq!(repository.join("notes").read_dir().unwrap().count(), 1);

        let found = assert_cmd::Command::cargo_bin("mko")
            .unwrap()
            .args(["find", "sampling"])
            .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
            .current_dir(&repository)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(String::from_utf8_lossy(&found).contains("[내 생각] ADC sampling rate"));
    }

    #[allow(deprecated)]
    /// A record whose generated projection is gone: Core reports it blocked and
    /// wanting diagnosis, which is the state this guidance has to notice.
    fn seed_blocked_record(repository: &Path) {
        let provider = repository.parent().unwrap().join("seed-inbox");
        fs::create_dir_all(&provider).unwrap();
        fs::write(provider.join("paper.pdf"), b"%PDF-1.7\nfixture").unwrap();
        let asset =
            mko_core::asset_v2::register_pdf_asset_v2(mko_core::asset_v2::RegisterAssetRequestV2 {
                repository_root: repository,
                provider_root: &provider,
                logical_locator: "paper.pdf",
                hydration_confirmation: mko_core::asset_v2::HydrationConfirmationV2::NotConfirmed,
            })
            .unwrap()
            .asset;
        let bundle = mko_core::prepared_v2::build_pdf_prepared_content_v2(
            &asset,
            &["Evidence text for the test.".into()],
            mko_core::model_v2::PreparedMetadataV2 {
                title: Some("Seeded paper".into()),
                authors: Vec::new(),
                created_at: None,
            },
        )
        .unwrap();
        let evidence = mko_core::model_v2::EvidenceRefV2 {
            block_id: "block-000001".into(),
            locator: "page:1;chunk:1;granularity:coarse".into(),
            text_span_utf8: None,
            table_range: None,
        };
        let response = mko_core::model_v2::SourceResponseV2 {
            schema_version: 2,
            title: "Seeded paper".into(),
            authors: Vec::new(),
            publication_date: None,
            one_sentence_summary: "A bounded summary.".into(),
            general_summary: "A grounded general summary.".into(),
            key_claims: vec![mko_core::model_v2::SourceClaimV2 {
                text: "The evidence text exists.".into(),
                evidence_refs: vec![evidence],
            }],
            limitations: Vec::new(),
            tags: Vec::new(),
            knowledge_recommendation: mko_core::model_v2::KnowledgeRecommendationV2 {
                outcome: mko_core::model_v2::KnowledgeRecommendationOutcomeV2::Recommend,
                reasons: vec!["Reusable concept.".into()],
            },
        };
        let written = mko_core::records_v2::write_source_record_v2(
            mko_core::records_v2::WriteSourceRecordRequestV2 {
                repository_root: repository,
                asset: &asset,
                bundle: &bundle,
                response: &response,
                expected_revision: None,
            },
            &mko_core::clock::SystemClock,
        )
        .unwrap();
        fs::remove_file(repository.join(
            mko_core::projection_v2::record_projection_relative_path_v2(
                mko_core::projection_v2::ProjectionRecordTypeV2::Source,
                &written.record_id,
            ),
        ))
        .unwrap();
    }

    #[allow(deprecated)]
    fn run_home_and_quit(repository: &Path, provider: &Path, home: &Path) -> std::process::Output {
        let script = "set timeout 10\nset bin $env(MKO_TEST_BIN)\nspawn -noecho $bin\nexpect {\n  \"선택 ›\" { send -- \"q\\r\"; exp_continue }\n  eof {}\n}\nset status [wait]\nexit [lindex $status 3]\n";
        Command::new("/usr/bin/expect")
            .args(["-c", script])
            .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
            .env("MKO_PERSONAL_PROVIDER_ROOT", provider)
            .env("HOME", home)
            .current_dir(repository)
            .output()
            .unwrap()
    }

    #[allow(deprecated)]
    fn run_remember(
        repository: &Path,
        provider: &Path,
        home: &Path,
        text: &str,
        answer: &str,
    ) -> std::process::Output {
        let script = "set timeout 10\nset bin $env(MKO_TEST_BIN)\nset text $env(MKO_TEST_TEXT)\nset answer $env(MKO_TEST_ANSWER)\nspawn -noecho $bin remember $text\nexpect {\n  \"\\[y/N\\]\" { send -- \"$answer\\r\"; exp_continue }\n  eof {}\n}\nset status [wait]\nexit [lindex $status 3]\n";
        Command::new("/usr/bin/expect")
            .args(["-c", script])
            .env("MKO_TEST_BIN", assert_cmd::cargo::cargo_bin("mko"))
            .env("MKO_TEST_TEXT", text)
            .env("MKO_TEST_ANSWER", answer)
            .env("MKO_PERSONAL_PROVIDER_ROOT", provider)
            .env("HOME", home)
            .current_dir(repository)
            .output()
            .unwrap()
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        collect_files(root, root, &mut files);
        files
    }

    fn collect_files(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.file_type().unwrap().is_dir() {
                collect_files(root, &entry.path(), files);
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    // Finding nothing used to end the conversation. Search only covers approved
    // knowledge, so an owner whose material is all still waiting on them was
    // told "not found" about a shelf they had never filled.
    #[test]
    #[allow(deprecated)]
    fn an_empty_search_says_why_and_where_to_go() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();

        let output = Command::new(assert_cmd::cargo::cargo_bin("mko"))
            .args(["find", "sampling"])
            .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
            .env("HOME", root.path())
            .current_dir(&repository)
            .output()
            .unwrap();

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(screen.contains("승인된 지식에서 찾지 못했습니다."));
        assert!(
            screen.contains("아직 승인된 지식이 없습니다"),
            "an empty shelf must be named as such: {screen}"
        );
        assert!(
            screen.contains("`mko`"),
            "the owner needs somewhere to go: {screen}"
        );
    }

    // A blocked item is waiting on the owner more urgently than an unreviewed
    // one: it needs diagnosis. Telling them to go organize new material while
    // something is stuck is the dead end this guidance exists to remove.
    #[test]
    #[allow(deprecated)]
    fn an_empty_search_names_material_that_is_stuck() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        seed_blocked_record(&repository);

        let output = Command::new(assert_cmd::cargo::cargo_bin("mko"))
            .args(["find", "sampling"])
            .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
            .env("HOME", root.path())
            .current_dir(&repository)
            .output()
            .unwrap();

        assert!(output.status.success());
        let screen = String::from_utf8_lossy(&output.stdout);
        assert!(
            screen.contains("문제가 있어 멈춘 항목이 1개 있습니다."),
            "stuck material must be named: {screen}"
        );
        assert!(
            !screen.contains("새 자료를 정리하는 것부터"),
            "do not send the owner elsewhere while something is stuck: {screen}"
        );
    }
    // Opening a session and asking for the pile to be summarized needs Core to
    // answer "what is waiting". Home derived it as a count for the owner; an
    // agent had to re-scan and could not tell already-drafted material from
    // material still waiting.
    #[test]
    #[allow(deprecated)]
    fn core_can_be_asked_what_is_waiting_to_be_drafted() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        seed_blocked_record(&repository);

        let empty = Command::new(assert_cmd::cargo::cargo_bin("mko"))
            .args(["queue", "--pending-drafts", "--format", "json-v2"])
            .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
            .env("HOME", root.path())
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(empty.status.success());
        let report: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
        assert_eq!(report["command"], "queue.drafts");
        assert_eq!(report["schema_version"], 2);
        // The seeded record lives outside the provider, so nothing is waiting
        // there — and an empty answer still has to be a typed one.
        assert!(report["data"]["items"].is_array());
        assert!(report["data"]["scan_complete"].is_boolean());
    }

    // `mko setup` records where the material lives, so nobody exports a
    // variable afterwards. Reading that record only when the caller is
    // somewhere else made every command that needs material fail inside the
    // knowledge base itself — the one place the owner is most likely to run it.
    #[test]
    #[allow(deprecated)]
    fn a_configured_machine_needs_no_environment_variable_inside_its_own_repository() {
        let root = tempdir().unwrap();
        let repository = root.path().join("v3-kb");
        // doctor holds the provider root to the exact Personal Inbox, so the
        // fixture has to be the real shape or it fails for an unrelated reason.
        let provider = root.path().join("My-Knowledge-OS-Assets/personal/inbox");
        let home = root.path().join("home");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::create_dir(&home).unwrap();
        write_machine_profile(&home, &repository, &provider);

        for arguments in [
            vec!["queue", "--pending-drafts", "--format", "json-v2"],
            vec!["doctor", "--format", "json-v2"],
        ] {
            let output = Command::new(assert_cmd::cargo::cargo_bin("mko"))
                .args(&arguments)
                .env_remove("MKO_PERSONAL_PROVIDER_ROOT")
                .env("HOME", &home)
                .current_dir(&repository)
                .output()
                .unwrap();

            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{arguments:?} must work where the owner runs it: {stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !stdout.contains("provider_root_missing"),
                "the machine profile already answers this: {stdout}"
            );
        }

        // doctor reports trouble as a healthy exit carrying a blocked check, so
        // exiting zero proves nothing about what it decided. Its verdict is the
        // assertion: a configured machine is configured, wherever it is asked.
        let verdict = |directory: &Path| -> serde_json::Value {
            let output = Command::new(assert_cmd::cargo::cargo_bin("mko"))
                .args(["doctor", "--format", "json-v2"])
                .env_remove("MKO_PERSONAL_PROVIDER_ROOT")
                .env("HOME", &home)
                .current_dir(directory)
                .output()
                .unwrap();
            serde_json::from_slice(&output.stdout).unwrap()
        };

        let inside = verdict(&repository);
        let outside = verdict(root.path());
        assert_eq!(
            inside["data"]["healthy"], outside["data"]["healthy"],
            "the same machine cannot be healthy from one directory and broken from another:\n{inside}\n{outside}"
        );
        assert_eq!(
            inside["data"]["healthy"], true,
            "a configured machine must diagnose as configured: {inside}"
        );
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
