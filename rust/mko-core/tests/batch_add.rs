use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use chrono::{TimeZone, Utc};
use mko_core::{
    add::{AddInput, AddRequest, AddRunResult, BackupAttestation, add},
    clock::Clock,
    context::{ContextSource, ResolvedPersonalContext, Scope},
    json_v1::{AddOutcome, NextAction, UserState},
    provider_scan::ElapsedClock,
};
use tempfile::TempDir;

struct FixedClock;

impl Clock for FixedClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 19, 1, 2, 3).single().unwrap()
    }
}

struct FixedElapsed;

impl ElapsedClock for FixedElapsed {
    fn elapsed_ms(&self) -> u64 {
        0
    }
}

struct ReplacingAuditClock {
    path: PathBuf,
    replacement: Vec<u8>,
    fired: AtomicBool,
}

impl ReplacingAuditClock {
    fn new(path: PathBuf, replacement: &[u8]) -> Self {
        Self {
            path,
            replacement: replacement.to_vec(),
            fired: AtomicBool::new(false),
        }
    }
}

impl Clock for ReplacingAuditClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        if !self.fired.swap(true, Ordering::SeqCst) {
            fs::write(&self.path, &self.replacement).unwrap();
        }
        FixedClock.now_utc()
    }
}

struct DeletingAuditClock {
    path: PathBuf,
    fired: AtomicBool,
}

impl DeletingAuditClock {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            fired: AtomicBool::new(false),
        }
    }
}

impl Clock for DeletingAuditClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        if !self.fired.swap(true, Ordering::SeqCst) {
            fs::remove_file(&self.path).unwrap();
        }
        FixedClock.now_utc()
    }
}

struct NthCallElapsed {
    calls: AtomicU64,
    limit_call: u64,
}

impl NthCallElapsed {
    fn new(limit_call: u64) -> Self {
        Self {
            calls: AtomicU64::new(0),
            limit_call,
        }
    }
}

impl ElapsedClock for NthCallElapsed {
    fn elapsed_ms(&self) -> u64 {
        if self.calls.fetch_add(1, Ordering::Relaxed) >= self.limit_call {
            10_000
        } else {
            0
        }
    }
}

#[test]
fn mixed_batch_persists_safe_items_and_reports_item_failures() {
    let fixture = Fixture::new();
    fixture.pdf("z-new.pdf", b"%PDF-1.7\nnew");
    fixture.pdf("a-known.pdf", b"%PDF-1.7\nknown");

    let known = add(
        AddRequest::new(
            fixture.context(),
            AddInput::File(fixture.provider.join("a-known.pdf")),
        )
        .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap();
    assert!(matches!(known, AddRunResult::Single(_)));
    fixture.pdf("m-invalid.pdf", b"not a PDF");

    let result = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap();
    let AddRunResult::Batch(batch) = result else {
        panic!("expected batch result")
    };

    assert_eq!(
        batch
            .items
            .iter()
            .map(|item| item.provider_locator.as_str())
            .collect::<Vec<_>>(),
        vec!["z-new.pdf", "a-known.pdf", "m-invalid.pdf"]
    );
    let new = &batch.items[0];
    assert_eq!(new.user_state, UserState::Registered);
    assert_eq!(new.next_action, NextAction::Prepare);
    assert!(new.asset_id.is_some());
    let known = &batch.items[1];
    assert_eq!(known.user_state, UserState::Registered);
    assert_eq!(known.next_action, NextAction::Prepare);
    assert!(known.asset_id.is_some());
    let invalid = &batch.items[2];
    assert_eq!(invalid.user_state, UserState::Blocked);
    assert_eq!(invalid.next_action, NextAction::Repair);
    assert_eq!(invalid.error.as_ref().unwrap().code, "invalid_pdf");
    assert!(fixture.registry_count() >= 2);
}

#[test]
fn retry_resumes_from_the_canonical_registered_state() {
    let fixture = Fixture::new();
    fixture.pdf("paper.pdf", b"%PDF-1.7\ncontent");
    let request = || {
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified)
    };

    let AddRunResult::Batch(first) = add(request(), &FixedClock, &FixedElapsed).unwrap() else {
        panic!("expected batch")
    };
    let AddRunResult::Batch(second) = add(request(), &FixedClock, &FixedElapsed).unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(
        first.items[0].next_action,
        NextAction::Prepare,
        "{first:#?}"
    );
    assert_eq!(
        second.items[0].next_action,
        NextAction::Prepare,
        "{second:#?}"
    );
    assert_eq!(first.items[0].asset_id, second.items[0].asset_id);
    assert_eq!(fixture.registry_count(), 1);
}

#[cfg(unix)]
#[test]
fn batch_never_registers_a_symlinked_pdf() {
    let fixture = Fixture::new();
    let outside = fixture.root.path().join("outside.pdf");
    fs::write(&outside, b"%PDF-1.7\noutside").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.provider.join("link.pdf")).unwrap();

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert!(
        batch
            .items
            .iter()
            .all(|item| item.provider_locator != "link.pdf")
    );
    assert_eq!(fixture.registry_count(), 0);
}

