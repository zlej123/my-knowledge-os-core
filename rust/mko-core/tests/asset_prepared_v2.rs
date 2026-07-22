#[path = "support/pdf_fixture.rs"]
mod pdf_fixture;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use mko_core::{
    asset_v2::{
        AssetRegistrationOutcomeV2, HydrationConfirmationV2, RegisterAssetRequestV2, read_asset_v2,
        register_pdf_asset_v2,
    },
    clock::Clock,
    config_v2::{DerivedArtifactsPolicyV2, KnowledgeConfigV2},
    fingerprint::fingerprint_file,
    model_v2::PreparedMetadataV2,
    prepared_v2::{
        PreparePdfAssetRequestV2, PreparedPersistenceOutcomeV2,
        prepare_pdf_asset_v2_with_extractor, prepare_pdf_asset_v2_with_extractor_and_clock,
        read_prepared_content_v2_with_clock,
    },
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::TempDir;

use pdf_fixture::write_pdf;

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

fn at(value: &str) -> FixedClock {
    FixedClock(
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc),
    )
}

fn replace_private_file(source: &Path, destination: &Path, bytes: &[u8]) {
    fs::copy(source, destination).unwrap();
    fs::write(destination, bytes).unwrap();
}

fn regular_file_names(directory: &Path) -> BTreeSet<String> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .expect("test filenames are Unicode")
        })
        .collect()
}

fn tree_contains_bytes(root: &Path, excluded: Option<&Path>, needle: &[u8]) -> bool {
    let excluded = excluded.map(|path| fs::canonicalize(path).unwrap());
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let canonical = fs::canonicalize(&path).unwrap();
        if excluded.as_ref().is_some_and(|value| value == &canonical) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            pending.extend(
                fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else if metadata.is_file()
            && fs::read(&path)
                .unwrap()
                .windows(needle.len())
                .any(|window| window == needle)
        {
            return true;
        }
    }
    false
}

struct TestEnv {
    _root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        Self {
            _root: root,
            repository,
            provider,
        }
    }

    fn write_pdf(&self, relative: &str, text: &str) {
        let path = self.provider.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_pdf(&path, &[text.into()]);
    }

    fn register(&self, locator: &str) -> mko_core::asset_v2::AssetRegistrationResultV2 {
        register_pdf_asset_v2(RegisterAssetRequestV2 {
            repository_root: &self.repository,
            provider_root: &self.provider,
            logical_locator: locator,
            hydration_confirmation: HydrationConfirmationV2::Confirmed,
        })
        .unwrap()
    }

    fn prepare(
        &self,
        asset_id: &str,
    ) -> Result<mko_core::prepared_v2::PreparedPdfResultV2, mko_core::error::MkoError> {
        prepare_pdf_asset_v2_with_extractor(
            PreparePdfAssetRequestV2 {
                repository_root: &self.repository,
                provider_root: &self.provider,
                asset_id,
                metadata: PreparedMetadataV2 {
                    title: Some("Fixture".into()),
                    authors: vec!["Researcher".into()],
                    created_at: None,
                },
                hydration_confirmation: HydrationConfirmationV2::Confirmed,
            },
            |_, _| Ok(vec!["Exact extracted evidence".into()]),
        )
    }

    fn prepare_at(
        &self,
        asset_id: &str,
        clock: &dyn Clock,
    ) -> Result<mko_core::prepared_v2::PreparedPdfResultV2, mko_core::error::MkoError> {
        prepare_pdf_asset_v2_with_extractor_and_clock(
            PreparePdfAssetRequestV2 {
                repository_root: &self.repository,
                provider_root: &self.provider,
                asset_id,
                metadata: PreparedMetadataV2 {
                    title: Some("Fixture".into()),
                    authors: vec!["Researcher".into()],
                    created_at: None,
                },
                hydration_confirmation: HydrationConfirmationV2::Confirmed,
            },
            clock,
            |_, _| Ok(vec!["Exact extracted evidence".into()]),
        )
    }
}

