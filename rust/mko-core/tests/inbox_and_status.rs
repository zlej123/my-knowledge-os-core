use std::{
    collections::BTreeMap,
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{TimeZone, Utc};

use mko_core::{
    catalog::{CatalogBlocker, CatalogEvidence, SourceObservation, classify_catalog_item},
    fingerprint::{asset_id, fingerprint_file},
    front_matter::render_markdown,
    inbox::{InboxScanRequest, project_catalog_items, scan_inbox},
    json_v1::{
        DiagnosticData, InboxData, JsonV1Success, NextAction, ScanLimitsData, StatusData,
        SuccessResult, UserState,
    },
    model::{AssetRecord, AssetStatus, Classification, LastError, LastSuccessfulStep, Provider},
    provider_scan::{DEFAULT_SCAN_LIMITS, ElapsedClock, ScanLimits},
    status::status_from_inbox,
};
use tempfile::TempDir;

#[test]
fn every_catalog_state_has_one_deterministic_user_action() {
    let cases = [
        (
            None,
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::New,
            NextAction::Add,
        ),
        (
            Some(AssetStatus::Registered),
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::Registered,
            NextAction::Prepare,
        ),
        (
            Some(AssetStatus::Extracted),
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::Incomplete,
            NextAction::WriteDraft,
        ),
        (
            Some(AssetStatus::ReviewPending),
            SourceObservation::ReviewPending,
            CatalogBlocker::None,
            UserState::ReviewPending,
            NextAction::Review,
        ),
        (
            Some(AssetStatus::Processed),
            SourceObservation::Approved,
            CatalogBlocker::None,
            UserState::Processed,
            NextAction::None,
        ),
        (
            Some(AssetStatus::Changed),
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::Blocked,
            NextAction::Repair,
        ),
        (
            Some(AssetStatus::Missing),
            SourceObservation::Absent,
            CatalogBlocker::ProviderMissing,
            UserState::Blocked,
            NextAction::Hydrate,
        ),
        (
            Some(AssetStatus::Failed),
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::Blocked,
            NextAction::Retry,
        ),
        (
            Some(AssetStatus::Superseded),
            SourceObservation::Absent,
            CatalogBlocker::None,
            UserState::Blocked,
            NextAction::Repair,
        ),
        (
            Some(AssetStatus::Registered),
            SourceObservation::Absent,
            CatalogBlocker::ActiveLock,
            UserState::Blocked,
            NextAction::Retry,
        ),
        (
            Some(AssetStatus::ReviewPending),
            SourceObservation::Invalid,
            CatalogBlocker::StateMismatch,
            UserState::Blocked,
            NextAction::Repair,
        ),
        (
            Some(AssetStatus::Processed),
            SourceObservation::ReviewPending,
            CatalogBlocker::StateMismatch,
            UserState::Blocked,
            NextAction::Repair,
        ),
    ];

    for (asset_status, source, blocker, expected_state, expected_action) in cases {
        let item = classify_catalog_item(
            "inbox/paper.pdf",
            asset_status.as_ref().map(|_| "asset-004".to_owned()),
            CatalogEvidence {
                asset_status,
                source,
                blocker,
            },
        );
        assert_eq!(item.user_state, expected_state);
        assert_eq!(item.next_action, expected_action);
    }
}

#[test]
fn default_scan_limits_are_shared_and_fixed() {
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

#[test]
fn inbox_projection_is_bounded_and_reports_visible_remaining_count() {
    let fixture = Fixture::new();
    for index in 0..23 {
        fs::write(
            fixture.provider.join(format!("paper-{index:02}.pdf")),
            format!("%PDF-1.7\n{index}\n%%EOF\n"),
        )
        .unwrap();
    }

    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert!(!result.scan_complete);
    assert_eq!(result.items.len(), 20);
    assert_eq!(result.remaining, 3);
    assert!(result.warnings.iter().any(|warning| {
        warning.code == "actionable_limit_reached"
            && warning.message == "20 actionable inbox items shown; 3 remaining."
    }));
}

#[test]
fn inbox_scan_does_not_create_repository_state() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let provider = root.path().join("provider");
    fs::create_dir(&repository).unwrap();
    fs::create_dir(&provider).unwrap();
    fs::write(provider.join("paper.pdf"), b"%PDF-1.7\nread only\n%%EOF\n").unwrap();

    let before = fs::read_dir(&repository).unwrap().count();
    let result = scan_inbox(
        InboxScanRequest::new(&repository, &provider),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert_eq!(result.items[0].user_state, UserState::New);
    assert_eq!(fs::read_dir(&repository).unwrap().count(), before);
    assert!(!repository.join("assets").exists());
    assert!(!repository.join("sources").exists());
    assert!(!repository.join(".knowledge-os").exists());
}

#[test]
fn incomplete_provider_scan_does_not_invent_missing_or_hydrate_state() {
    let fixture = Fixture::new();
    let file = fixture.provider.join("known.pdf");
    fs::write(&file, b"%PDF-1.7\nknown\n%%EOF\n").unwrap();
    write_asset_record(
        &fixture.repository,
        valid_asset(&file, "known.pdf", AssetStatus::Registered),
    );
    fs::write(
        fixture.provider.join("other.pdf"),
        b"%PDF-1.7\nother\n%%EOF\n",
    )
    .unwrap();

    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider).with_limits(ScanLimits {
            max_entries: 1,
            ..DEFAULT_SCAN_LIMITS
        }),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert!(!result.scan_complete);
    assert!(
        result
            .items
            .iter()
            .all(|item| item.next_action != NextAction::Hydrate)
    );
    assert_eq!(result.state_counts[&UserState::Blocked], 0);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "scan_limit_reached")
    );
}

