use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use chrono::{TimeZone, Utc};
use mko_core::{
    add::{AddRequest, BackupAttestation, add_pdf},
    clock::Clock,
    context::{ContextSource, ResolvedPersonalContext, Scope},
    fingerprint::{MAX_ASSET_BYTES, fingerprint_file},
    json_v1::{AddOutcome, ImportOutcome},
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderScanRequest, ScanLimits, scan_provider_pdfs,
    },
};
use tempfile::TempDir;

const KNOWLEDGE_CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";

#[derive(Clone)]
struct FixedAuditClock;

impl Clock for FixedAuditClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 19, 1, 2, 3).single().unwrap()
    }
}

struct MutatingAuditClock {
    path: PathBuf,
}

impl Clock for MutatingAuditClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        fs::write(&self.path, b"%PDF-1.7\nreplacement").unwrap();
        Utc.with_ymd_and_hms(2026, 7, 19, 1, 2, 3).single().unwrap()
    }
}

#[derive(Default)]
struct FixedElapsedClock;

impl ElapsedClock for FixedElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        0
    }
}

struct AdvancingClock {
    now: AtomicU64,
    step: u64,
}

#[derive(Default)]
struct CountingElapsedClock {
    calls: AtomicU64,
}

impl CountingElapsedClock {
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ElapsedClock for CountingElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        0
    }
}

impl AdvancingClock {
    fn new(step: u64) -> Self {
        Self {
            now: AtomicU64::new(0),
            step,
        }
    }
}

impl ElapsedClock for AdvancingClock {
    fn elapsed_ms(&self) -> u64 {
        self.now.fetch_add(self.step, Ordering::SeqCst)
    }
}

struct MutatingClock {
    calls: AtomicU64,
    path: PathBuf,
}

struct PreOpenMutatingClock {
    calls: AtomicU64,
    path: PathBuf,
}

impl ElapsedClock for PreOpenMutatingClock {
    fn elapsed_ms(&self) -> u64 {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 4 {
            fs::write(&self.path, b"not a PDF after the directory entry check").unwrap();
        }
        0
    }
}

#[cfg(unix)]
struct SwapToLinkClock {
    calls: AtomicU64,
    path: PathBuf,
    target: PathBuf,
}

#[cfg(unix)]
impl ElapsedClock for SwapToLinkClock {
    fn elapsed_ms(&self) -> u64 {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 4 {
            fs::remove_file(&self.path).unwrap();
            std::os::unix::fs::symlink(&self.target, &self.path).unwrap();
        }
        0
    }
}

impl ElapsedClock for MutatingClock {
    fn elapsed_ms(&self) -> u64 {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 6 {
            fs::write(&self.path, b"%PDF-1.7\nmutated while scanning").unwrap();
        }
        0
    }
}