#[test]
fn registration_uses_full_sha256_and_identical_retry_is_immutable() {
    let env = TestEnv::new();
    env.write_pdf("Inbox/paper.pdf", "Full hash fixture");

    let first = env.register("Inbox/paper.pdf");
    let bytes = fs::read(&first.registry_path).unwrap();
    let second = env.register("Inbox/paper.pdf");
    let fingerprint = fingerprint_file(&env.provider.join("Inbox/paper.pdf")).unwrap();

    assert_eq!(first.outcome, AssetRegistrationOutcomeV2::Created);
    assert_eq!(second.outcome, AssetRegistrationOutcomeV2::Existing);
    assert_eq!(first.asset.fingerprint, fingerprint.value);
    assert_eq!(first.asset, second.asset);
    assert_eq!(fs::read(&second.registry_path).unwrap(), bytes);
    assert_eq!(
        read_asset_v2(&env.repository, &first.asset.id).unwrap(),
        first.asset
    );
}

#[test]
fn large_provider_file_requires_explicit_full_hydration_confirmation() {
    let env = TestEnv::new();
    let path = env.provider.join("large.pdf");
    let mut bytes = vec![0_u8; 11 * 1024 * 1024];
    bytes[..5].copy_from_slice(b"%PDF-");
    fs::write(&path, bytes).unwrap();

    let error = register_pdf_asset_v2(RegisterAssetRequestV2 {
        repository_root: &env.repository,
        provider_root: &env.provider,
        logical_locator: "large.pdf",
        hydration_confirmation: HydrationConfirmationV2::NotConfirmed,
    })
    .unwrap_err();

    assert_eq!(error.code(), "hydration_confirmation_required");
    assert!(error.message().contains("11534336 bytes"));
    assert!(
        fs::read_dir(env.repository.join("assets/registry"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(
        env.register("large.pdf").outcome,
        AssetRegistrationOutcomeV2::Created
    );
}

#[test]
fn changed_registered_provider_snapshot_blocks_preparation_without_cache_output() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Original");
    let registered = env.register("paper.pdf");
    env.write_pdf("paper.pdf", "Changed provider content");

    let error = env.prepare(&registered.asset.id).unwrap_err();

    assert_eq!(error.code(), "registered_asset_changed");
    assert!(!env.repository.join(".mko/runtime").exists());
}

#[test]
fn prepared_plaintext_is_local_only_private_bounded_session_and_idempotent() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Prepare me");
    let registered = env.register("paper.pdf");
    let provider_entries_before = fs::read_dir(&env.provider).unwrap().count();

    let first = env.prepare(&registered.asset.id).unwrap();
    let first_bytes = fs::read(&first.bundle_path).unwrap();
    let second = env.prepare(&registered.asset.id).unwrap();

    assert_eq!(first.outcome, PreparedPersistenceOutcomeV2::Created);
    assert_eq!(second.outcome, PreparedPersistenceOutcomeV2::Existing);
    assert_eq!(first.bundle, second.bundle);
    assert_eq!(fs::read(&second.bundle_path).unwrap(), first_bytes);
    assert!(
        first.bundle_path.starts_with(
            fs::canonicalize(&env.repository)
                .unwrap()
                .join(".mko/runtime/sessions/prepared")
        )
    );
    assert_eq!(
        fs::read(env.repository.join(".mko/.gitignore")).unwrap(),
        b"runtime/\n"
    );
    assert_eq!(
        fs::read_dir(&env.provider).unwrap().count(),
        provider_entries_before
    );
    assert!(
        fs::read_to_string(&first.bundle_path)
            .unwrap()
            .contains("Exact extracted evidence")
    );
    assert!(!tree_contains_bytes(
        &env.provider,
        None,
        b"Exact extracted evidence"
    ));
    assert!(!tree_contains_bytes(
        &env.repository,
        Some(&env.repository.join(".mko/runtime")),
        b"Exact extracted evidence"
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(env.repository.join(".mko/runtime"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(&first.bundle_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}

#[test]
fn prepare_removes_recognized_crash_temporary_file_without_parsing_it() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Crash cleanup");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let digest = prepared
        .bundle
        .bundle_id
        .strip_prefix("prepared-content-sha256-")
        .unwrap();
    let temporary = prepared
        .bundle_path
        .parent()
        .unwrap()
        .join(format!(".mko-prepared-session-{digest}-999-1.tmp"));
    replace_private_file(&prepared.bundle_path, &temporary, b"partial crash artifact");

    let repeated = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:01Z"))
        .unwrap();

    assert_eq!(repeated.outcome, PreparedPersistenceOutcomeV2::Existing);
    assert!(!temporary.exists());
}

#[test]
fn prepare_refuses_unmanaged_and_special_entries_without_deleting_them() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Hostile cleanup entry");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let directory = prepared.bundle_path.parent().unwrap();
    let unmanaged = directory.join("notes.txt");
    replace_private_file(&prepared.bundle_path, &unmanaged, b"not Core managed");

    let error = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:01Z"))
        .unwrap_err();
    assert_eq!(error.code(), "prepared_session_directory_invalid");
    assert!(unmanaged.exists());

    fs::remove_file(&unmanaged).unwrap();
    let digest = prepared
        .bundle
        .bundle_id
        .strip_prefix("prepared-content-sha256-")
        .unwrap();
    let special = directory.join(format!(".mko-prepared-session-{digest}-999-2.tmp"));
    fs::create_dir(&special).unwrap();
    let error = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:02Z"))
        .unwrap_err();
    assert_eq!(error.code(), "prepared_session_directory_invalid");
    assert!(special.is_dir());
}

#[cfg(unix)]
#[test]
fn prepare_refuses_managed_looking_symlink_without_following_or_deleting_it() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Hostile cleanup symlink");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let digest = prepared
        .bundle
        .bundle_id
        .strip_prefix("prepared-content-sha256-")
        .unwrap();
    let outside = env._root.path().join("outside.txt");
    fs::write(&outside, b"must survive").unwrap();
    let linked = prepared
        .bundle_path
        .parent()
        .unwrap()
        .join(format!(".mko-prepared-session-{digest}-999-3.tmp"));
    symlink(&outside, &linked).unwrap();

    let error = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:01Z"))
        .unwrap_err();

    assert_eq!(error.code(), "prepared_session_directory_invalid");
    assert_eq!(fs::read(&outside).unwrap(), b"must survive");
    assert!(
        fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn expired_sessions_are_cleaned_in_deterministic_bounded_batches() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Bounded cleanup");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let directory = prepared.bundle_path.parent().unwrap();
    let template: serde_json::Value =
        serde_json::from_slice(&fs::read(&prepared.bundle_path).unwrap()).unwrap();

    for index in 0..130 {
        let mut session = template.clone();
        session["bundle"]["metadata"]["title"] =
            serde_json::json!(format!("Expired fixture {index:03}"));
        let mut semantic = session["bundle"].clone();
        let object = semantic.as_object_mut().unwrap();
        object.remove("bundle_id");
        object.remove("content_digest");
        let digest = canonical_json_sha256(&semantic).unwrap();
        let bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
        session["bundle"]["bundle_id"] = serde_json::json!(bundle_id);
        session["bundle"]["content_digest"] = serde_json::json!(digest);
        let filename = format!(
            "{}.session.json",
            session["bundle"]["bundle_id"].as_str().unwrap()
        );
        replace_private_file(
            &prepared.bundle_path,
            &directory.join(filename),
            &canonical_json_bytes(&session).unwrap(),
        );
    }

    let before = regular_file_names(directory);
    assert_eq!(before.len(), 131);
    let first_batch = env
        .prepare_at(&registered.asset.id, &at("2026-07-24T00:00:01Z"))
        .unwrap();
    let after_first = regular_file_names(directory);
    let first_128: BTreeSet<_> = before.iter().take(128).cloned().collect();
    let current_name = first_batch
        .bundle_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut expected_after_first: BTreeSet<_> = before.difference(&first_128).cloned().collect();
    expected_after_first.insert(current_name.clone());
    assert_eq!(after_first, expected_after_first);

    let second_batch = env
        .prepare_at(&registered.asset.id, &at("2026-07-24T00:00:02Z"))
        .unwrap();
    assert_eq!(second_batch.outcome, PreparedPersistenceOutcomeV2::Existing);
    assert_eq!(
        regular_file_names(directory),
        BTreeSet::from([current_name])
    );
}

#[test]
fn prepared_session_expires_at_24_hours_and_reprepare_rotates_exact_artifact() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Prepare session expiry");
    let registered = env.register("paper.pdf");
    let created = at("2026-07-23T00:00:00Z");
    let first = env.prepare_at(&registered.asset.id, &created).unwrap();
    let first_bytes = fs::read(&first.bundle_path).unwrap();

    assert_eq!(first.created_at, created.now_utc());
    assert_eq!(
        first.expires_at,
        DateTime::parse_from_rfc3339("2026-07-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
    assert_eq!(
        read_prepared_content_v2_with_clock(&first.bundle_path, &at("2026-07-23T23:59:59Z"))
            .unwrap(),
        first.bundle
    );
    let expired =
        read_prepared_content_v2_with_clock(&first.bundle_path, &at("2026-07-24T00:00:00Z"))
            .unwrap_err();
    assert_eq!(expired.code(), "prepared_session_expired");
    assert!(expired.message().contains("prepare the Asset again"));

    let second = env
        .prepare_at(&registered.asset.id, &at("2026-07-24T00:00:01Z"))
        .unwrap();
    assert_eq!(second.outcome, PreparedPersistenceOutcomeV2::Created);
    assert_eq!(second.bundle_path, first.bundle_path);
    assert_ne!(fs::read(&second.bundle_path).unwrap(), first_bytes);
    assert_eq!(
        read_prepared_content_v2_with_clock(&second.bundle_path, &at("2026-07-24T00:00:02Z"))
            .unwrap(),
        second.bundle
    );
}

#[test]
fn prepared_session_rejects_caller_modified_lifetime_metadata() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Do not extend lifetime");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&prepared.bundle_path).unwrap()).unwrap();
    value["expires_at"] = serde_json::json!("2026-07-25T00:00:00Z");
    fs::write(&prepared.bundle_path, canonical_json_bytes(&value).unwrap()).unwrap();

    let error =
        read_prepared_content_v2_with_clock(&prepared.bundle_path, &at("2026-07-23T00:00:01Z"))
            .unwrap_err();
    assert_eq!(error.code(), "prepared_session_invalid");
    assert!(error.message().contains("Core-owned lifetime"));
}

#[cfg(unix)]
#[test]
fn prepared_session_read_does_not_follow_replacement_symlink() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "No follow");
    let registered = env.register("paper.pdf");
    let prepared = env
        .prepare_at(&registered.asset.id, &at("2026-07-23T00:00:00Z"))
        .unwrap();
    let outside = env._root.path().join("outside-session.json");
    fs::copy(&prepared.bundle_path, &outside).unwrap();
    fs::remove_file(&prepared.bundle_path).unwrap();
    symlink(&outside, &prepared.bundle_path).unwrap();

    let error =
        read_prepared_content_v2_with_clock(&prepared.bundle_path, &at("2026-07-23T00:00:01Z"))
            .unwrap_err();
    assert_eq!(error.code(), "local_runtime_invalid");
}

#[test]
fn provider_derived_artifact_policy_blocks_before_plaintext_or_runtime_write() {
    let env = TestEnv::new();
    env.write_pdf("paper.pdf", "Do not leak");
    let registered = env.register("paper.pdf");
    let mut config = KnowledgeConfigV2::read(&env.repository).unwrap();
    config.derived_artifacts = DerivedArtifactsPolicyV2::Provider;
    fs::write(
        env.repository.join("knowledge-os.yaml"),
        config.render().unwrap(),
    )
    .unwrap();

    let error = env.prepare(&registered.asset.id).unwrap_err();

    assert_eq!(error.code(), "derived_artifacts_policy_unsupported");
    assert!(!env.repository.join(".mko/runtime").exists());
    assert_eq!(fs::read_dir(&env.provider).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn provider_file_and_registry_symlinks_are_rejected_without_following_targets() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new();
    let outside = env._root.path().join("outside.pdf");
    write_pdf(&outside, &["Outside".into()]);
    symlink(&outside, env.provider.join("linked.pdf")).unwrap();
    let error = register_pdf_asset_v2(RegisterAssetRequestV2 {
        repository_root: &env.repository,
        provider_root: &env.provider,
        logical_locator: "linked.pdf",
        hydration_confirmation: HydrationConfirmationV2::Confirmed,
    })
    .unwrap_err();
    assert_eq!(error.code(), "file_unreadable");
    assert!(
        fs::read_dir(env.repository.join("assets/registry"))
            .unwrap()
            .next()
            .is_none()
    );

    env.write_pdf("real.pdf", "Real");
    let registered = env.register("real.pdf");
    fs::remove_file(&registered.registry_path).unwrap();
    symlink(&outside, &registered.registry_path).unwrap();
    assert_eq!(
        read_asset_v2(&env.repository, &registered.asset.id)
            .unwrap_err()
            .code(),
        "asset_registry_unreadable"
    );
}