#[test]
fn superseded_history_sharing_a_locator_is_hidden_in_favor_of_current_successor() {
    let fixture = Fixture::new();
    let file = fixture.provider.join("paper.pdf");
    fs::write(&file, b"%PDF-1.7\ncurrent\n%%EOF\n").unwrap();
    let successor = valid_asset(&file, "paper.pdf", AssetStatus::Registered);
    let old_file = fixture._root.path().join("old.pdf");
    fs::write(&old_file, b"%PDF-1.7\nold\n%%EOF\n").unwrap();
    let mut old = valid_asset(&old_file, "paper.pdf", AssetStatus::Superseded);
    old.durable_state_history = vec![AssetStatus::Registered];
    let mut successor = successor;
    successor.supersedes = Some(old.id.clone());
    write_asset_record(&fixture.repository, old);
    write_asset_record(&fixture.repository, successor.clone());

    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].asset_id.as_deref(),
        Some(successor.id.as_str())
    );
    assert_eq!(result.items[0].user_state, UserState::Registered);
    assert_eq!(result.state_counts[&UserState::Blocked], 0);
}

#[test]
fn lock_scan_ignores_noise_but_fails_closed_when_sorted_lock_records_are_truncated() {
    let fixture = Fixture::new();
    let file = fixture.provider.join("paper.pdf");
    fs::write(&file, b"%PDF-1.7\nlock\n%%EOF\n").unwrap();
    let asset = valid_asset(&file, "paper.pdf", AssetStatus::Registered);
    write_asset_record(&fixture.repository, asset.clone());
    let locks = fixture.repository.join(".knowledge-os/runtime/locks");
    fs::create_dir_all(&locks).unwrap();
    fs::write(locks.join("000-noise.txt"), b"noise").unwrap();
    fs::write(locks.join(format!("{}.lock", asset.id)), b"{}").unwrap();
    fs::write(locks.join("zzz.lock.takeover"), b"{}").unwrap();
    fs::write(locks.join("zzz2.lock"), b"{}").unwrap();

    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider).with_limits(ScanLimits {
            max_entries: 2,
            ..DEFAULT_SCAN_LIMITS
        }),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert!(!result.scan_complete);
    assert_eq!(result.items[0].next_action, NextAction::Retry);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.code == "lock_scan_incomplete")
    );
}

#[test]
fn invalid_registry_record_is_repair_blocked_without_exposing_unsafe_locator() {
    let fixture = Fixture::new();
    let outside = fixture._root.path().join("outside.pdf");
    fs::write(&outside, b"%PDF-1.7\ninvalid registry\n%%EOF\n").unwrap();
    let mut asset = valid_asset(&outside, "../escape.pdf", AssetStatus::Registered);
    asset.scope = "work".into();
    write_asset_record(&fixture.repository, asset);

    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider),
        &FixedElapsedClock::new(0),
    )
    .unwrap();

    assert!(
        result
            .items
            .iter()
            .all(|item| !item.provider_locator.contains(".."))
    );
    assert_eq!(result.state_counts[&UserState::Blocked], 1);
    assert_eq!(result.recommended_action, NextAction::Repair);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.code == "registry_invalid")
    );
}

