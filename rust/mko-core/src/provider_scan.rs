use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use cap_std::{
    ambient_authority,
    fs::{Dir, DirEntry, File, Metadata, OpenOptions, OpenOptionsExt},
};
use unicode_normalization::UnicodeNormalization;

use crate::{
    error::MkoError,
    fingerprint::{fingerprint_open_file, fingerprint_open_file_with_guard, validate_pdf_content},
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

#[derive(Clone, Copy)]
pub(crate) struct ScanDeadline<'a> {
    elapsed_clock: &'a dyn ElapsedClock,
    started_ms: u64,
    max_elapsed_ms: u64,
}

impl<'a> ScanDeadline<'a> {
    pub(crate) fn start(elapsed_clock: &'a dyn ElapsedClock, limits: ScanLimits) -> Self {
        Self {
            elapsed_clock,
            started_ms: elapsed_clock.elapsed_ms(),
            max_elapsed_ms: limits.max_elapsed_ms,
        }
    }

    pub(crate) fn check(&self) -> Result<(), MkoError> {
        if self
            .elapsed_clock
            .elapsed_ms()
            .saturating_sub(self.started_ms)
            < self.max_elapsed_ms
        {
            Ok(())
        } else {
            Err(MkoError::new(
                "scan_time_limit",
                "provider scan reached the time limit",
            ))
        }
    }
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

#[derive(Debug)]
pub(crate) struct ProviderMetadataEntry {
    pub relative_path: PathBuf,
    hydration: ProviderHydrationDisposition,
    size_bytes: u64,
    retained_file: Option<File>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderHydrationDisposition {
    Placeholder,
    Supported,
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    Unsupported,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EnumeratedFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
type EnumeratedFileIdentity = mko_windows_acl::FileIdentity;

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EnumeratedFileIdentity;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderMetadataAccess<T> {
    Placeholder,
    Supported(T),
    Unsupported(T),
}

impl ProviderMetadataEntry {
    pub(crate) fn inspect_access<T>(
        &self,
        inspect: impl FnOnce() -> T,
    ) -> ProviderMetadataAccess<T> {
        match self.hydration {
            ProviderHydrationDisposition::Placeholder => ProviderMetadataAccess::Placeholder,
            ProviderHydrationDisposition::Supported => ProviderMetadataAccess::Supported(inspect()),
            ProviderHydrationDisposition::Unsupported => {
                ProviderMetadataAccess::Unsupported(inspect())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderMetadataIssue {
    pub relative_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Default)]
pub(crate) struct ProviderMetadataWalk {
    pub entries: Vec<ProviderMetadataEntry>,
    pub issues: Vec<ProviderMetadataIssue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderCatalogEntry {
    Placeholder {
        provider_locator: String,
        relative_path: PathBuf,
    },
    Readable(ProviderPdf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderCatalogScan {
    pub scan_complete: bool,
    pub mutation_safe: bool,
    pub entries: Vec<ProviderCatalogEntry>,
    pub warnings: Vec<ProviderScanWarning>,
}

pub(crate) fn scan_provider_catalog_metadata_first_with_deadline(
    request: ProviderScanRequest,
    deadline: &ScanDeadline<'_>,
) -> Result<ProviderCatalogScan, MkoError> {
    scan_provider_catalog_metadata_first_inner(request, deadline, || {}, &mut |_| {})
}

#[cfg(test)]
fn scan_provider_catalog_metadata_first_with_after_walk(
    request: ProviderScanRequest,
    elapsed_clock: &dyn ElapsedClock,
    after_walk: impl FnOnce(),
) -> Result<ProviderCatalogScan, MkoError> {
    let deadline = ScanDeadline::start(elapsed_clock, request.limits);
    scan_provider_catalog_metadata_first_inner(request, &deadline, after_walk, &mut |_| {})
}

#[cfg(test)]
fn scan_provider_catalog_metadata_first_with_before_file_open(
    request: ProviderScanRequest,
    elapsed_clock: &dyn ElapsedClock,
    before_file_open: &mut dyn FnMut(&Path),
) -> Result<ProviderCatalogScan, MkoError> {
    let deadline = ScanDeadline::start(elapsed_clock, request.limits);
    scan_provider_catalog_metadata_first_inner(request, &deadline, || {}, before_file_open)
}

fn scan_provider_catalog_metadata_first_inner(
    request: ProviderScanRequest,
    deadline: &ScanDeadline<'_>,
    after_walk: impl FnOnce(),
    before_file_open: &mut dyn FnMut(&Path),
) -> Result<ProviderCatalogScan, MkoError> {
    validate_limits(request.limits)?;
    let provider_root = canonical_directory(&request.provider_root, "provider_root_invalid")?;
    let walk = inspect_provider_metadata_with_deadline(
        &provider_root,
        request.limits,
        deadline,
        &mut |_| {},
        before_file_open,
    )?;
    after_walk();
    let mut warnings = walk
        .issues
        .iter()
        .map(metadata_issue_warning)
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    for entry in walk.entries {
        if let Err(error) = deadline.check() {
            warnings.push(ProviderScanWarning {
                code: error.code().into(),
                message: error.message().into(),
                provider_locator: None,
            });
            break;
        }
        let relative_path = entry.relative_path.clone();
        match materialize_metadata_entry(entry, |relative_path, file| {
            fingerprint_retained_pdf(relative_path, file, deadline)
        }) {
            ProviderMetadataAccess::Placeholder => {
                entries.push(ProviderCatalogEntry::Placeholder {
                    provider_locator: logical_locator(&relative_path)?,
                    relative_path,
                });
            }
            ProviderMetadataAccess::Supported(result)
            | ProviderMetadataAccess::Unsupported(result) => match result {
                Ok(pdf) => entries.push(ProviderCatalogEntry::Readable(pdf)),
                Err(error) => {
                    let timed_out = error.code() == "scan_time_limit";
                    warnings.push(ProviderScanWarning {
                        code: error.code().into(),
                        message: error.message().into(),
                        provider_locator: logical_locator(&relative_path).ok(),
                    });
                    if timed_out {
                        break;
                    }
                }
            },
        }
    }
    entries.sort_by(|left, right| catalog_locator(left).cmp(catalog_locator(right)));
    warnings.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then(left.provider_locator.cmp(&right.provider_locator))
    });
    let mutation_safe = warnings
        .iter()
        .all(|warning| matches!(warning.code.as_str(), "invalid_pdf" | "pdf_too_large"));
    Ok(ProviderCatalogScan {
        scan_complete: warnings.is_empty(),
        mutation_safe,
        entries,
        warnings,
    })
}

fn catalog_locator(entry: &ProviderCatalogEntry) -> &str {
    match entry {
        ProviderCatalogEntry::Placeholder {
            provider_locator, ..
        } => provider_locator,
        ProviderCatalogEntry::Readable(pdf) => &pdf.provider_locator,
    }
}

fn materialize_metadata_entry<T>(
    mut entry: ProviderMetadataEntry,
    inspect_content: impl FnOnce(&Path, &mut File) -> Result<T, MkoError>,
) -> ProviderMetadataAccess<Result<T, MkoError>> {
    let relative_path = entry.relative_path.clone();
    match entry.hydration {
        ProviderHydrationDisposition::Placeholder => ProviderMetadataAccess::Placeholder,
        ProviderHydrationDisposition::Supported | ProviderHydrationDisposition::Unsupported => {
            let inspected = entry.retained_file.as_mut().map_or_else(
                || {
                    Err(MkoError::new(
                        "scan_file_unreadable",
                        "readable metadata entry has no retained file handle",
                    ))
                },
                |file| inspect_content(&relative_path, file),
            );
            match entry.hydration {
                ProviderHydrationDisposition::Supported => {
                    ProviderMetadataAccess::Supported(inspected)
                }
                ProviderHydrationDisposition::Unsupported => {
                    ProviderMetadataAccess::Unsupported(inspected)
                }
                ProviderHydrationDisposition::Placeholder => unreachable!(),
            }
        }
    }
}

fn fingerprint_retained_pdf(
    relative_path: &Path,
    file: &mut File,
    deadline: &ScanDeadline<'_>,
) -> Result<ProviderPdf, MkoError> {
    let locator = logical_locator(relative_path)?;
    let mut check_deadline = || deadline.check();
    let before = fingerprint_open_file_with_guard(file, &mut check_deadline)?;
    deadline.check()?;
    validate_pdf_content(file)?;
    deadline.check()?;
    let after = fingerprint_open_file_with_guard(file, &mut check_deadline)?;
    deadline.check()?;
    validate_pdf_content(file)?;
    deadline.check()?;
    if before != after {
        return Err(MkoError::new(
            "fingerprint_changed",
            "PDF changed while the provider was being scanned",
        ));
    }
    Ok(ProviderPdf {
        provider_locator: locator,
        relative_path: relative_path.to_path_buf(),
        size_bytes: before.size_bytes,
        fingerprint: before.fingerprint,
    })
}

fn metadata_issue_warning(issue: &ProviderMetadataIssue) -> ProviderScanWarning {
    let code = if issue.message.contains("time limit") {
        "scan_time_limit"
    } else if issue.message.contains("entry limit") {
        "scan_entry_limit"
    } else if issue.message.contains("aggregate PDF byte limit") {
        "scan_byte_limit"
    } else if issue.message.contains("depth limit") {
        "scan_depth_limit"
    } else if issue.message.contains("enumerated PDF candidate") {
        "scan_file_unreadable"
    } else {
        "provider_inspection_failed"
    };
    ProviderScanWarning {
        code: code.into(),
        message: issue.message.clone(),
        provider_locator: issue
            .relative_path
            .as_deref()
            .and_then(|path| logical_locator(path).ok()),
    }
}

pub(crate) fn inspect_provider_metadata(
    provider_root: &Path,
    limits: ScanLimits,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<ProviderMetadataWalk, MkoError> {
    inspect_provider_metadata_with_observer(provider_root, limits, elapsed_clock, &mut |_| {})
}

fn inspect_provider_metadata_with_observer(
    provider_root: &Path,
    limits: ScanLimits,
    elapsed_clock: &dyn ElapsedClock,
    observer: &mut dyn FnMut(&Path),
) -> Result<ProviderMetadataWalk, MkoError> {
    validate_limits(limits)?;
    let deadline = ScanDeadline::start(elapsed_clock, limits);
    inspect_provider_metadata_with_deadline(provider_root, limits, &deadline, observer, &mut |_| {})
}

fn inspect_provider_metadata_with_deadline(
    provider_root: &Path,
    limits: ScanLimits,
    deadline: &ScanDeadline<'_>,
    observer: &mut dyn FnMut(&Path),
    before_file_open: &mut dyn FnMut(&Path),
) -> Result<ProviderMetadataWalk, MkoError> {
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
    let root = Arc::new(
        open_root_directory_nofollow(provider_root).map_err(|error| {
            MkoError::new(
                "provider_inspection_failed",
                format!("cannot open provider root: {error}"),
            )
        })?,
    );
    let mut state = MetadataWalkState {
        limits,
        deadline,
        entries_seen: 0,
        total_pdf_bytes: 0,
        stopped: false,
        walk: ProviderMetadataWalk::default(),
        observer,
        before_file_open,
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
    deadline: &'a ScanDeadline<'a>,
    entries_seen: u64,
    total_pdf_bytes: u64,
    stopped: bool,
    walk: ProviderMetadataWalk,
    observer: &'a mut dyn FnMut(&Path),
    before_file_open: &'a mut dyn FnMut(&Path),
}

fn walk_provider_metadata(
    directory: &Arc<Dir>,
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
                Ok(child) => walk_provider_metadata(&Arc::new(child), &relative, depth + 1, state),
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
        let metadata_entry =
            match inspect_enumerated_pdf(&entry, directory, name, relative.clone(), || {
                (state.before_file_open)(&relative)
            }) {
                Ok(metadata_entry) => metadata_entry,
                Err(message) => {
                    metadata_issue(state, Some(relative), message);
                    continue;
                }
            };
        let next_total = match state.total_pdf_bytes.checked_add(metadata_entry.size_bytes) {
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
        state.walk.entries.push(metadata_entry);
    }
}

fn metadata_limit_reached(state: &mut MetadataWalkState<'_>) -> bool {
    if state.stopped {
        return true;
    }
    if state.deadline.check().is_ok() {
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

#[cfg(windows)]
fn inspect_enumerated_pdf(
    entry: &DirEntry,
    directory: &Arc<Dir>,
    name: &str,
    relative_path: PathBuf,
    before_open: impl FnOnce(),
) -> Result<ProviderMetadataEntry, String> {
    use cap_std::fs::MetadataExt;

    // On Windows this reads the attributes retained by directory enumeration.
    // Do not replace it with a path-based metadata call: RECALL_ON_OPEN must be
    // classified before any operation that can acquire a new file handle.
    let enumerated = entry
        .metadata()
        .map_err(|error| format!("cannot inspect enumerated PDF metadata: {error}"))?;
    let mut inspected = inspect_windows_enumerated_pdf(
        relative_path,
        enumerated.file_attributes(),
        enumerated.len(),
        || inspect_nofollow_pdf_metadata(directory, name),
    )?;
    if inspected.hydration != ProviderHydrationDisposition::Placeholder {
        let retained_file = open_windows_enumerated_file(entry)?;
        let retained_metadata = retained_file
            .metadata()
            .map_err(|error| format!("cannot inspect retained PDF candidate: {error}"))?;
        let identity = windows_file_identity(&retained_file)?;
        inspected.size_bytes = retained_metadata.len();
        before_open();
        verify_windows_file_still_current(directory, name, identity, inspected.size_bytes)?;
        inspected.retained_file = Some(retained_file);
    }
    Ok(inspected)
}

#[cfg(not(windows))]
fn inspect_enumerated_pdf(
    entry: &DirEntry,
    directory: &Arc<Dir>,
    name: &str,
    relative_path: PathBuf,
    before_open: impl FnOnce(),
) -> Result<ProviderMetadataEntry, String> {
    let enumerated = inspect_nofollow_pdf_metadata_snapshot(directory, name)?;
    let size_bytes = enumerated.len();
    let identity = enumerated_file_identity(&enumerated)?;
    let hydration = non_windows_hydration_disposition(directory, name)?;
    let retained_file = if hydration == ProviderHydrationDisposition::Placeholder {
        None
    } else {
        before_open();
        verify_enumerated_file_still_current(directory, name, identity, size_bytes)?;
        Some(open_enumerated_file(entry, identity, size_bytes)?)
    };
    Ok(ProviderMetadataEntry {
        relative_path,
        hydration,
        size_bytes,
        retained_file,
    })
}

#[cfg(not(windows))]
fn open_enumerated_file(
    entry: &DirEntry,
    expected_identity: EnumeratedFileIdentity,
    expected_size: u64,
) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    let file = entry
        .open_with(&options)
        .map_err(|error| format!("cannot open enumerated PDF candidate: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect enumerated PDF candidate: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("enumerated PDF candidate changed to a non-file or link".into());
    }
    if enumerated_file_identity(&metadata)? != expected_identity {
        return Err("enumerated PDF candidate identity changed before content access".into());
    }
    if metadata.len() != expected_size {
        return Err("enumerated PDF candidate changed size before its handle was retained".into());
    }
    Ok(file)
}

#[cfg(windows)]
fn inspect_nofollow_pdf_metadata(directory: &Dir, name: &str) -> Result<u64, String> {
    inspect_nofollow_pdf_metadata_snapshot(directory, name).map(|metadata| metadata.len())
}

fn inspect_nofollow_pdf_metadata_snapshot(directory: &Dir, name: &str) -> Result<Metadata, String> {
    let metadata = directory
        .symlink_metadata(name)
        .map_err(|error| format!("cannot inspect PDF metadata: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("PDF candidate changed to a non-file or link".into());
    }
    Ok(metadata)
}

#[cfg(not(windows))]
fn verify_enumerated_file_still_current(
    directory: &Dir,
    name: &str,
    expected_identity: EnumeratedFileIdentity,
    expected_size: u64,
) -> Result<(), String> {
    let current = inspect_nofollow_pdf_metadata_snapshot(directory, name)
        .map_err(|message| format!("enumerated PDF candidate cannot be revalidated: {message}"))?;
    if enumerated_file_identity(&current)? != expected_identity {
        return Err("enumerated PDF candidate identity changed before content open".into());
    }
    if current.len() != expected_size {
        return Err("enumerated PDF candidate changed size before content open".into());
    }
    if current_hydration_disposition(directory, name, &current)?
        == ProviderHydrationDisposition::Placeholder
    {
        return Err("enumerated PDF candidate changed to a placeholder before content open".into());
    }
    Ok(())
}

#[cfg(unix)]
fn enumerated_file_identity(metadata: &Metadata) -> Result<EnumeratedFileIdentity, String> {
    use cap_std::fs::MetadataExt;

    Ok(EnumeratedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(any(unix, windows)))]
fn enumerated_file_identity(_: &Metadata) -> Result<EnumeratedFileIdentity, String> {
    Err("enumerated PDF candidate identity is unsupported; refusing content open".into())
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<EnumeratedFileIdentity, String> {
    let file = file
        .try_clone()
        .map(File::into_std)
        .map_err(|error| format!("cannot retain PDF identity handle: {error}"))?;
    mko_windows_acl::file_identity(&file)
        .map_err(|error| format!("cannot inspect stable PDF identity: {error}"))
}

#[cfg(windows)]
fn open_windows_enumerated_file(entry: &DirEntry) -> Result<File, String> {
    use cap_std::fs::MetadataExt;

    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    let file = entry
        .open_with(&options)
        .map_err(|error| format!("cannot open enumerated PDF candidate: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect retained PDF candidate: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("enumerated PDF candidate changed to a non-file or link".into());
    }
    if windows_hydration_disposition(metadata.file_attributes())
        == ProviderHydrationDisposition::Placeholder
    {
        return Err("enumerated PDF candidate changed to a placeholder before content open".into());
    }
    windows_file_identity(&file)?;
    Ok(file)
}

#[cfg(windows)]
fn verify_windows_file_still_current(
    directory: &Dir,
    name: &str,
    expected_identity: EnumeratedFileIdentity,
    expected_size: u64,
) -> Result<(), String> {
    use cap_std::fs::MetadataExt;

    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    let current = directory
        .open_with(name, &options)
        .map_err(|error| format!("enumerated PDF candidate cannot be revalidated: {error}"))?;
    let metadata = current
        .metadata()
        .map_err(|error| format!("cannot inspect revalidated PDF candidate: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("enumerated PDF candidate changed to a non-file or link".into());
    }
    if windows_file_identity(&current)? != expected_identity {
        return Err("enumerated PDF candidate identity changed before content open".into());
    }
    if metadata.len() != expected_size {
        return Err("enumerated PDF candidate changed size before content open".into());
    }
    if windows_hydration_disposition(metadata.file_attributes())
        == ProviderHydrationDisposition::Placeholder
    {
        return Err("enumerated PDF candidate changed to a placeholder before content open".into());
    }
    Ok(())
}

#[cfg(windows)]
fn current_hydration_disposition(
    _: &Dir,
    _: &str,
    metadata: &Metadata,
) -> Result<ProviderHydrationDisposition, String> {
    use cap_std::fs::MetadataExt;

    Ok(windows_hydration_disposition(metadata.file_attributes()))
}

#[cfg(not(windows))]
fn current_hydration_disposition(
    directory: &Dir,
    name: &str,
    _: &Metadata,
) -> Result<ProviderHydrationDisposition, String> {
    non_windows_hydration_disposition(directory, name)
}

#[cfg(any(windows, test))]
fn inspect_windows_enumerated_pdf(
    relative_path: PathBuf,
    attributes: u32,
    enumerated_size: u64,
    inspect_metadata_handle: impl FnOnce() -> Result<u64, String>,
) -> Result<ProviderMetadataEntry, String> {
    if windows_hydration_disposition(attributes) == ProviderHydrationDisposition::Placeholder {
        return Ok(ProviderMetadataEntry {
            relative_path,
            hydration: ProviderHydrationDisposition::Placeholder,
            size_bytes: enumerated_size,
            retained_file: None,
        });
    }
    Ok(ProviderMetadataEntry {
        relative_path,
        hydration: ProviderHydrationDisposition::Supported,
        size_bytes: inspect_metadata_handle()?,
        retained_file: None,
    })
}

#[cfg(any(windows, test))]
fn windows_hydration_disposition(attributes: u32) -> ProviderHydrationDisposition {
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    if attributes
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
    {
        ProviderHydrationDisposition::Placeholder
    } else {
        ProviderHydrationDisposition::Supported
    }
}

#[cfg(target_os = "macos")]
fn non_windows_hydration_disposition(
    directory: &Dir,
    name: &str,
) -> Result<ProviderHydrationDisposition, String> {
    use std::os::fd::AsFd;

    use nix::{fcntl::AtFlags, sys::stat::fstatat};

    const SF_DATALESS: u32 = 0x4000_0000;
    let metadata = fstatat(directory.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(|error| format!("cannot inspect PDF platform metadata: {error}"))?;
    Ok(if metadata.st_flags & SF_DATALESS != 0 {
        ProviderHydrationDisposition::Placeholder
    } else {
        ProviderHydrationDisposition::Supported
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn non_windows_hydration_disposition(
    _: &Dir,
    _: &str,
) -> Result<ProviderHydrationDisposition, String> {
    Ok(ProviderHydrationDisposition::Unsupported)
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

#[cfg(test)]
mod metadata_walk_tests {
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[cfg(not(windows))]
    use super::open_enumerated_file;
    use super::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderCatalogEntry, ProviderMetadataAccess,
        ProviderScanRequest, ScanLimits, inspect_provider_metadata_with_observer,
        inspect_windows_enumerated_pdf, materialize_metadata_entry,
        scan_provider_catalog_metadata_first_with_after_walk,
        scan_provider_catalog_metadata_first_with_before_file_open,
    };
    use crate::fingerprint::fingerprint_file;

    struct FixedElapsedClock;

    impl ElapsedClock for FixedElapsedClock {
        fn elapsed_ms(&self) -> u64 {
            0
        }
    }

    #[derive(Default)]
    struct AdvancingElapsedClock {
        elapsed_ms: AtomicU64,
    }

    impl ElapsedClock for AdvancingElapsedClock {
        fn elapsed_ms(&self) -> u64 {
            self.elapsed_ms.fetch_add(5, Ordering::Relaxed)
        }
    }

    #[cfg(unix)]
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
            &FixedElapsedClock,
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

    #[cfg(unix)]
    #[test]
    fn materialization_uses_retained_file_after_intermediate_directory_swap() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(&child).unwrap();
        let inside = child.join("paper.pdf");
        fs::write(&inside, b"%PDF-1.7\ninside\n%%EOF\n").unwrap();
        fs::write(
            outside.path().join("paper.pdf"),
            b"%PDF-1.7\noutside\n%%EOF\n",
        )
        .unwrap();
        let expected = fingerprint_file(&inside).unwrap();

        let scan = scan_provider_catalog_metadata_first_with_after_walk(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            || {
                fs::rename(&child, root.path().join("child-original")).unwrap();
                symlink(outside.path(), &child).unwrap();
            },
        )
        .unwrap();

        let ProviderCatalogEntry::Readable(pdf) = &scan.entries[0] else {
            panic!("local PDF should be readable");
        };
        assert_eq!(pdf.fingerprint, expected);
    }

    #[cfg(unix)]
    #[test]
    fn materialization_uses_exact_enumerated_handle_after_same_basename_replacement() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        fs::write(&paper, b"%PDF-1.7\noriginal\n%%EOF\n").unwrap();
        let expected = fingerprint_file(&paper).unwrap();

        let scan = scan_provider_catalog_metadata_first_with_after_walk(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            || {
                fs::rename(&paper, root.path().join("enumerated-original.pdf")).unwrap();
                fs::write(&paper, b"%PDF-1.7\nreplacement\n%%EOF\n").unwrap();
            },
        )
        .unwrap();

        assert_eq!(only_readable_fingerprint(&scan), expected);
    }

    #[cfg(unix)]
    #[test]
    fn same_size_replacement_between_classification_and_open_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        let original = b"%PDF-1.7\noriginal-A\n%%EOF\n";
        let replacement = b"%PDF-1.7\nreplaced-B\n%%EOF\n";
        assert_eq!(original.len(), replacement.len());
        fs::write(&paper, original).unwrap();
        let mut hook_calls = 0;

        let scan = scan_provider_catalog_metadata_first_with_before_file_open(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            &mut |relative| {
                assert_eq!(relative, Path::new("paper.pdf"));
                hook_calls += 1;
                fs::rename(&paper, root.path().join("classified-original.pdf")).unwrap();
                fs::write(&paper, replacement).unwrap();
            },
        )
        .unwrap();

        assert_eq!(hook_calls, 1);
        assert_identity_race_warning(&scan);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_between_classification_and_open_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        fs::write(&paper, b"%PDF-1.7\noriginal\n%%EOF\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_paper = outside.path().join("outside.pdf");
        fs::write(&outside_paper, b"%PDF-1.7\noutside\n%%EOF\n").unwrap();
        let mut hook_calls = 0;

        let scan = scan_provider_catalog_metadata_first_with_before_file_open(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            &mut |relative| {
                assert_eq!(relative, Path::new("paper.pdf"));
                hook_calls += 1;
                fs::rename(&paper, root.path().join("classified-original.pdf")).unwrap();
                symlink(&outside_paper, &paper).unwrap();
            },
        )
        .unwrap();

        assert_eq!(hook_calls, 1);
        assert_identity_race_warning(&scan);
    }

    #[cfg(unix)]
    #[test]
    fn opened_handle_identity_is_checked_before_materialization() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        let original = b"%PDF-1.7\noriginal-A\n%%EOF\n";
        let replacement = b"%PDF-1.7\nreplaced-B\n%%EOF\n";
        assert_eq!(original.len(), replacement.len());
        fs::write(&paper, original).unwrap();
        let directory =
            cap_std::fs::Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let entry = directory.entries().unwrap().next().unwrap().unwrap();
        let enumerated = entry.metadata().unwrap();
        let identity = super::enumerated_file_identity(&enumerated).unwrap();

        fs::rename(&paper, root.path().join("classified-original.pdf")).unwrap();
        fs::write(&paper, replacement).unwrap();

        let error = match open_enumerated_file(&entry, identity, original.len() as u64) {
            Ok(_) => panic!("replacement handle must be rejected before materialization"),
            Err(error) => error,
        };
        assert!(error.contains("identity changed"), "{error}");
    }

    fn assert_identity_race_warning(scan: &super::ProviderCatalogScan) {
        assert!(!scan.scan_complete);
        assert!(!scan.mutation_safe);
        assert!(scan.entries.is_empty());
        assert_eq!(scan.warnings.len(), 1);
        assert_eq!(scan.warnings[0].code, "scan_file_unreadable");
    }

    #[cfg(unix)]
    #[test]
    fn one_identity_race_marks_an_otherwise_readable_catalog_mutation_unsafe() {
        let root = tempfile::tempdir().unwrap();
        let safe = root.path().join("a-safe.pdf");
        let raced = root.path().join("z-raced.pdf");
        fs::write(&safe, b"%PDF-1.7\nsafe\n%%EOF\n").unwrap();
        fs::write(&raced, b"%PDF-1.7\noriginal\n%%EOF\n").unwrap();
        let mut replaced = false;

        let scan = scan_provider_catalog_metadata_first_with_before_file_open(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            &mut |relative| {
                if relative == Path::new("z-raced.pdf") && !replaced {
                    replaced = true;
                    fs::rename(&raced, root.path().join("z-raced-original.pdf")).unwrap();
                    fs::write(&raced, b"%PDF-1.7\nchanged!\n%%EOF\n").unwrap();
                }
            },
        )
        .unwrap();

        assert!(replaced);
        assert!(!scan.scan_complete);
        assert!(!scan.mutation_safe);
        assert!(scan.entries.iter().any(|entry| {
            matches!(entry, ProviderCatalogEntry::Readable(pdf) if pdf.provider_locator == "a-safe.pdf")
        }));
        assert!(scan.warnings.iter().any(|warning| {
            warning.provider_locator.as_deref() == Some("z-raced.pdf")
                && warning.code == "scan_file_unreadable"
        }));
    }

    #[cfg(unix)]
    #[test]
    fn deletion_after_enumeration_does_not_become_a_top_level_error() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        fs::write(&paper, b"%PDF-1.7\ndeleted name\n%%EOF\n").unwrap();
        let expected = fingerprint_file(&paper).unwrap();

        let scan = scan_provider_catalog_metadata_first_with_after_walk(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            || fs::remove_file(&paper).unwrap(),
        )
        .unwrap();

        assert_eq!(only_readable_fingerprint(&scan), expected);
    }

    #[cfg(unix)]
    #[test]
    fn basename_symlink_race_never_reads_the_link_target_or_errors_the_command() {
        let root = tempfile::tempdir().unwrap();
        let paper = root.path().join("paper.pdf");
        fs::write(&paper, b"%PDF-1.7\noriginal handle\n%%EOF\n").unwrap();
        let expected = fingerprint_file(&paper).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_paper = outside.path().join("outside.pdf");
        fs::write(&outside_paper, b"%PDF-1.7\noutside target\n%%EOF\n").unwrap();

        let scan = scan_provider_catalog_metadata_first_with_after_walk(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            || {
                fs::rename(&paper, root.path().join("enumerated-original.pdf")).unwrap();
                symlink(&outside_paper, &paper).unwrap();
            },
        )
        .unwrap();

        assert_eq!(only_readable_fingerprint(&scan), expected);
    }

    fn only_readable_fingerprint(scan: &super::ProviderCatalogScan) -> crate::model::Fingerprint {
        let [ProviderCatalogEntry::Readable(pdf)] = scan.entries.as_slice() else {
            panic!("expected exactly one readable PDF: {:?}", scan.warnings);
        };
        pdf.fingerprint.clone()
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

        let walk = inspect_provider_metadata_with_observer(
            root.path(),
            limits,
            &FixedElapsedClock,
            &mut |_| {},
        )
        .unwrap();

        assert!(walk.entries.is_empty());
        assert!(
            walk.issues
                .iter()
                .any(|issue| issue.message.contains("entry limit"))
        );
    }

    #[test]
    fn enumerated_windows_recall_on_open_skips_metadata_handle_and_access() {
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
        let metadata_calls = Cell::new(0);
        let entry = inspect_windows_enumerated_pdf(
            PathBuf::from("placeholder.pdf"),
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            42,
            || {
                metadata_calls.set(metadata_calls.get() + 1);
                Ok(42)
            },
        )
        .unwrap();
        let access_calls = Cell::new(0);

        let access = entry.inspect_access(|| {
            access_calls.set(access_calls.get() + 1);
        });

        assert_eq!(access, ProviderMetadataAccess::Placeholder);
        assert_eq!(metadata_calls.get(), 0);
        assert_eq!(access_calls.get(), 0);
    }

    #[test]
    fn enumerated_windows_placeholder_flags_all_skip_later_inspection() {
        for attributes in [0x0000_1000, 0x0004_0000, 0x0040_0000] {
            let metadata_calls = Cell::new(0);
            let entry = inspect_windows_enumerated_pdf(
                PathBuf::from("placeholder.pdf"),
                attributes,
                42,
                || {
                    metadata_calls.set(metadata_calls.get() + 1);
                    Ok(42)
                },
            )
            .unwrap();
            let access_calls = Cell::new(0);

            let access = entry.inspect_access(|| {
                access_calls.set(access_calls.get() + 1);
            });

            assert_eq!(access, ProviderMetadataAccess::Placeholder);
            assert_eq!(metadata_calls.get(), 0, "attributes {attributes:#010x}");
            assert_eq!(access_calls.get(), 0, "attributes {attributes:#010x}");
        }
    }

    #[test]
    fn metadata_first_inbox_projection_never_opens_placeholder_content() {
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        let metadata_calls = Cell::new(0);
        let entry = inspect_windows_enumerated_pdf(
            PathBuf::from("offline.pdf"),
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            42,
            || {
                metadata_calls.set(metadata_calls.get() + 1);
                Ok(42)
            },
        )
        .unwrap();
        let content_open_calls = Cell::new(0);

        let materialized =
            materialize_metadata_entry(entry, |_, _| -> Result<(), crate::error::MkoError> {
                content_open_calls.set(content_open_calls.get() + 1);
                unreachable!("placeholder content must never be opened")
            });

        assert!(matches!(materialized, ProviderMetadataAccess::Placeholder));
        assert_eq!(metadata_calls.get(), 0);
        assert_eq!(content_open_calls.get(), 0);
    }

    #[test]
    fn missing_retained_handle_is_a_per_entry_unreadable_result() {
        let entry = super::ProviderMetadataEntry {
            relative_path: PathBuf::from("missing-handle.pdf"),
            hydration: super::ProviderHydrationDisposition::Supported,
            size_bytes: 42,
            retained_file: None,
        };

        let materialized =
            materialize_metadata_entry(entry, |_, _| -> Result<(), crate::error::MkoError> {
                unreachable!("a missing handle cannot be inspected")
            });

        let ProviderMetadataAccess::Supported(Err(error)) = materialized else {
            panic!("missing handle must stay a per-entry error");
        };
        assert_eq!(error.code(), "scan_file_unreadable");
    }

    #[cfg(not(any(unix, windows)))]
    #[test]
    fn unsupported_file_identity_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("paper.pdf"),
            b"%PDF-1.7\nunsupported\n%%EOF\n",
        )
        .unwrap();

        let scan = scan_provider_catalog_metadata_first_with_before_file_open(
            ProviderScanRequest::new(root.path()),
            &FixedElapsedClock,
            &mut |_| panic!("unsupported identity must fail before content open"),
        )
        .unwrap();

        assert_identity_race_warning(&scan);
    }

    #[test]
    fn enumerated_windows_non_placeholder_runs_metadata_and_denied_access_once() {
        let metadata_calls = Cell::new(0);
        let entry = inspect_windows_enumerated_pdf(PathBuf::from("local.pdf"), 0, 42, || {
            metadata_calls.set(metadata_calls.get() + 1);
            Ok(42)
        })
        .unwrap();
        let access_calls = Cell::new(0);

        let access = entry.inspect_access(|| {
            access_calls.set(access_calls.get() + 1);
            false
        });

        assert_eq!(access, ProviderMetadataAccess::Supported(false));
        assert_eq!(metadata_calls.get(), 1);
        assert_eq!(access_calls.get(), 1);
    }

    #[test]
    fn metadata_walk_uses_injected_elapsed_clock_for_time_limit() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("visible.pdf"), b"visible").unwrap();
        let limits = ScanLimits {
            max_elapsed_ms: 1,
            ..DEFAULT_SCAN_LIMITS
        };

        let walk = inspect_provider_metadata_with_observer(
            root.path(),
            limits,
            &AdvancingElapsedClock::default(),
            &mut |_| {},
        )
        .unwrap();

        assert!(walk.entries.is_empty());
        assert!(
            walk.issues
                .iter()
                .any(|issue| issue.message.contains("time limit"))
        );
    }
}
