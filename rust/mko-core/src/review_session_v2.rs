use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    clock::Clock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    model_v2::ReviewTargetTypeV2,
    queue_v2::{ReviewCardTargetStateV2, show_review_card_v2},
    review_v2::{
        NonTtyReviewDecisionV2, NonTtyReviewRequestV2, NonTtyReviewTargetV2, ReviewPublicationV2,
        ReviewTargetSnapshotV2, publish_non_tty_review_locked_v2,
    },
    revision_v2::canonical_json_bytes,
};

const SESSION_TTL: Duration = Duration::minutes(15);
const MAX_SESSION_BYTES: u64 = 256 * 1024;
const MAX_OPEN_SESSIONS: usize = 256;
const SESSION_SCAN_DEADLINE: StdDuration = StdDuration::from_millis(100);
const SESSION_PREFIX: &str = "mko-review-session-";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSessionApprovalModeV2 {
    Tty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSessionTargetV2 {
    pub record_type: ReviewTargetTypeV2,
    pub record_id: String,
    pub displayed_revision: String,
    pub review_head_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOpenDataV2 {
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
    pub card_markdown: String,
    pub card_digest: String,
    pub effect_digest: String,
    pub targets: Vec<ReviewSessionTargetV2>,
    pub approval_mode: ReviewSessionApprovalModeV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSessionTargetDecisionV2 {
    pub record_id: String,
    pub decision: NonTtyReviewDecisionV2,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSessionDecisionInputV2 {
    pub session_id: String,
    pub card_digest: String,
    pub target_decisions: Vec<ReviewSessionTargetDecisionV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredReviewSessionV2 {
    schema_version: u32,
    session_id: String,
    item_id: String,
    opened_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    card_digest: String,
    effect_digest: String,
    targets: Vec<ReviewTargetSnapshotV2>,
}

struct SessionDirectories {
    open: PathBuf,
    consumed: PathBuf,
}

pub fn open_review_session_v2(
    repository_root: &Path,
    stable_id: &str,
    clock: &dyn Clock,
) -> Result<ReviewOpenDataV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let card = show_review_card_v2(repository_root, stable_id)?;
    if card.targets.iter().all(|target| {
        matches!(
            target.state,
            ReviewCardTargetStateV2::Approved | ReviewCardTargetStateV2::Blocked
        )
    }) {
        return Err(MkoError::new(
            "review_session_not_actionable",
            "the exact review card has no target eligible for feedback or defer",
        ));
    }
    let card_markdown = String::from_utf8(card.card_bytes.clone()).map_err(|error| {
        MkoError::new(
            "review_card_invalid",
            format!("canonical review card is not UTF-8: {error}"),
        )
    })?;
    let directories = ensure_session_directories(repository_root)?;
    enforce_open_session_bound(&directories.open)?;
    let session_id = new_session_id()?;
    let opened_at = clock.now_utc();
    let expires_at = opened_at + SESSION_TTL;
    let targets = card
        .targets
        .iter()
        .map(|target| target.snapshot.clone())
        .collect::<Vec<_>>();
    let stored = StoredReviewSessionV2 {
        schema_version: 2,
        session_id: session_id.clone(),
        item_id: card.item_id,
        opened_at,
        expires_at,
        card_digest: card.card_digest.clone(),
        effect_digest: card.effect_digest.clone(),
        targets: targets.clone(),
    };
    let bytes = canonical_json_bytes(&stored)?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err(MkoError::new(
            "review_session_too_large",
            "the exact review session exceeds its bounded local representation",
        ));
    }
    let path = directories.open.join(format!("{session_id}.json"));
    write_private_new(&path, &bytes)?;

    Ok(ReviewOpenDataV2 {
        session_id,
        expires_at,
        single_use: true,
        card_markdown,
        card_digest: card.card_digest,
        effect_digest: card.effect_digest,
        targets: targets
            .into_iter()
            .map(|target| ReviewSessionTargetV2 {
                record_type: target.record_type,
                record_id: target.record_id,
                displayed_revision: target.displayed_revision,
                review_head_id: target.expected_review_head_id,
            })
            .collect(),
        approval_mode: ReviewSessionApprovalModeV2::Tty,
    })
}

pub fn apply_review_session_decision_v2(
    repository_root: &Path,
    input: ReviewSessionDecisionInputV2,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    validate_session_id(&input.session_id)?;
    validate_digest(&input.card_digest, "review_card_digest_invalid")?;
    validate_decision_input(&input)?;

    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 review session apply",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    KnowledgeConfigV2::read(repository_root)?;
    let directories = ensure_session_directories(repository_root)?;
    let filename = format!("{}.json", input.session_id);
    let consumed_path = directories.consumed.join(&filename);
    if path_exists_nofollow(&consumed_path)? {
        require_private_regular_file(&consumed_path)?;
        return Err(consumed_error());
    }
    let open_path = directories.open.join(&filename);
    let stored = read_session(&open_path)?;
    if stored.session_id != input.session_id || stored.schema_version != 2 {
        return Err(MkoError::new(
            "review_session_invalid",
            "the machine-local review session identity is invalid",
        ));
    }
    if clock.now_utc() >= stored.expires_at {
        return Err(MkoError::new(
            "review_session_expired",
            "the review session expired; display the current card again",
        ));
    }
    if input.card_digest != stored.card_digest {
        return Err(stale_error());
    }

    let current = show_review_card_v2(repository_root, &stored.item_id)?;
    let current_targets = current
        .targets
        .iter()
        .map(|target| target.snapshot.clone())
        .collect::<Vec<_>>();
    if current.card_digest != stored.card_digest
        || current.effect_digest != stored.effect_digest
        || current_targets != stored.targets
    {
        return Err(stale_error());
    }

    let snapshots = stored
        .targets
        .iter()
        .map(|target| (target.record_id.as_str(), target))
        .collect::<HashMap<_, _>>();
    let request = NonTtyReviewRequestV2 {
        targets: input
            .target_decisions
            .into_iter()
            .map(|decision| {
                let snapshot = snapshots
                    .get(decision.record_id.as_str())
                    .ok_or_else(stale_error)?;
                Ok(NonTtyReviewTargetV2 {
                    record_type: snapshot.record_type.clone(),
                    record_id: snapshot.record_id.clone(),
                    displayed_revision: snapshot.displayed_revision.clone(),
                    expected_review_head_id: snapshot.expected_review_head_id.clone(),
                    decision: decision.decision,
                    feedback: decision.feedback,
                })
            })
            .collect::<Result<Vec<_>, MkoError>>()?,
    };
    let publication = publish_non_tty_review_locked_v2(repository_root, request, clock)?;
    consume_session(&open_path, &consumed_path)?;
    Ok(publication)
}

fn validate_decision_input(input: &ReviewSessionDecisionInputV2) -> Result<(), MkoError> {
    if input.target_decisions.is_empty() || input.target_decisions.len() > 2 {
        return Err(MkoError::new(
            "review_session_decision_invalid",
            "a review session decision must select one or two displayed targets",
        ));
    }
    let mut ids = HashSet::new();
    for target in &input.target_decisions {
        if !ids.insert(target.record_id.as_str()) {
            return Err(MkoError::new(
                "review_session_decision_invalid",
                "a review session decision cannot repeat a target",
            ));
        }
    }
    Ok(())
}

fn ensure_session_directories(repository_root: &Path) -> Result<SessionDirectories, MkoError> {
    require_real_directory(repository_root)?;
    let mko = repository_root.join(".mko");
    require_real_directory(&mko)?;
    let runtime = ensure_private_directory(&mko.join("runtime"))?;
    let sessions = ensure_private_directory(&runtime.join("review-sessions"))?;
    let open = ensure_private_directory(&sessions.join("open"))?;
    let consumed = ensure_private_directory(&sessions.join("consumed"))?;
    Ok(SessionDirectories { open, consumed })
}

fn enforce_open_session_bound(directory: &Path) -> Result<(), MkoError> {
    let deadline = Instant::now() + SESSION_SCAN_DEADLINE;
    let entries = fs::read_dir(directory)
        .map_err(|error| MkoError::new("review_session_scan_failed", error.to_string()))?;
    for (index, entry) in entries.enumerate() {
        if index + 1 >= MAX_OPEN_SESSIONS || Instant::now() >= deadline {
            return Err(MkoError::new(
                "review_session_scan_limit",
                "too many open review sessions; consume or expire existing sessions before retrying",
            ));
        }
        let entry = entry
            .map_err(|error| MkoError::new("review_session_scan_failed", error.to_string()))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| MkoError::new("review_session_scan_failed", error.to_string()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(MkoError::new(
                "review_session_invalid",
                "the review session directory contains a link or non-file entry",
            ));
        }
    }
    Ok(())
}

fn new_session_id() -> Result<String, MkoError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| {
        MkoError::new(
            "review_session_random_failed",
            "secure randomness is unavailable for a review session capability",
        )
    })?;
    Ok(format!("{SESSION_PREFIX}{}", hex::encode(random)))
}

fn validate_session_id(id: &str) -> Result<(), MkoError> {
    let suffix = id.strip_prefix(SESSION_PREFIX).ok_or_else(|| {
        MkoError::new(
            "review_session_id_invalid",
            "review session ID is not a Core-issued capability",
        )
    })?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(MkoError::new(
            "review_session_id_invalid",
            "review session ID is not a Core-issued capability",
        ));
    }
    Ok(())
}