#[test]
fn projection_prioritizes_actionable_items_before_processed_history() {
    let mut items = (0..25)
        .map(|index| mko_core::catalog::CatalogItem {
            provider_locator: format!("processed-{index:02}.pdf"),
            user_state: UserState::Processed,
            asset_id: Some(format!("processed-{index:02}")),
            next_action: NextAction::None,
            diagnostic: None,
        })
        .collect::<Vec<_>>();
    items.push(item_at("z-new.pdf", UserState::New, NextAction::Add));
    items.push(item_at(
        "z-review.pdf",
        UserState::ReviewPending,
        NextAction::Review,
    ));

    let projection = project_catalog_items(items, 20);

    assert!(
        projection
            .items
            .iter()
            .any(|item| item.provider_locator == "z-new.pdf")
    );
    assert!(
        projection
            .items
            .iter()
            .any(|item| item.provider_locator == "z-review.pdf")
    );
    assert_eq!(projection.items.len(), 20);
    assert_eq!(projection.remaining, 7);
}

#[test]
fn mixed_blockers_use_one_priority_for_primary_diagnostic_and_action() {
    let cases = [
        ("lock_active", NextAction::Retry),
        ("provider_missing", NextAction::Hydrate),
        ("registry_provider_mismatch", NextAction::Repair),
    ];
    let mut items = Vec::new();
    for (index, (code, action)) in cases.into_iter().enumerate() {
        let mut catalog = item_at(&format!("{index}.pdf"), UserState::Blocked, action);
        catalog.diagnostic = Some(DiagnosticData {
            code: code.into(),
            message: code.into(),
            path: Some(format!("{index}.pdf")),
        });
        items.push(catalog);
    }
    let inbox = inbox_result_for_status(
        items,
        vec![DiagnosticData {
            code: "repository_unreadable".into(),
            message: "repo".into(),
            path: Some("repo".into()),
        }],
    );

    let status = status_from_inbox(&inbox);

    assert_eq!(status.next_action, NextAction::Repair);
    assert_eq!(
        status.primary_blocker.unwrap().code,
        "repository_unreadable"
    );
}

#[test]
fn every_fixed_provider_limit_returns_a_stable_incomplete_warning() {
    let entry_fixture = Fixture::new();
    fs::write(
        entry_fixture.provider.join("a.pdf"),
        b"%PDF-1.7\na\n%%EOF\n",
    )
    .unwrap();
    fs::write(
        entry_fixture.provider.join("b.pdf"),
        b"%PDF-1.7\nb\n%%EOF\n",
    )
    .unwrap();
    assert_limit_warning(
        &entry_fixture,
        ScanLimits {
            max_entries: 1,
            ..DEFAULT_SCAN_LIMITS
        },
        &FixedElapsedClock::new(0),
        "The inbox scan reached its entry limit.",
    );

    let byte_fixture = Fixture::new();
    fs::write(
        byte_fixture.provider.join("paper.pdf"),
        b"%PDF-1.7\nbytes\n%%EOF\n",
    )
    .unwrap();
    assert_limit_warning(
        &byte_fixture,
        ScanLimits {
            max_total_bytes: 1,
            ..DEFAULT_SCAN_LIMITS
        },
        &FixedElapsedClock::new(0),
        "The inbox scan reached its aggregate byte limit.",
    );

    let depth_fixture = Fixture::new();
    fs::create_dir(depth_fixture.provider.join("nested")).unwrap();
    fs::write(
        depth_fixture.provider.join("nested/paper.pdf"),
        b"%PDF-1.7\ndepth\n%%EOF\n",
    )
    .unwrap();
    assert_limit_warning(
        &depth_fixture,
        ScanLimits {
            max_depth: 0,
            ..DEFAULT_SCAN_LIMITS
        },
        &FixedElapsedClock::new(0),
        "The inbox scan reached its depth limit.",
    );

    let time_fixture = Fixture::new();
    fs::write(
        time_fixture.provider.join("paper.pdf"),
        b"%PDF-1.7\ntime\n%%EOF\n",
    )
    .unwrap();
    assert_limit_warning(
        &time_fixture,
        ScanLimits {
            max_elapsed_ms: 1,
            ..DEFAULT_SCAN_LIMITS
        },
        &IncrementingElapsedClock::default(),
        "The inbox scan reached its time limit.",
    );
}

