use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Instant,
};

use cap_std::{
    ambient_authority,
    fs::{Dir, DirEntry, File, OpenOptions, OpenOptionsExt},
};
use unicode_normalization::UnicodeNormalization;

use crate::{
    error::MkoError,
    fingerprint::{fingerprint_open_file, validate_pdf_content},
    model::Fingerprint,
    path_policy::{canonical_directory, validate_portable_relative_path},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_elapsed_ms: u64,
    pub max_depth: u64,
    /// Downstream Task 9 projection ceiling. The scanner still fingerprints every
    /// PDF within the safety bounds so duplicate detection remains complete.
    pub max_batch_items: u64,
}

pub const DEFAULT_SCAN_LIMITS: ScanLimits = ScanLimits {
    max_entries: 4096,
    max_total_bytes: 1_073_741_824,
    max_elapsed_ms: 5_000,
    max_depth: 32,
    max_batch_items: 20,
};

pub trait ElapsedClock: Send + Sync {
    fn elapsed_ms(&self) -> u64;
}

#[derive(Clone, Debug)]
pub struct MonotonicElapsedClock {
    started_at: Instant,
}

impl MonotonicElapsedClock {
    pub fn start() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl ElapsedClock for MonotonicElapsedClock {
    fn elapsed_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Debug)]
pub struct ProviderScanRequest {
    provider_root: PathBuf,
    limits: ScanLimits,
}

impl ProviderScanRequest {
    pub fn new(provider_root: impl AsRef<Path>) -> Self {
        Self {
            provider_root: provider_root.as_ref().to_path_buf(),
            limits: DEFAULT_SCAN_LIMITS,
        }
    }

