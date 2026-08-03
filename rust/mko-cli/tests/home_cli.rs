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
        assert_eq!(snapshot(&repository), before);
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
}