#[test]
fn batch_never_infers_backup_attestation() {
    let fixture = Fixture::new();
    fixture.pdf("only-copy.pdf", b"%PDF-1.7\nonly copy");

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(
        batch.items[0].error.as_ref().unwrap().code,
        "backup_confirmation_required"
    );
    assert_eq!(fixture.registry_count(), 0);
}

#[test]
fn batch_mutates_only_the_first_twenty_nfc_ordered_actionable_items() {
    let fixture = Fixture::new();
    for index in (0..21).rev() {
        fixture.pdf(
            &format!("paper-{index:02}.pdf"),
            format!("%PDF-1.7\n{index}").as_bytes(),
        );
    }

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(batch.items.len(), 20);
    assert_eq!(batch.remaining, 1);
    assert_eq!(batch.items[0].provider_locator, "paper-00.pdf");
    assert_eq!(batch.items[19].provider_locator, "paper-19.pdf");
    assert_eq!(fixture.registry_count(), 20);

    let AddRunResult::Batch(second) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };
    assert_eq!(fixture.registry_count(), 21, "{second:#?}");
    assert!(
        second
            .items
            .iter()
            .any(|item| item.provider_locator == "paper-20.pdf"),
        "{second:#?}"
    );

    let AddRunResult::Batch(third) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };
    assert_eq!(fixture.registry_count(), 21, "{third:#?}");
    assert!(
        third
            .items
            .iter()
            .all(|item| item.next_action != NextAction::Add),
        "{third:#?}"
    );
}

#[test]
fn duplicate_pdf_bytes_converge_to_one_canonical_asset() {
    let fixture = Fixture::new();
    fixture.pdf("a-copy.pdf", b"%PDF-1.7\nsame content");
    fixture.pdf("b-copy.pdf", b"%PDF-1.7\nsame content");

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(batch.items.len(), 2);
    assert!(
        batch.items.iter().all(|item| item.error.is_none()),
        "{batch:#?}"
    );
    assert_eq!(batch.items[0].asset_id, batch.items[1].asset_id);
    assert_eq!(batch.items[0].add_outcome, Some(AddOutcome::Created));
    assert_eq!(batch.items[1].add_outcome, Some(AddOutcome::Existing));
    assert_eq!(fixture.registry_count(), 1);

    let AddRunResult::Batch(retry) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };
    assert!(
        retry
            .items
            .iter()
            .all(|item| item.next_action != NextAction::Add),
        "{retry:#?}"
    );
    assert!(
        retry
            .items
            .iter()
            .all(|item| item.add_outcome == Some(AddOutcome::Existing)),
        "{retry:#?}"
    );
    assert_eq!(fixture.registry_count(), 1);
}

#[test]
fn decomposed_unicode_physical_path_keeps_a_normalized_logical_locator() {
    use unicode_normalization::UnicodeNormalization;

    let fixture = Fixture::new();
    let physical_name = "cafe\u{301}.pdf";
    let logical_name = physical_name.nfc().collect::<String>();
    assert_ne!(physical_name.as_bytes(), logical_name.as_bytes());
    fixture.pdf(physical_name, b"%PDF-1.7\nunicode path");

    let AddRunResult::Batch(first) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(first.items[0].provider_locator, logical_name);
    assert_eq!(
        first.items[0].next_action,
        NextAction::Prepare,
        "{first:#?}"
    );
    assert!(fixture.provider.join(physical_name).is_file());
    assert_eq!(fixture.registry_count(), 1);

    let registry = fs::read_to_string(
        fs::read_dir(fixture.repository.join("assets/registry"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(registry.contains(&logical_name), "{registry}");

    let AddRunResult::Batch(second) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };
    assert_eq!(
        second.items[0].next_action,
        NextAction::Prepare,
        "{second:#?}"
    );
    assert_eq!(second.items[0].add_outcome, Some(AddOutcome::Existing));
}

#[test]
fn same_size_replacement_after_discovery_is_rejected_without_registration() {
    let fixture = Fixture::new();
    let path = fixture.provider.join("paper.pdf");
    let original = b"%PDF-1.7\noriginal-A";
    let replacement = b"%PDF-1.7\nchanged--B";
    assert_eq!(original.len(), replacement.len());
    fixture.pdf("paper.pdf", original);

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &ReplacingAuditClock::new(path, replacement),
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(
        batch.items[0].error.as_ref().unwrap().code,
        "fingerprint_changed"
    );
    assert_eq!(
        batch.items[0].error.as_ref().unwrap().message,
        "The inbox PDF changed during processing; retry the scan."
    );
    assert_eq!(
        batch.items[0].error.as_ref().unwrap().recovery,
        Some(mko_core::json_v1::Recovery {
            kind: mko_core::json_v1::RecoveryKind::Retry,
        })
    );
    assert_eq!(fixture.registry_count(), 0);
}

#[test]
fn deletion_after_discovery_is_rejected_without_registration() {
    let fixture = Fixture::new();
    let path = fixture.provider.join("paper.pdf");
    fixture.pdf("paper.pdf", b"%PDF-1.7\ndelete me");

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &DeletingAuditClock::new(path),
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    let error = batch.items[0].error.as_ref().expect("stable item error");
    assert_eq!(error.code, "file_unreadable");
    assert_eq!(
        error.message,
        "The inbox PDF could not be reopened safely; retry after it is available."
    );
    assert_eq!(
        error.recovery,
        Some(mko_core::json_v1::Recovery {
            kind: mko_core::json_v1::RecoveryKind::Retry,
        })
    );
    assert!(
        !error
            .message
            .contains(fixture.root.path().to_str().unwrap())
    );
    assert_eq!(fixture.registry_count(), 0);
}

#[test]
fn partial_provider_scan_never_registers_new_items() {
    let fixture = Fixture::new();
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 2 * 1024 * 1024));
    fixture.pdf("slow.pdf", &bytes);

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &NthCallElapsed::new(20),
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert!(!batch.scan_complete, "{batch:#?}");
    assert_eq!(fixture.registry_count(), 0);
}

