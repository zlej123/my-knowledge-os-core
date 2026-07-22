#[path = "support/pdf_fixture.rs"]
mod pdf_fixture;

use std::{fs, path::PathBuf};

use mko_core::{
    asset_v2::{
        AssetRegistrationOutcomeV2, HydrationConfirmationV2, RegisterAssetRequestV2, read_asset_v2,
        register_pdf_asset_v2,
    },
    config_v2::{DerivedArtifactsPolicyV2, KnowledgeConfigV2},
    fingerprint::fingerprint_file,
    model_v2::PreparedMetadataV2,
    prepared_v2::{
        PreparePdfAssetRequestV2, PreparedPersistenceOutcomeV2, prepare_pdf_asset_v2_with_extractor,
    },
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::TempDir;

use pdf_fixture::write_pdf;

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
fn prepared_plaintext_is_local_only_private_bounded_and_idempotent() {
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
                .join(".mko/runtime/cache/prepared")
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