#[test]
fn status_counts_all_states_and_uses_stable_action_priority() {
    let items = vec![
        item(UserState::New, NextAction::Add),
        item(UserState::Registered, NextAction::Prepare),
        item(UserState::Incomplete, NextAction::WriteDraft),
        item(UserState::ReviewPending, NextAction::Review),
        item(UserState::Processed, NextAction::None),
    ];
    let inbox = mko_core::inbox::InboxScanResult {
        scan_complete: true,
        scan_limits: DEFAULT_SCAN_LIMITS,
        items,
        errors: Vec::new(),
        warnings: Vec::new(),
        remaining: 0,
        state_counts: BTreeMap::from([
            (UserState::New, 1),
            (UserState::Registered, 1),
            (UserState::Incomplete, 1),
            (UserState::ReviewPending, 1),
            (UserState::Processed, 1),
            (UserState::Blocked, 0),
        ]),
        primary_blocker: None,
        recommended_action: NextAction::Review,
    };
    let status = status_from_inbox(&inbox);

    assert!(status.healthy);
    assert_eq!(status.counts[&UserState::New], 1);
    assert_eq!(status.counts[&UserState::Registered], 1);
    assert_eq!(status.counts[&UserState::Incomplete], 1);
    assert_eq!(status.counts[&UserState::ReviewPending], 1);
    assert_eq!(status.counts[&UserState::Processed], 1);
    assert_eq!(status.counts[&UserState::Blocked], 0);
    assert_eq!(status.next_action, NextAction::Review);
}

#[test]
fn blocked_status_prefers_repair_and_exposes_primary_blocker() {
    let mut blocked = item(UserState::Blocked, NextAction::Repair);
    blocked.diagnostic = Some(DiagnosticData {
        code: "repository_unreadable".into(),
        message: "The repository is not readable.".into(),
        path: Some("/knowledge".into()),
    });
    let inbox = mko_core::inbox::InboxScanResult {
        scan_complete: true,
        scan_limits: DEFAULT_SCAN_LIMITS,
        items: vec![blocked],
        errors: Vec::new(),
        warnings: Vec::new(),
        remaining: 0,
        state_counts: BTreeMap::from([
            (UserState::New, 0),
            (UserState::Registered, 0),
            (UserState::Incomplete, 0),
            (UserState::ReviewPending, 0),
            (UserState::Processed, 0),
            (UserState::Blocked, 1),
        ]),
        primary_blocker: Some(DiagnosticData {
            code: "repository_unreadable".into(),
            message: "The repository is not readable.".into(),
            path: Some("/knowledge".into()),
        }),
        recommended_action: NextAction::Repair,
    };
    let status = status_from_inbox(&inbox);

    assert!(!status.healthy);
    assert_eq!(status.next_action, NextAction::Repair);
    assert_eq!(
        status.primary_blocker.unwrap().code,
        "repository_unreadable"
    );
}

#[test]
fn frozen_inbox_and_status_payloads_remain_exact() {
    let inbox = JsonV1Success::Inbox {
        schema_version: 1,
        result: SuccessResult::Ok,
        data: InboxData {
            scan_complete: true,
            scan_limits: ScanLimitsData {
                max_entries: 1000,
                max_total_bytes: 1_073_741_824,
                max_elapsed_ms: 5_000,
                max_depth: 4,
                max_batch_items: 20,
            },
            items: vec![mko_core::json_v1::InboxItemData {
                provider_locator: "inbox/paper.pdf".into(),
                user_state: UserState::New,
                asset_id: None,
                next_action: NextAction::Add,
            }],
            errors: Vec::new(),
            warnings: Vec::new(),
        },
    };
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/inbox-success.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(inbox).unwrap(), expected);

    let status = JsonV1Success::Status {
        schema_version: 1,
        result: SuccessResult::Ok,
        data: StatusData {
            healthy: true,
            counts: BTreeMap::from([
                (UserState::New, 1),
                (UserState::Registered, 2),
                (UserState::Incomplete, 0),
                (UserState::ReviewPending, 3),
                (UserState::Processed, 8),
                (UserState::Blocked, 0),
            ]),
            primary_blocker: None,
            next_action: NextAction::Review,
        },
    };
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/status-success.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(status).unwrap(), expected);

    let incomplete: JsonV1Success = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/inbox-incomplete.json"
    ))
    .unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/inbox-incomplete.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(incomplete).unwrap(), expected);

    let blocked: JsonV1Success = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/status-blocked.json"
    ))
    .unwrap();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/status-blocked.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(blocked).unwrap(), expected);
}

