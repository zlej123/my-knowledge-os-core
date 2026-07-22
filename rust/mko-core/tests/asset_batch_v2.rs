#[path = "support/pdf_fixture.rs"]
mod pdf_fixture;

use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
};

use mko_core::{
    asset_v2::{
        AssetRegistrationOutcomeV2, HydrationConfirmationV2, RegisterInboxAssetsRequestV2,
        register_inbox_pdf_assets_v2,
    },
    fingerprint::MAX_ASSET_BYTES,
    provider_scan::ElapsedClock,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::TempDir;

use pdf_fixture::write_pdf;

struct FixedElapsedClock;

impl ElapsedClock for FixedElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        0
    }
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

    fn register_batch(
        &self,
        hydration_confirmation: HydrationConfirmationV2,
    ) -> mko_core::asset_v2::InboxAssetRegistrationResultV2 {
        register_inbox_pdf_assets_v2(
            RegisterInboxAssetsRequestV2 {
                repository_root: &self.repository,
                provider_root: &self.provider,
                hydration_confirmation,
            },
            &FixedElapsedClock,
        )
        .unwrap()
    }
}

#[test]
fn mixed_batch_keeps_valid_and_duplicate_successes_beside_item_failures() {
    let env = TestEnv::new();
    let first = env.provider.join("a-valid.pdf");
    write_pdf(&first, &["Same immutable content".into()]);
    fs::copy(&first, env.provider.join("b-duplicate.pdf")).unwrap();
    fs::write(env.provider.join("c-invalid.pdf"), b"not a PDF").unwrap();
    let too_large = env.provider.join("d-too-large.pdf");
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&too_large)
        .unwrap();
    file.set_len(MAX_ASSET_BYTES + 1).unwrap();

    let result = env.register_batch(HydrationConfirmationV2::Confirmed);

    assert!(result.scan_complete);
    assert_eq!(result.remaining, 0);
    assert!(result.warnings.is_empty());
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.logical_locator.as_str())
            .collect::<Vec<_>>(),
        vec![
            "a-valid.pdf",
            "b-duplicate.pdf",
            "c-invalid.pdf",
            "d-too-large.pdf"
        ]
    );

    let first_registration = result.items[0].registration.as_ref().unwrap();
    let duplicate_registration = result.items[1].registration.as_ref().unwrap();
    assert_eq!(
        first_registration.outcome,
        AssetRegistrationOutcomeV2::Created
    );
    assert_eq!(
        duplicate_registration.outcome,
        AssetRegistrationOutcomeV2::Existing
    );
    assert_eq!(first_registration.asset.id, duplicate_registration.asset.id);
    assert_eq!(
        result.items[2].error.as_ref().unwrap().code(),
        "invalid_pdf"
    );
    assert_eq!(
        result.items[3].error.as_ref().unwrap().code(),
        "file_too_large"
    );
    assert_eq!(
        fs::read_dir(env.repository.join("assets/registry"))
            .unwrap()
            .count(),
        1
    );

    let retry = env.register_batch(HydrationConfirmationV2::Confirmed);
    assert_eq!(
        retry.items[0].registration.as_ref().unwrap().outcome,
        AssetRegistrationOutcomeV2::Existing
    );
    assert_eq!(
        retry.items[1].registration.as_ref().unwrap().outcome,
        AssetRegistrationOutcomeV2::Existing
    );
}

#[test]
fn registration_failure_for_one_readable_pdf_does_not_erase_other_success() {
    let env = TestEnv::new();
    write_pdf(&env.provider.join("a-small.pdf"), &["Small".into()]);
    let large = env.provider.join("b-needs-confirmation.pdf");
    let mut bytes = vec![0_u8; 11 * 1024 * 1024];
    bytes[..5].copy_from_slice(b"%PDF-");
    fs::write(&large, bytes).unwrap();

    let result = env.register_batch(HydrationConfirmationV2::NotConfirmed);

    assert!(result.scan_complete);
    assert!(result.items[0].registration.is_some());
    assert_eq!(
        result.items[1].error.as_ref().unwrap().code(),
        "hydration_confirmation_required"
    );
    assert_eq!(
        fs::read_dir(env.repository.join("assets/registry"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn batch_ceiling_is_deterministic_and_remaining_counts_known_omissions() {
    let env = TestEnv::new();
    for index in 0..21 {
        write_pdf(
            &env.provider.join(format!("paper-{index:02}.pdf")),
            &[format!("Document {index}")],
        );
    }

    let result = env.register_batch(HydrationConfirmationV2::Confirmed);

    assert!(!result.scan_complete);
    assert_eq!(result.items.len(), 20);
    assert_eq!(result.remaining, 1);
    assert_eq!(result.items[0].logical_locator, "paper-00.pdf");
    assert_eq!(result.items[19].logical_locator, "paper-19.pdf");
    assert!(result.warnings.iter().any(|warning| {
        warning.code == "batch_item_limit" && warning.provider_locator.is_none()
    }));
    assert_eq!(
        fs::read_dir(env.repository.join("assets/registry"))
            .unwrap()
            .count(),
        20
    );
}