struct Fixture {
    _root: TempDir,
    repository: PathBuf,
    provider: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("Personal KB");
        let provider = root
            .path()
            .join("Drive/My-Knowledge-OS-Assets/personal/inbox");
        let outside = root.path().join("Downloads");
        fs::create_dir_all(repository.join("assets/registry")).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), KNOWLEDGE_CONFIG).unwrap();
        Self {
            _root: root,
            repository,
            provider,
            outside,
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

    fn outside_pdf(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.outside.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn provider_pdf(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.provider.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}

fn limits() -> ScanLimits {
    DEFAULT_SCAN_LIMITS
}

#[test]
fn provider_scan_recurses_and_excludes_hidden_temp_and_links() {
    let fixture = Fixture::new();
    fixture.provider_pdf("visible/root.pdf", b"%PDF-1.7\nroot");
    fixture.provider_pdf(".hidden.pdf", b"%PDF-1.7\nhidden");
    fixture.provider_pdf("visible/.hidden/nested.pdf", b"%PDF-1.7\nhidden");
    fixture.provider_pdf("visible/paper.pdf.tmp", b"%PDF-1.7\ntemp");
    fixture.provider_pdf("visible/.paper.import.tmp", b"%PDF-1.7\ntemp");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        fixture.outside_pdf("linked.pdf", b"%PDF-1.7\noutside"),
        fixture.provider.join("visible/linked.pdf"),
    )
    .unwrap();

    let result = scan_provider_pdfs(
        ProviderScanRequest::new(&fixture.provider),
        &FixedElapsedClock,
    )
    .unwrap();

    assert!(result.scan_complete);
    assert_eq!(
        result
            .pdfs
            .iter()
            .map(|pdf| pdf.provider_locator.as_str())
            .collect::<Vec<_>>(),
        vec!["visible/root.pdf"]
    );
}

#[test]
fn provider_scan_stops_at_entry_byte_time_and_depth_limits() {
    let fixture = Fixture::new();
    fixture.provider_pdf("a.pdf", b"%PDF-1.7\na");
    fixture.provider_pdf("b.pdf", b"%PDF-1.7\nb");
    fixture.provider_pdf("one/two/deep.pdf", b"%PDF-1.7\ndeep");

    for constrained in [
        ScanLimits {
            max_entries: 1,
            ..limits()
        },
        ScanLimits {
            max_total_bytes: 5,
            ..limits()
        },
        ScanLimits {
            max_depth: 1,
            ..limits()
        },
    ] {
        let result = scan_provider_pdfs(
            ProviderScanRequest::new(&fixture.provider).with_limits(constrained),
            &FixedElapsedClock,
        )
        .unwrap();
        assert!(!result.scan_complete, "{constrained:?}");
        assert!(!result.warnings.is_empty(), "{constrained:?}");
    }

    let result = scan_provider_pdfs(
        ProviderScanRequest::new(&fixture.provider).with_limits(ScanLimits {
            max_elapsed_ms: 1,
            ..limits()
        }),
        &AdvancingClock::new(1),
    )
    .unwrap();
    assert!(!result.scan_complete);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "scan_time_limit")
    );
}

#[test]
fn entry_limit_returns_an_empty_deterministic_partial_without_over_enumeration() {
    let fixture = Fixture::new();
    fixture.provider_pdf("z.pdf", b"%PDF-1.7\nz");
    fixture.provider_pdf("a.pdf", b"%PDF-1.7\na");
    fixture.provider_pdf("m.pdf", b"%PDF-1.7\nm");

    let clock = CountingElapsedClock::default();
    let result = scan_provider_pdfs(
        ProviderScanRequest::new(&fixture.provider).with_limits(ScanLimits {
            max_entries: 1,
            ..limits()
        }),
        &clock,
    )
    .unwrap();

    assert!(!result.scan_complete);
    assert_eq!(result.entries_seen, 1);
    assert!(result.pdfs.is_empty());
    assert!(clock.calls() <= 4, "elapsed checks: {}", clock.calls());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "scan_entry_limit")
    );
}

#[cfg(unix)]
#[test]
fn unreadable_subtree_makes_scan_incomplete_without_losing_readable_items() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.provider_pdf("readable.pdf", b"%PDF-1.7\nreadable");
    fixture.provider_pdf("blocked/secret.pdf", b"%PDF-1.7\nblocked");
    let blocked = fixture.provider.join("blocked");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let result = scan_provider_pdfs(
        ProviderScanRequest::new(&fixture.provider),
        &FixedElapsedClock,
    )
    .unwrap();

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!result.scan_complete);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "scan_subtree_unreadable")
    );
    assert!(
        result
            .pdfs
            .iter()
            .any(|pdf| pdf.provider_locator == "readable.pdf")
    );
}

#[test]
fn provider_scan_rejects_source_mutation_during_fingerprint() {
    let fixture = Fixture::new();
    let path = fixture.provider_pdf("paper.pdf", b"%PDF-1.7\noriginal");
    let clock = MutatingClock {
        calls: AtomicU64::new(0),
        path,
    };

    let error =
        scan_provider_pdfs(ProviderScanRequest::new(&fixture.provider), &clock).unwrap_err();

    assert_eq!(error.code(), "fingerprint_changed");
}

#[test]
fn scanner_revalidates_pdf_after_a_pre_open_mutation() {
    let fixture = Fixture::new();
    let path = fixture.provider_pdf("paper.pdf", b"%PDF-1.7\noriginal");
    let clock = PreOpenMutatingClock {
        calls: AtomicU64::new(0),
        path,
    };

    let error =
        scan_provider_pdfs(ProviderScanRequest::new(&fixture.provider), &clock).unwrap_err();

    assert_eq!(error.code(), "invalid_pdf");
}