fn validate_digest(value: &str, code: &str) -> Result<(), MkoError> {
    let hash = value
        .strip_prefix("sha256:")
        .ok_or_else(|| MkoError::new(code, "digest must use sha256:<64 lowercase hex>"))?;
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(MkoError::new(
            code,
            "digest must use sha256:<64 lowercase hex>",
        ))
    }
}

fn read_session(path: &Path) -> Result<StoredReviewSessionV2, MkoError> {
    let bytes = read_private_regular(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("review_session_invalid", error.to_string()))
}

fn read_private_regular(path: &Path) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MkoError::new(
                "review_session_not_found",
                "the machine-local review session does not exist on this device",
            )
        } else {
            MkoError::new("review_session_invalid", error.to_string())
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("review_session_invalid", error.to_string()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SESSION_BYTES
    {
        return Err(MkoError::new(
            "review_session_invalid",
            "review session must be a bounded regular non-link file",
        ));
    }
    ensure_private_file_permissions(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SESSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("review_session_invalid", error.to_string()))?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err(MkoError::new(
            "review_session_invalid",
            "review session exceeds its bounded input size",
        ));
    }
    Ok(bytes)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_create(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| MkoError::new("review_session_write_failed", error.to_string()))?;
    protect_private_file(&file)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| MkoError::new("review_session_write_failed", error.to_string()))?;
    ensure_private_file_permissions(path)
}

