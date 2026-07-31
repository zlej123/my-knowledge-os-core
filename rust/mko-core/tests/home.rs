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