#[cfg(unix)]
#[test]
fn scanner_does_not_follow_entry_swapped_to_symlink_before_open() {
    let fixture = Fixture::new();
    let path = fixture.provider_pdf("paper.pdf", b"%PDF-1.7\noriginal");
    let target = fixture.outside_pdf("target.pdf", b"%PDF-1.7\noutside");
    let clock = SwapToLinkClock {
        calls: AtomicU64::new(0),
        path,
        target,
    };

    let result = scan_provider_pdfs(ProviderScanRequest::new(&fixture.provider), &clock).unwrap();

    assert!(!result.scan_complete);
    assert!(result.pdfs.is_empty());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "scan_file_unreadable")
    );
}

#[test]
fn outside_pdf_is_copied_verified_and_original_is_unchanged() {
    let fixture = Fixture::new();
    let original = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncontent");
    let before = fs::read(&original).unwrap();

    let result = add_pdf(
        AddRequest::new(fixture.context(), &original),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.add_outcome, AddOutcome::Created);
    assert_eq!(result.import_outcome, ImportOutcome::Copied);
    assert_eq!(fs::read(original).unwrap(), before);
    assert!(fixture.provider.join(&result.provider_locator).is_file());
}

#[test]
fn inbox_only_copy_requires_verified_backup_before_registration() {
    let fixture = Fixture::new();
    let source = fixture.provider_pdf("Paper.pdf", b"%PDF-1.7\ncontent");

    let error = add_pdf(
        AddRequest::new(fixture.context(), &source),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "backup_confirmation_required");
    assert!(
        fs::read_dir(fixture.repository.join("assets/registry"))
            .unwrap()
            .next()
            .is_none()
    );

    let result = add_pdf(
        AddRequest::new(fixture.context(), &source)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(result.import_outcome, ImportOutcome::AlreadyInInbox);
    assert_eq!(result.add_outcome, AddOutcome::Created);
}

#[test]
fn verified_temporary_source_is_accepted_without_outside_retention_attestation() {
    let fixture = Fixture::new();
    let source = fixture.outside_pdf("Attachment.pdf", b"%PDF-1.7\nattachment");

    let error = add_pdf(
        AddRequest::new(fixture.context(), &source).with_temporary_source(true),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "backup_confirmation_required");

    let result = add_pdf(
        AddRequest::new(fixture.context(), &source)
            .with_temporary_source(true)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(result.import_outcome, ImportOutcome::Copied);
}

#[test]
fn mutation_immediately_before_capture_cannot_register_different_content() {
    let fixture = Fixture::new();
    let source = fixture.provider_pdf("Paper.pdf", b"%PDF-1.7\noriginal");

    let error = add_pdf(
        AddRequest::new(fixture.context(), &source)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &MutatingAuditClock {
            path: source.clone(),
        },
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "fingerprint_changed");
    assert!(
        fs::read_dir(fixture.repository.join("assets/registry"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn invalid_and_oversized_pdfs_fail_before_provider_mutation() {
    let fixture = Fixture::new();
    let invalid = fixture.outside_pdf("invalid.pdf", b"not a pdf");
    let error = add_pdf(
        AddRequest::new(fixture.context(), invalid),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "invalid_pdf");
    assert!(fs::read_dir(&fixture.provider).unwrap().next().is_none());

    let oversized = fixture.outside.join("large.pdf");
    let file = fs::File::create(&oversized).unwrap();
    file.set_len(MAX_ASSET_BYTES + 1).unwrap();
    let error = add_pdf(
        AddRequest::new(fixture.context(), oversized),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "file_too_large");
    assert!(fs::read_dir(&fixture.provider).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn add_rejects_a_source_symlink_without_importing_its_target() {
    let fixture = Fixture::new();
    let target = fixture.outside_pdf("target.pdf", b"%PDF-1.7\ntarget");
    let link = fixture.outside.join("link.pdf");
    std::os::unix::fs::symlink(target, &link).unwrap();

    let error = add_pdf(
        AddRequest::new(fixture.context(), link),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "file_unreadable");
    assert!(fs::read_dir(&fixture.provider).unwrap().next().is_none());
}

#[test]
fn existing_asset_and_unregistered_inbox_duplicate_converge() {
    let fixture = Fixture::new();
    let content = b"%PDF-1.7\nsame content";
    let inbox_copy = fixture.provider_pdf("existing/Inbox Copy.pdf", content);
    let outside = fixture.outside_pdf("Different Name.pdf", content);

    let first = add_pdf(
        AddRequest::new(fixture.context(), &outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(first.import_outcome, ImportOutcome::ReusedInboxCopy);
    assert_eq!(first.provider_locator, "existing/Inbox Copy.pdf");
    assert_eq!(fs::read(inbox_copy).unwrap(), content);

    let second = add_pdf(
        AddRequest::new(fixture.context(), &outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(second.add_outcome, AddOutcome::Existing);
    assert_eq!(second.import_outcome, ImportOutcome::ReusedInboxCopy);
    assert_eq!(second.asset_id, first.asset_id);
}

#[test]
fn existing_registry_missing_persisted_provider_locator_requires_repair() {
    let fixture = Fixture::new();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncontent");
    let first = add_pdf(
        AddRequest::new(fixture.context(), &outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    fs::rename(
        fixture.provider.join(&first.provider_locator),
        fixture.provider.join("Moved.pdf"),
    )
    .unwrap();

    let error = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "registry_provider_missing");
}

#[test]
fn existing_registry_changed_provider_locator_requires_repair() {
    let fixture = Fixture::new();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncontent");
    let first = add_pdf(
        AddRequest::new(fixture.context(), &outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    fixture.provider_pdf("Other.pdf", b"%PDF-1.7\nother");
    let registry = fixture.repository.join(&first.registry_path);
    let contents = fs::read_to_string(&registry).unwrap();
    fs::write(
        &registry,
        contents.replace("locator: Paper.pdf", "locator: Other.pdf"),
    )
    .unwrap();

    let error = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "registry_provider_mismatch");
}

#[test]
fn same_name_with_different_content_gets_hash_suffix_without_overwrite() {
    let fixture = Fixture::new();
    let existing = fixture.provider_pdf("Paper.pdf", b"%PDF-1.7\nexisting");
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\nnew content");
    let hash = fingerprint_file(&outside).unwrap().value;
    let hash = hash.strip_prefix("sha256:").unwrap();

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(fs::read(existing).unwrap(), b"%PDF-1.7\nexisting");
    assert_eq!(
        result.provider_locator,
        format!("Paper-{}.pdf", &hash[..12])
    );
}

#[test]
fn concurrent_identical_adds_converge_without_temp_files() {
    let fixture = Fixture::new();
    let outside = fixture.outside_pdf("Concurrent.pdf", b"%PDF-1.7\nconcurrent");
    let context = fixture.context();
    let handles = (0..2)
        .map(|_| {
            let outside = outside.clone();
            let context = context.clone();
            thread::spawn(move || {
                add_pdf(
                    AddRequest::new(context, outside),
                    &FixedAuditClock,
                    &FixedElapsedClock,
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(results[0].asset_id, results[1].asset_id);
    assert_eq!(results[0].provider_locator, results[1].provider_locator);
    assert_eq!(
        fs::read_dir(&fixture.provider)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .count(),
        0
    );
}

#[test]
fn concurrent_same_name_different_content_never_overwrites() {
    let fixture = Fixture::new();
    let first = fixture.outside_pdf("first/Paper.pdf", b"%PDF-1.7\nfirst");
    let second = fixture.outside_pdf("second/Paper.pdf", b"%PDF-1.7\nsecond");
    let context = fixture.context();
    let handles = [first.clone(), second.clone()]
        .into_iter()
        .map(|source| {
            let context = context.clone();
            thread::spawn(move || {
                add_pdf(
                    AddRequest::new(context, source),
                    &FixedAuditClock,
                    &FixedElapsedClock,
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_ne!(results[0].provider_locator, results[1].provider_locator);
    assert_eq!(fs::read(&first).unwrap(), b"%PDF-1.7\nfirst");
    assert_eq!(fs::read(&second).unwrap(), b"%PDF-1.7\nsecond");
    let imported = results
        .iter()
        .map(|result| fs::read(fixture.provider.join(&result.provider_locator)).unwrap())
        .collect::<Vec<_>>();
    assert!(imported.contains(&b"%PDF-1.7\nfirst".to_vec()));
    assert!(imported.contains(&b"%PDF-1.7\nsecond".to_vec()));
}

#[test]
fn concurrent_case_variant_names_cannot_create_portable_collision() {
    let fixture = Fixture::new();
    let first = fixture.outside_pdf("first/Paper.pdf", b"%PDF-1.7\nfirst");
    let second = fixture.outside_pdf("second/paper.pdf", b"%PDF-1.7\nsecond");
    let context = fixture.context();
    let handles = [first, second]
        .into_iter()
        .map(|source| {
            let context = context.clone();
            thread::spawn(move || {
                add_pdf(
                    AddRequest::new(context, source),
                    &FixedAuditClock,
                    &FixedElapsedClock,
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_ne!(results[0].provider_locator, results[1].provider_locator);
    assert_ne!(
        results[0].provider_locator.to_lowercase(),
        results[1].provider_locator.to_lowercase()
    );
}

#[test]
fn orphaned_import_temp_is_ignored_and_does_not_block_retry() {
    let fixture = Fixture::new();
    let unrelated = fixture.provider.join(".user-notes.import.tmp");
    fs::write(&unrelated, b"user content").unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Paper.pdf");
    assert_eq!(
        fs::read(fixture.provider.join("Paper.pdf")).unwrap(),
        b"%PDF-1.7\ncomplete"
    );
    assert_eq!(fs::read(unrelated).unwrap(), b"user content");
}

#[test]
fn dead_import_lock_and_orphan_temp_are_recovered_on_retry() {
    let fixture = Fixture::new();
    let lock = fixture.provider.join(".mko-import-naming.lock");
    fs::write(&lock, "pid=4294967295\nhost=crashed-host\ntoken=crashed\n").unwrap();
    let orphan = fixture
        .provider
        .join(".mko-import-tmp/crashed/Paper.import.tmp");
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    fs::write(
        fixture.provider.join(".mko-import-tmp/.mko-owned"),
        "mko-import-temp-v1\n",
    )
    .unwrap();
    fs::write(&orphan, b"partial").unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Paper.pdf");
    assert!(lock.is_file());
    assert!(!orphan.exists());
}

#[test]
fn core_owned_temp_root_recovers_a_crash_after_directory_creation() {
    let fixture = Fixture::new();
    fs::write(
        fixture.provider.join(".mko-import-tmp.owner"),
        "mko-import-temp-owner-v1\n",
    )
    .unwrap();
    fs::create_dir(fixture.provider.join(".mko-import-tmp")).unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Paper.pdf");
}

#[test]
fn core_owned_temp_root_recovers_a_partial_marker_publication() {
    let fixture = Fixture::new();
    fs::write(
        fixture.provider.join(".mko-import-tmp.owner"),
        "mko-import-temp-owner-v1\n",
    )
    .unwrap();
    fs::create_dir(fixture.provider.join(".mko-import-tmp")).unwrap();
    fs::write(
        fixture.provider.join(".mko-import-tmp/.mko-owned"),
        "mko-import-",
    )
    .unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Paper.pdf");
}

#[test]
fn unrelated_reserved_name_directory_is_preserved_without_an_owner_claim() {
    let fixture = Fixture::new();
    let user_file = fixture.provider.join(".mko-import-tmp/user-notes.txt");
    fs::create_dir(fixture.provider.join(".mko-import-tmp")).unwrap();
    fs::write(&user_file, b"user content").unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let error = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "provider_import_failed");
    assert_eq!(fs::read(user_file).unwrap(), b"user content");
}

#[cfg(unix)]
#[test]
fn static_import_lock_symlink_is_rejected_without_touching_its_target() {
    let fixture = Fixture::new();
    let target = fixture.outside.join("lock-target");
    fs::write(&target, b"sentinel").unwrap();
    std::os::unix::fs::symlink(&target, fixture.provider.join(".mko-import-naming.lock")).unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let error = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap_err();

    assert_eq!(error.code(), "provider_import_locked");
    assert_eq!(fs::read(target).unwrap(), b"sentinel");
}

#[test]
fn malformed_partial_import_lock_is_recovered_after_bounded_wait() {
    let fixture = Fixture::new();
    let lock = fixture.provider.join(".mko-import-naming.lock");
    fs::write(&lock, b"pid=").unwrap();
    let outside = fixture.outside_pdf("Paper.pdf", b"%PDF-1.7\ncomplete");

    let result = add_pdf(
        AddRequest::new(fixture.context(), outside),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Paper.pdf");
    let record = fs::read_to_string(lock).unwrap();
    assert!(record.contains("token="));
}

#[test]
fn simultaneous_stale_lock_reclaimers_leave_live_owner_intact() {
    let fixture = Fixture::new();
    let lock = fixture.provider.join(".mko-import-naming.lock");
    fs::write(&lock, "pid=4294967295\nhost=crashed-host\ntoken=crashed\n").unwrap();
    let first = fixture.outside_pdf("one/First.pdf", b"%PDF-1.7\nfirst");
    let second = fixture.outside_pdf("two/Second.pdf", b"%PDF-1.7\nsecond");
    let context = fixture.context();
    let handles = [first, second]
        .into_iter()
        .map(|source| {
            let context = context.clone();
            thread::spawn(move || {
                add_pdf(
                    AddRequest::new(context, source),
                    &FixedAuditClock,
                    &FixedElapsedClock,
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(results.len(), 2);
    assert!(lock.is_file());
    assert!(fs::read_to_string(lock).unwrap().contains("token="));
}

#[test]
fn hidden_and_temporary_prefixes_map_to_visible_import_names() {
    for name in [".Hidden.pdf", "~$Temporary.pdf"] {
        let fixture = Fixture::new();
        let outside = fixture.outside_pdf(name, b"%PDF-1.7\ncontent");

        let result = add_pdf(
            AddRequest::new(fixture.context(), outside),
            &FixedAuditClock,
            &FixedElapsedClock,
        )
        .unwrap();
        let scan = scan_provider_pdfs(
            ProviderScanRequest::new(&fixture.provider),
            &FixedElapsedClock,
        )
        .unwrap();

        assert!(!result.provider_locator.starts_with('.'));
        assert!(!result.provider_locator.starts_with("~$"));
        assert!(
            scan.pdfs
                .iter()
                .any(|pdf| pdf.provider_locator == result.provider_locator)
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn decomposed_inbox_name_is_reopened_by_its_exact_physical_path() {
    let fixture = Fixture::new();
    let source = fixture.provider_pdf("Cafe\u{301}.pdf", b"%PDF-1.7\ncontent");

    let result = add_pdf(
        AddRequest::new(fixture.context(), source)
            .with_backup_attestation(BackupAttestation::UserVerified),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();

    assert_eq!(result.provider_locator, "Caf\u{e9}.pdf");
}

#[test]
fn import_suffixes_case_and_nfc_portable_name_collisions() {
    let fixture = Fixture::new();
    fixture.provider_pdf("Paper.pdf", b"%PDF-1.7\na");
    let case_source = fixture.outside_pdf("paper.pdf", b"%PDF-1.7\nb");
    let case_hash = fingerprint_file(&case_source).unwrap().value;
    let case_result = add_pdf(
        AddRequest::new(fixture.context(), case_source),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(
        case_result.provider_locator,
        format!("paper-{}.pdf", &case_hash[7..19])
    );

    fixture.provider_pdf("Caf\u{e9}.pdf", b"%PDF-1.7\nc");
    let nfc_source = fixture.outside_pdf("Cafe\u{301}.pdf", b"%PDF-1.7\nd");
    let nfc_hash = fingerprint_file(&nfc_source).unwrap().value;
    let nfc_result = add_pdf(
        AddRequest::new(fixture.context(), nfc_source),
        &FixedAuditClock,
        &FixedElapsedClock,
    )
    .unwrap();
    assert_eq!(
        nfc_result.provider_locator,
        format!("Caf\u{e9}-{}.pdf", &nfc_hash[7..19])
    );
}

#[test]
fn public_defaults_remain_fixed_and_bounded() {
    assert_eq!(
        DEFAULT_SCAN_LIMITS,
        ScanLimits {
            max_entries: 4096,
            max_total_bytes: 1_073_741_824,
            max_elapsed_ms: 5_000,
            max_depth: 32,
            max_batch_items: 20,
        }
    );
}