    pub fn with_limits(mut self, limits: ScanLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> ScanLimits {
        self.limits
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderPdf {
    pub provider_locator: String,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub fingerprint: Fingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderScanWarning {
    pub code: String,
    pub message: String,
    pub provider_locator: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderScanResult {
    pub scan_complete: bool,
    pub pdfs: Vec<ProviderPdf>,
    pub warnings: Vec<ProviderScanWarning>,
    pub entries_seen: u64,
    pub total_pdf_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderMetadataEntry {
    pub relative_path: PathBuf,
    pub platform_attributes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderMetadataIssue {
    pub relative_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderMetadataWalk {
    pub entries: Vec<ProviderMetadataEntry>,
    pub issues: Vec<ProviderMetadataIssue>,
}

pub(crate) fn inspect_provider_metadata(
    provider_root: &Path,
    limits: ScanLimits,
) -> Result<ProviderMetadataWalk, MkoError> {
    inspect_provider_metadata_with_observer(provider_root, limits, &mut |_| {})
}

fn inspect_provider_metadata_with_observer(
    provider_root: &Path,
    limits: ScanLimits,
    observer: &mut dyn FnMut(&Path),
) -> Result<ProviderMetadataWalk, MkoError> {
    validate_limits(limits)?;
    let root_metadata = std::fs::symlink_metadata(provider_root).map_err(|error| {
        MkoError::new(
            "provider_inspection_failed",
            format!("cannot inspect provider root: {error}"),
        )
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "provider_root_invalid",
            "provider root must be a non-link directory",
        ));
    }
    let root = open_root_directory_nofollow(provider_root).map_err(|error| {
        MkoError::new(
            "provider_inspection_failed",
            format!("cannot open provider root: {error}"),
        )
    })?;
    let mut state = MetadataWalkState {
        limits,
        started_at: Instant::now(),
        entries_seen: 0,
        total_pdf_bytes: 0,
        stopped: false,
        walk: ProviderMetadataWalk::default(),
        observer,
    };
    walk_provider_metadata(&root, Path::new(""), 0, &mut state);
    state
        .walk
        .entries
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(state.walk)
}

struct MetadataWalkState<'a> {
    limits: ScanLimits,
    started_at: Instant,
    entries_seen: u64,
    total_pdf_bytes: u64,
    stopped: bool,
    walk: ProviderMetadataWalk,
    observer: &'a mut dyn FnMut(&Path),
}

fn walk_provider_metadata(
    directory: &Dir,
    relative_directory: &Path,
    depth: u64,
    state: &mut MetadataWalkState<'_>,
) {
    if metadata_limit_reached(state) {
        return;
    }
    let mut read_dir = match directory.entries() {
        Ok(entries) => entries,
        Err(error) => {
            metadata_issue(
                state,
                Some(relative_directory.to_path_buf()),
                format!("cannot read provider subtree: {error}"),
            );
            return;
        }
    };
    let remaining = state.limits.max_entries.saturating_sub(state.entries_seen);
    if remaining == 0 {
        state.stopped = true;
        metadata_issue(
            state,
            None,
            "provider inspection reached the entry limit".into(),
        );
        return;
    }
    let mut entries = Vec::new();
    let mut inspected = 0;
    let mut exhausted = false;
    while inspected < remaining {
        if metadata_limit_reached(state) {
            state.entries_seen = state.entries_seen.saturating_add(inspected);
            return;
        }
        let Some(entry) = read_dir.next() else {
            exhausted = true;
            break;
        };
        inspected += 1;
        match entry {
            Ok(entry) => entries.push(entry),
            Err(error) => metadata_issue(
                state,
                Some(relative_directory.to_path_buf()),
                format!("cannot enumerate provider entry: {error}"),
            ),
        }
    }
    state.entries_seen = state.entries_seen.saturating_add(inspected);
    if !exhausted {
        state.stopped = true;
        metadata_issue(
            state,
            None,
            "provider inspection reached the entry limit before deterministic ordering was established"
                .into(),
        );
        return;
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if metadata_limit_reached(state) {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            metadata_issue(
                state,
                Some(relative_directory.to_path_buf()),
                "provider entry name is not valid UTF-8".into(),
            );
            continue;
        };
        if excluded_name(name) {
            continue;
        }
        let relative = relative_directory.join(name);
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                metadata_issue(
                    state,
                    Some(relative),
                    format!("cannot inspect provider entry type: {error}"),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if depth >= state.limits.max_depth {
                metadata_issue(
                    state,
                    Some(relative),
                    "provider inspection reached the depth limit".into(),
                );
                continue;
            }
            (state.observer)(&relative);
            match open_directory_nofollow(&entry) {
                Ok(child) => walk_provider_metadata(&child, &relative, depth + 1, state),
                Err(error) => metadata_issue(
                    state,
                    Some(relative),
                    format!("cannot open provider subtree without following links: {error}"),
                ),
            }
            continue;
        }
        if !file_type.is_file() || !has_pdf_extension(name) {
            continue;
        }
        (state.observer)(&relative);
        let metadata = match directory.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) => {
                metadata_issue(
                    state,
                    Some(relative),
                    format!("cannot inspect PDF metadata: {error}"),
                );
                continue;
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            metadata_issue(
                state,
                Some(relative),
                "PDF candidate changed to a non-file or link".into(),
            );
            continue;
        }
        let next_total = match state.total_pdf_bytes.checked_add(metadata.len()) {
            Some(total) => total,
            None => {
                state.stopped = true;
                metadata_issue(state, None, "provider PDF byte count overflowed".into());
                break;
            }
        };
        if next_total > state.limits.max_total_bytes {
            state.stopped = true;
            metadata_issue(
                state,
                None,
                "provider inspection reached the aggregate PDF byte limit".into(),
            );
            break;
        }
        state.total_pdf_bytes = next_total;
        match platform_attributes(directory, name, &metadata) {
            Ok(platform_attributes) => state.walk.entries.push(ProviderMetadataEntry {
                relative_path: relative,
                platform_attributes,
            }),
            Err(message) => metadata_issue(state, Some(relative), message),
        }
    }
}

fn metadata_limit_reached(state: &mut MetadataWalkState<'_>) -> bool {
    if state.stopped {
        return true;
    }
    if state.started_at.elapsed().as_millis() < u128::from(state.limits.max_elapsed_ms) {
        return false;
    }
    state.stopped = true;
    metadata_issue(
        state,
        None,
        "provider inspection reached the time limit".into(),
    );
    true
}

fn metadata_issue(
    state: &mut MetadataWalkState<'_>,
    relative_path: Option<PathBuf>,
    message: String,
) {
    state.walk.issues.push(ProviderMetadataIssue {
        relative_path,
        message,
    });
}

fn open_root_directory_nofollow(path: &Path) -> std::io::Result<Dir> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, true);
    let file = File::open_ambient_with(path, &options, ambient_authority())?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("root is not a non-link directory"));
    }
    Ok(Dir::from_std_file(file.into_std()))
}

#[cfg(target_os = "macos")]
fn platform_attributes(
    directory: &Dir,
    name: &str,
    _: &cap_std::fs::Metadata,
) -> Result<u32, String> {
    use std::os::fd::AsFd;

    use nix::{fcntl::AtFlags, sys::stat::fstatat};

    fstatat(directory.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map(|metadata| metadata.st_flags)
        .map_err(|error| format!("cannot inspect PDF platform metadata: {error}"))
}

#[cfg(windows)]
fn platform_attributes(_: &Dir, _: &str, metadata: &cap_std::fs::Metadata) -> Result<u32, String> {
    use cap_std::fs::MetadataExt;

    Ok(metadata.file_attributes())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_attributes(_: &Dir, _: &str, _: &cap_std::fs::Metadata) -> Result<u32, String> {
    Ok(0)
}

pub fn scan_provider_pdfs(
    request: ProviderScanRequest,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<ProviderScanResult, MkoError> {
    validate_limits(request.limits)?;
    let provider_root = canonical_directory(&request.provider_root, "provider_root_invalid")?;
    let root = Dir::open_ambient_dir(&provider_root, ambient_authority()).map_err(|error| {
        MkoError::new(
            "provider_scan_failed",
            format!("cannot open provider root: {error}"),
        )
    })?;
    let started_ms = elapsed_clock.elapsed_ms();
    let mut state = ScanState {
        limits: request.limits,
        started_ms,
        elapsed_clock,
        scan_complete: true,
        stopped: false,
        pdfs: Vec::new(),
        warnings: Vec::new(),
        entries_seen: 0,
        total_pdf_bytes: 0,
    };
    walk_directory(&root, Path::new(""), 0, &mut state)?;
    state
        .pdfs
        .sort_by(|left, right| left.provider_locator.cmp(&right.provider_locator));
    Ok(ProviderScanResult {
        scan_complete: state.scan_complete,
        pdfs: state.pdfs,
        warnings: state.warnings,
        entries_seen: state.entries_seen,
        total_pdf_bytes: state.total_pdf_bytes,
    })
}

struct ScanState<'a> {
    limits: ScanLimits,
    started_ms: u64,
    elapsed_clock: &'a dyn ElapsedClock,
    scan_complete: bool,
    stopped: bool,
    pdfs: Vec<ProviderPdf>,
    warnings: Vec<ProviderScanWarning>,
    entries_seen: u64,
    total_pdf_bytes: u64,
}

fn walk_directory(
    directory: &Dir,
    relative_directory: &Path,
    depth: u64,
    state: &mut ScanState<'_>,
) -> Result<(), MkoError> {
    if state.stopped || time_limit_reached(state) {
        return Ok(());
    }
    let mut read_dir = match directory.entries() {
        Ok(entries) => entries,
        Err(error) => {
            mark_incomplete(
                state,
                "scan_subtree_unreadable",
                format!("cannot read provider subtree: {error}"),
                locator_for_directory(relative_directory),
            );
            return Ok(());
        }
    };
    let remaining = state.limits.max_entries.saturating_sub(state.entries_seen);
    if remaining == 0 {
        mark_limit(
            state,
            "scan_entry_limit",
            "provider scan reached the entry limit",
        );
        return Ok(());
    }
    let mut entries = Vec::new();
    let mut inspected = 0;
    let mut exhausted = false;
    while inspected < remaining {
        if time_limit_reached(state) {
            state.entries_seen = state.entries_seen.saturating_add(inspected);
            return Ok(());
        }
        let Some(entry) = read_dir.next() else {
            exhausted = true;
            break;
        };
        inspected += 1;
        match entry {
            Ok(entry) => entries.push(entry),
            Err(error) => mark_incomplete(
                state,
                "scan_entry_unreadable",
                format!("cannot enumerate provider entry: {error}"),
                locator_for_directory(relative_directory),
            ),
        }
    }
    state.entries_seen = state.entries_seen.saturating_add(inspected);
    if !exhausted {
        mark_incomplete(
            state,
            "scan_entry_limit",
            "provider scan reached the entry limit before deterministic ordering was established"
                .into(),
            None,
        );
        return Ok(());
    }
    entries.sort_by_key(|entry| entry.file_name());
    reject_directory_collisions(&entries)?;

    for entry in entries {
        if state.stopped || time_limit_reached(state) {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            mark_incomplete(
                state,
                "scan_path_invalid",
                "provider entry name is not valid UTF-8".into(),
                locator_for_directory(relative_directory),
            );
            continue;
        };
        if excluded_name(name) {
            continue;
        }
        let relative = relative_directory.join(name);
        let locator = logical_locator(&relative)?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                mark_incomplete(
                    state,
                    "scan_entry_unreadable",
                    format!("cannot inspect provider entry: {error}"),
                    Some(locator),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if depth >= state.limits.max_depth {
                mark_incomplete(
                    state,
                    "scan_depth_limit",
                    "provider scan reached the depth limit".into(),
                    Some(locator),
                );
                continue;
            }
            let _ = state.elapsed_clock.elapsed_ms();
            match open_directory_nofollow(&entry) {
                Ok(child) => walk_directory(&child, &relative, depth + 1, state)?,
                Err(error) => mark_incomplete(
                    state,
                    "scan_subtree_unreadable",
                    format!("cannot open provider subtree: {error}"),
                    Some(locator),
                ),
            }
            continue;
        }
        if !file_type.is_file() || !has_pdf_extension(name) {
            continue;
        }
        let _ = state.elapsed_clock.elapsed_ms();
        let mut file = match open_file_nofollow(&entry) {
            Ok(file) => file,
            Err(error) => {
                mark_incomplete(
                    state,
                    "scan_file_unreadable",
                    format!("cannot open PDF candidate: {error}"),
                    Some(locator),
                );
                continue;
            }
        };
        let metadata = file.metadata().map_err(|error| {
            MkoError::new(
                "file_unreadable",
                format!("cannot inspect opened PDF: {error}"),
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let before = fingerprint_open_file(&mut file)?;
        validate_pdf_content(&mut file)?;
        let _ = state.elapsed_clock.elapsed_ms();
        let after = fingerprint_open_file(&mut file)?;
        validate_pdf_content(&mut file)?;
        if before != after {
            return Err(MkoError::new(
                "fingerprint_changed",
                "PDF changed while the provider was being scanned",
            ));
        }
        let next_total = state
            .total_pdf_bytes
            .checked_add(before.size_bytes)
            .ok_or_else(|| {
                MkoError::new("scan_byte_limit", "provider PDF byte count overflowed")
            })?;
        if next_total > state.limits.max_total_bytes {
            mark_limit(
                state,
                "scan_byte_limit",
                "provider scan reached the aggregate PDF byte limit",
            );
            break;
        }
        state.total_pdf_bytes = next_total;
        state.pdfs.push(ProviderPdf {
            provider_locator: locator,
            relative_path: relative,
            size_bytes: before.size_bytes,
            fingerprint: before.fingerprint,
        });
    }
    Ok(())
}

fn validate_limits(limits: ScanLimits) -> Result<(), MkoError> {
    if limits.max_entries == 0
        || limits.max_total_bytes == 0
        || limits.max_elapsed_ms == 0
        || limits.max_batch_items == 0
    {
        return Err(MkoError::new(
            "scan_limits_invalid",
            "provider scan limits must be positive",
        ));
    }
    Ok(())
}

fn time_limit_reached(state: &mut ScanState<'_>) -> bool {
    let elapsed = state
        .elapsed_clock
        .elapsed_ms()
        .saturating_sub(state.started_ms);
    if elapsed < state.limits.max_elapsed_ms {
        return false;
    }
    mark_limit(
        state,
        "scan_time_limit",
        "provider scan reached the time limit",
    );
    true
}

fn mark_limit(state: &mut ScanState<'_>, code: &str, message: &str) {
    state.stopped = true;
    mark_incomplete(state, code, message.into(), None);
}

fn mark_incomplete(
    state: &mut ScanState<'_>,
    code: &str,
    message: String,
    provider_locator: Option<String>,
) {
    state.scan_complete = false;
    if !state
        .warnings
        .iter()
        .any(|warning| warning.code == code && warning.provider_locator == provider_locator)
    {
        state.warnings.push(ProviderScanWarning {
            code: code.into(),
            message,
            provider_locator,
        });
    }
}

fn reject_directory_collisions(entries: &[cap_std::fs::DirEntry]) -> Result<(), MkoError> {
    let mut names = HashMap::new();
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            MkoError::new("invalid_path", "provider filename must be valid UTF-8")
        })?;
        if excluded_name(name) {
            continue;
        }
        let key = collision_key(name);
        if names.insert(key, name.to_owned()).is_some() {
            return Err(MkoError::new(
                "path_collision",
                "provider contains a case or Unicode-normalization filename collision",
            ));
        }
    }
    Ok(())
}

fn logical_locator(relative: &Path) -> Result<String, MkoError> {
    let locator = relative
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(|value| value.nfc().collect::<String>())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| MkoError::new("invalid_path", "provider path must be valid UTF-8"))?
        .join("/");
    validate_portable_relative_path(&locator)?;
    Ok(locator)
}

fn locator_for_directory(relative: &Path) -> Option<String> {
    if relative.as_os_str().is_empty() {
        None
    } else {
        logical_locator(relative).ok()
    }
}

pub(crate) fn excluded_name(name: &str) -> bool {
    let lowercase = name.to_ascii_lowercase();
    name.starts_with('.')
        || name.starts_with("~$")
        || name.ends_with('~')
        || lowercase.ends_with(".tmp")
        || lowercase.ends_with(".part")
        || lowercase.ends_with(".partial")
        || lowercase.ends_with(".crdownload")
}

fn has_pdf_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn collision_key(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

fn open_file_nofollow(entry: &DirEntry) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    entry.open_with(&options)
}

fn open_directory_nofollow(entry: &DirEntry) -> std::io::Result<Dir> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, true);
    let file = entry.open_with(&options)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("entry is not a non-link directory"));
    }
    Ok(Dir::from_std_file(file.into_std()))
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_DIRECTORY: i32 = 0x10_000;
    options.custom_flags(O_NOFOLLOW | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NOFOLLOW: i32 = 0x100;
    const O_DIRECTORY: i32 = 0x10_0000;
    options.custom_flags(O_NOFOLLOW | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    options.custom_flags(
        FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            },
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions, _directory: bool) {}

#[cfg(all(test, unix))]
mod metadata_walk_tests {
    use std::{fs, os::unix::fs::symlink, path::Path};

    use super::{DEFAULT_SCAN_LIMITS, ScanLimits, inspect_provider_metadata_with_observer};

    #[test]
    fn metadata_walk_does_not_follow_a_directory_swapped_to_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(&child).unwrap();
        fs::write(child.join("inside.pdf"), b"inside").unwrap();
        fs::write(outside.path().join("outside.pdf"), b"outside").unwrap();
        let mut swapped = false;

        let walk = inspect_provider_metadata_with_observer(
            root.path(),
            DEFAULT_SCAN_LIMITS,
            &mut |relative: &Path| {
                if relative == Path::new("child") && !swapped {
                    fs::rename(&child, root.path().join("child-original")).unwrap();
                    symlink(outside.path(), &child).unwrap();
                    swapped = true;
                }
            },
        )
        .unwrap();

        assert!(swapped);
        assert!(
            walk.entries
                .iter()
                .all(|entry| entry.relative_path != Path::new("child/outside.pdf"))
        );
        assert!(
            walk.issues
                .iter()
                .any(|issue| issue.relative_path.as_deref() == Some(Path::new("child")))
        );
    }

    #[test]
    fn metadata_walk_enforces_the_shared_entry_bound_before_collecting_a_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.pdf"), b"a").unwrap();
        fs::write(root.path().join("b.pdf"), b"b").unwrap();
        let limits = ScanLimits {
            max_entries: 1,
            ..DEFAULT_SCAN_LIMITS
        };

        let walk =
            inspect_provider_metadata_with_observer(root.path(), limits, &mut |_| {}).unwrap();

        assert!(walk.entries.is_empty());
        assert!(
            walk.issues
                .iter()
                .any(|issue| issue.message.contains("entry limit"))
        );
    }
}