fn assert_limit_warning(
    fixture: &Fixture,
    limits: ScanLimits,
    clock: &dyn ElapsedClock,
    message: &str,
) {
    let result = scan_inbox(
        InboxScanRequest::new(&fixture.repository, &fixture.provider).with_limits(limits),
        clock,
    )
    .unwrap();
    assert!(!result.scan_complete);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| { warning.code == "scan_limit_reached" && warning.message == message }),
        "{:?}",
        result.warnings
    );
}

fn item(user_state: UserState, next_action: NextAction) -> mko_core::catalog::CatalogItem {
    mko_core::catalog::CatalogItem {
        provider_locator: "inbox/paper.pdf".into(),
        user_state,
        asset_id: None,
        next_action,
        diagnostic: None,
    }
}

fn item_at(
    provider_locator: &str,
    user_state: UserState,
    next_action: NextAction,
) -> mko_core::catalog::CatalogItem {
    mko_core::catalog::CatalogItem {
        provider_locator: provider_locator.into(),
        user_state,
        asset_id: None,
        next_action,
        diagnostic: None,
    }
}

fn inbox_result_for_status(
    items: Vec<mko_core::catalog::CatalogItem>,
    errors: Vec<DiagnosticData>,
) -> mko_core::inbox::InboxScanResult {
    let mut counts = [
        UserState::New,
        UserState::Registered,
        UserState::Incomplete,
        UserState::ReviewPending,
        UserState::Processed,
        UserState::Blocked,
    ]
    .into_iter()
    .map(|state| (state, 0))
    .collect::<BTreeMap<_, _>>();
    for item in &items {
        *counts.entry(item.user_state.clone()).or_default() += 1;
    }
    mko_core::inbox::InboxScanResult {
        scan_complete: true,
        scan_limits: DEFAULT_SCAN_LIMITS,
        items,
        errors,
        warnings: Vec::new(),
        remaining: 0,
        state_counts: counts,
        primary_blocker: None,
        recommended_action: NextAction::None,
    }
}

fn valid_asset(file: &std::path::Path, locator: &str, status: AssetStatus) -> AssetRecord {
    let fingerprint = fingerprint_file(file).unwrap();
    let id = asset_id(&fingerprint).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap();
    AssetRecord {
        id,
        record_type: "asset".into(),
        schema_version: 1,
        scope: "personal".into(),
        title: "Paper".into(),
        classification: Classification::Personal,
        asset_class: "document".into(),
        media_type: "application/pdf".into(),
        provider: Provider {
            r#type: "google-drive-stream".into(),
            locator: locator.into(),
            revision: None,
        },
        size_bytes: fs::metadata(file).unwrap().len(),
        modified_at: now,
        fingerprint,
        asset_status: status,
        durable_state_history: Vec::new(),
        supersedes: None,
        last_successful_step: LastSuccessfulStep::Registered,
        last_error: LastError {
            code: None,
            retryable: false,
        },
        created_at: now,
        updated_at: now,
    }
}

fn write_asset_record(repository: &std::path::Path, asset: AssetRecord) {
    let path = repository
        .join("assets/registry")
        .join(format!("{}.md", asset.id));
    fs::write(path, render_markdown(&asset, "# Asset Registry\n").unwrap()).unwrap();
}

struct Fixture {
    _root: TempDir,
    repository: std::path::PathBuf,
    provider: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        fs::create_dir_all(repository.join("assets/registry")).unwrap();
        fs::create_dir_all(&provider).unwrap();
        Self {
            _root: root,
            repository,
            provider,
        }
    }
}

struct FixedElapsedClock(AtomicU64);

impl FixedElapsedClock {
    fn new(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }
}

impl ElapsedClock for FixedElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct IncrementingElapsedClock(AtomicU64);

impl ElapsedClock for IncrementingElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}