#[test]
fn bounded_selection_reserves_work_queue_capacity_before_nfc_fill() {
    let fixture = Fixture::new();
    fixture.pdf("z-existing.pdf", b"%PDF-1.7\nexisting");
    add(
        AddRequest::new(
            fixture.context(),
            AddInput::File(fixture.provider.join("z-existing.pdf")),
        )
        .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap();
    for index in 0..20 {
        fixture.pdf(
            &format!("a-new-{index:02}.pdf"),
            format!("%PDF-1.7\n{index}").as_bytes(),
        );
    }

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert_eq!(batch.items.len(), 20);
    assert_eq!(batch.remaining, 1);
    assert_eq!(
        batch
            .items
            .iter()
            .filter(|item| item.provider_locator.starts_with("a-new-"))
            .count(),
        19,
        "{batch:#?}"
    );
    assert!(
        batch
            .items
            .iter()
            .any(|item| item.provider_locator == "z-existing.pdf"),
        "{batch:#?}"
    );
    assert_eq!(fixture.registry_count(), 20);
}

#[test]
fn repository_diagnostics_never_become_provider_locators_or_allow_registration() {
    let fixture = Fixture::new();
    fixture.pdf("paper.pdf", b"%PDF-1.7\nnew");
    fs::write(
        fixture.repository.join("assets/registry/broken.md"),
        "---\ntype: asset\ninvalid: [\n---\n",
    )
    .unwrap();

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedClock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert!(!batch.scan_complete, "{batch:#?}");
    assert_eq!(
        fixture.registry_count(),
        1,
        "the pre-existing broken record only"
    );
    assert!(batch.items.iter().all(|item| {
        !PathBuf::from(&item.provider_locator).is_absolute()
            && !item.provider_locator.contains("assets/registry")
            && !item.provider_locator.contains(';')
    }));
    assert_eq!(batch.items[0].next_action, NextAction::Retry, "{batch:#?}");
}

#[cfg(unix)]
#[test]
fn intermediate_directory_symlink_swap_is_rejected_without_outside_access() {
    struct SwappingAuditClock {
        directory: PathBuf,
        parked: PathBuf,
        outside: PathBuf,
        fired: AtomicBool,
    }

    impl Clock for SwappingAuditClock {
        fn now_utc(&self) -> chrono::DateTime<Utc> {
            if !self.fired.swap(true, Ordering::SeqCst) {
                fs::rename(&self.directory, &self.parked).unwrap();
                std::os::unix::fs::symlink(&self.outside, &self.directory).unwrap();
            }
            FixedClock.now_utc()
        }
    }

    let fixture = Fixture::new();
    let directory = fixture.provider.join("nested");
    let parked = fixture.provider.join("nested-parked");
    let outside = fixture.root.path().join("outside");
    fs::create_dir_all(&directory).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(directory.join("paper.pdf"), b"%PDF-1.7\ninside").unwrap();
    fs::write(outside.join("paper.pdf"), b"%PDF-1.7\noutside").unwrap();
    let clock = SwappingAuditClock {
        directory,
        parked,
        outside,
        fired: AtomicBool::new(false),
    };

    let AddRunResult::Batch(batch) = add(
        AddRequest::new(fixture.context(), AddInput::InboxScan)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &clock,
        &FixedElapsed,
    )
    .unwrap() else {
        panic!("expected batch")
    };

    assert!(batch.items[0].error.is_some(), "{batch:#?}");
    assert_eq!(fixture.registry_count(), 0);
}

struct Fixture {
    root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        fs::create_dir_all(repository.join("assets/registry")).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        Self {
            root,
            repository,
            provider,
        }
    }

    fn context(&self) -> ResolvedPersonalContext {
        ResolvedPersonalContext {
            repository_root: self.repository.clone(),
            provider_root: self.provider.clone(),
            provider_type: "google-drive-stream".into(),
            profile_name: "personal".into(),
            scope: Scope::Personal,
            source: ContextSource::Profile,
        }
    }

    fn pdf(&self, name: &str, bytes: &[u8]) {
        fs::write(self.provider.join(name), bytes).unwrap();
    }

    fn registry_count(&self) -> usize {
        fs::read_dir(self.repository.join("assets/registry"))
            .unwrap()
            .count()
    }
}
