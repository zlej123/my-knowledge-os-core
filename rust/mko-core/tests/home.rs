use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

use mko_core::{
    home::{
        HomeNextAction, HomeReport, RepositoryGeneration, detect_repository_generation,
        inspect_home,
    },
    provider_scan::ElapsedClock,
    scaffold_v2::scaffold_personal_kb_v2,
};

struct FixedElapsedClock(AtomicU64);

impl FixedElapsedClock {
    fn new() -> Self {
        Self(AtomicU64::new(0))
    }
}

impl ElapsedClock for FixedElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn legacy_configuration_is_detected_before_v3_parsing_errors_escape() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("knowledge-os.yaml"),
        "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
    )
    .unwrap();

    assert_eq!(
        detect_repository_generation(root.path()).unwrap(),
        RepositoryGeneration::LegacyV1
    );
}

#[test]
fn v3_home_inspection_is_read_only_and_counts_new_provider_pdfs() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("provider");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir(&provider).unwrap();
    fs::write(provider.join("paper.pdf"), b"%PDF-1.7\nfixture").unwrap();

    let report = inspect_home(&repository, &provider, &FixedElapsedClock::new()).unwrap();
    let HomeReport::V3(report) = report else {
        panic!("expected a v3 home report");
    };
    assert_eq!(report.new_material, 1);
    assert_eq!(report.registered, 0);
    assert_eq!(report.review_pending, 0);
    assert_eq!(report.approved_knowledge, 0);
    assert_eq!(report.blocked, 0);
    assert_eq!(HomeReport::V3(report).next_action(), HomeNextAction::Add);
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

// Registered material that never became a record is real work nobody is
// waiting on: extraction can fail and a session can end mid-way. Counting it as
// nothing at all made the owner's home read "새 자료 0 · 검토 1 · 문제 0" while
// two of their PDFs sat unprocessed.
#[test]
fn material_registered_but_not_yet_recorded_stays_visible_on_home() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("provider");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir(&provider).unwrap();
    fs::write(provider.join("stuck.pdf"), b"%PDF-1.7\nfixture").unwrap();

    let registration =
        mko_core::asset_v2::register_pdf_asset_v2(mko_core::asset_v2::RegisterAssetRequestV2 {
            repository_root: &repository,
            provider_root: &provider,
            logical_locator: "stuck.pdf",
            hydration_confirmation: mko_core::asset_v2::HydrationConfirmationV2::NotConfirmed,
        })
        .unwrap();
    assert!(!registration.asset.id.is_empty());

    let report = inspect_home(&repository, &provider, &FixedElapsedClock::new()).unwrap();
    let HomeReport::V3(report) = report else {
        panic!("expected a v3 home report");
    };

    assert_eq!(report.new_material, 0, "it is no longer new");
    assert_eq!(report.registered, 1);
    assert_eq!(
        report.in_progress, 1,
        "registered without a record is unfinished work"
    );
    assert_eq!(report.review_pending, 0);
    assert_eq!(
        HomeReport::V3(report).next_action(),
        HomeNextAction::Add,
        "home must point at the material instead of recommending nothing"
    );
}

// A count without a reason leaves the owner guessing, and guessing wrong sends
// them around a loop that fails the same way. Each recorded failure has to
// resolve to the one action that would actually move the item.
#[test]
fn stopped_material_reports_why_it_stopped() {
    use mko_core::attempt_v2::{
        PreparationOutcomeV2, StuckReasonV2, record_preparation_attempt_v2,
    };

    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
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

    let reason = |repository: &std::path::Path| {
        let HomeReport::V3(report) =
            inspect_home(repository, &provider, &FixedElapsedClock::new()).unwrap()
        else {
            panic!("expected a v3 home report");
        };
        assert_eq!(report.stuck.len(), 1);
        assert_eq!(report.stuck[0].asset_id, asset.id);
        assert_eq!(report.stuck[0].title, asset.title_fallback);
        report.stuck[0].reason
    };

    // Registered and untouched: not a failure, and not a wrong claim either.
    assert_eq!(reason(&repository), StuckReasonV2::NotAttempted);

    for (code, expected) in [
        ("pdf_text_unreadable", StuckReasonV2::TextUnreadable),
        (
            "hydration_confirmation_required",
            StuckReasonV2::DownloadRequired,
        ),
        ("prepared_text_invalid", StuckReasonV2::Retryable),
    ] {
        record_preparation_attempt_v2(
            &repository,
            &asset.id,
            PreparationOutcomeV2::Failed,
            Some(code),
            &FixedClock(
                chrono::DateTime::parse_from_rfc3339("2026-08-04T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
                    + chrono::Duration::minutes(match code {
                        "pdf_text_unreadable" => 1,
                        "hydration_confirmation_required" => 2,
                        _ => 3,
                    }),
            ),
        )
        .unwrap();
        assert_eq!(reason(&repository), expected, "code {code}");
    }
}

#[derive(Clone, Copy)]
struct FixedClock(chrono::DateTime<chrono::Utc>);

impl mko_core::clock::Clock for FixedClock {
    fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}