fn consume_session(open: &Path, consumed: &Path) -> Result<(), MkoError> {
    if path_exists_nofollow(consumed)? {
        return Err(consumed_error());
    }
    require_private_regular_file(open)?;
    fs::rename(open, consumed)
        .map_err(|error| MkoError::new("review_session_consume_failed", error.to_string()))?;
    require_private_regular_file(consumed)?;
    sync_parent_directory(consumed)
}

fn path_exists_nofollow(path: &Path) -> Result<bool, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("review_session_invalid", error.to_string())),
    }
}

fn require_private_regular_file(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("review_session_invalid", error.to_string()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SESSION_BYTES
    {
        return Err(MkoError::new(
            "review_session_invalid",
            "review session must be a bounded regular non-link file",
        ));
    }
    ensure_private_file_permissions(path)
}

fn require_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("review_session_invalid", error.to_string()))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            "review_session_invalid",
            "review session path must contain only real directories",
        ))
    }
}

fn ensure_private_directory(path: &Path) -> Result<PathBuf, MkoError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(MkoError::new(
                "review_session_write_failed",
                error.to_string(),
            ));
        }
    }
    require_real_directory(path)?;
    protect_private_directory(path)?;
    ensure_private_directory_permissions(path)?;
    Ok(path.to_path_buf())
}

fn consumed_error() -> MkoError {
    MkoError::new(
        "review_session_consumed",
        "the single-use review session was already consumed",
    )
}

fn stale_error() -> MkoError {
    MkoError::new(
        "review_snapshot_stale",
        "the displayed review card is no longer the exact current snapshot",
    )
}

#[cfg(unix)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_path(
        path,
        mko_windows_acl::Inheritance::ContainersAndObjects,
    )
    .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_directory(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "review_session_permissions_unsupported",
        "owner-only review session storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "review_session_permissions_invalid",
            "review session directories must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_directory_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "review_session_permissions_unsupported",
        "owner-only review session storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn protect_private_file(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_file(file: &fs::File) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_file(file)
        .map(|_| ())
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_file(_file: &fs::File) -> Result<(), MkoError> {
    Err(MkoError::new(
        "review_session_permissions_unsupported",
        "owner-only review session storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "review_session_permissions_invalid",
            "review session files must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_file_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "review_session_permissions_unsupported",
        "owner-only review session storage is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn validate_windows_acl(path: &Path) -> Result<(), MkoError> {
    const FULL_CONTROL_MASK: u32 = 0x001f_01ff;
    let inspection = mko_windows_acl::inspect_path(path)
        .map_err(|error| MkoError::new("review_session_permissions_invalid", error.to_string()))?;
    if inspection.owner_is_current_user
        && inspection.dacl_is_protected
        && inspection.entries.len() == 1
        && inspection.entries[0].allows_current_user
        && inspection.entries[0].access_mask == FULL_CONTROL_MASK
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "review_session_permissions_invalid",
            "review session ACL must grant full control only to the current user",
        ))
    }
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x20_000 | 0x800);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x100 | 0x4);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x0020_0000);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_private_create(options: &mut OpenOptions) {
    options.mode(0o600);
    configure_nofollow(options);
}

#[cfg(windows)]
fn configure_private_create(options: &mut OpenOptions) {
    configure_nofollow(options);
}

#[cfg(not(any(unix, windows)))]
fn configure_private_create(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), MkoError> {
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            "review_session_consume_failed",
            "review session destination has no parent",
        )
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MkoError::new("review_session_consume_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_parent_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}
