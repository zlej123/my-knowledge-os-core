use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions, OpenOptionsExt},
};

use crate::{
    asset_validation::validate_canonical_asset,
    canonical_source::validate_canonical_source,
    catalog::{
        CatalogBlocker, CatalogEvidence, CatalogItem, SourceObservation, classify_catalog_item,
    },
    error::MkoError,
    front_matter::parse_markdown,
    json_v1::{DiagnosticData, InboxData, InboxItemData, NextAction, ScanLimitsData, UserState},
    model::Fingerprint,
    model::{AssetRecord, AssetStatus, SourceRecord, SourceStatus},
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderCatalogEntry, ProviderScanRequest,
        ProviderScanWarning, ScanDeadline, ScanLimits,
        scan_provider_catalog_metadata_first_with_deadline,
    },
    status::select_status_decision,
};

#[derive(Clone, Debug)]
pub struct InboxScanRequest {
    repository_root: PathBuf,
    provider_root: PathBuf,
    limits: ScanLimits,
}

impl InboxScanRequest {
    pub fn new(repository_root: impl AsRef<Path>, provider_root: impl AsRef<Path>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
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
pub struct InboxScanResult {
    pub scan_complete: bool,
    pub scan_limits: ScanLimits,
    pub items: Vec<CatalogItem>,
    pub errors: Vec<DiagnosticData>,
    pub warnings: Vec<DiagnosticData>,
    pub remaining: u64,
    pub state_counts: BTreeMap<UserState, u64>,
    pub primary_blocker: Option<DiagnosticData>,
    pub recommended_action: NextAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredPdfSnapshot {
    pub fingerprint: Fingerprint,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InboxAddScan {
    pub report: InboxScanResult,
    pub snapshots: HashMap<String, DiscoveredPdfSnapshot>,
}

type SourceScanResult = (
    HashMap<String, SourceObservation>,
    Vec<DiagnosticData>,
    bool,
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogProjection {
    pub items: Vec<CatalogItem>,
    pub remaining: u64,
}

pub fn project_catalog_items(mut items: Vec<CatalogItem>, max_items: u64) -> CatalogProjection {
    items.sort_by(|left, right| {
        action_priority(&left.next_action)
            .cmp(&action_priority(&right.next_action))
            .then(left.provider_locator.cmp(&right.provider_locator))
            .then(left.asset_id.cmp(&right.asset_id))
    });
    let visible_limit = usize::try_from(max_items).unwrap_or(usize::MAX);
    let remaining = items.len().saturating_sub(visible_limit) as u64;
    items.truncate(visible_limit);
    CatalogProjection { items, remaining }
}

pub fn scan_inbox(
    request: InboxScanRequest,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<InboxScanResult, MkoError> {
    scan_inbox_internal(request, elapsed_clock, true).map(|scan| scan.report)
}

pub(crate) fn scan_inbox_for_add(
    request: InboxScanRequest,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<InboxAddScan, MkoError> {
    scan_inbox_internal(request, elapsed_clock, false)
}

fn scan_inbox_internal(
    request: InboxScanRequest,
    elapsed_clock: &dyn ElapsedClock,
    project_for_read_only_output: bool,
) -> Result<InboxAddScan, MkoError> {
    let repository_root = canonical_readable_directory(
        &request.repository_root,
        "repository_unreadable",
        "The repository is not readable.",
    )?;
    let deadline = ScanDeadline::start(elapsed_clock, request.limits);
    let scan = scan_provider_catalog_metadata_first_with_deadline(
        ProviderScanRequest::new(&request.provider_root).with_limits(request.limits),
        &deadline,
    )?;
    let snapshots = scan
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ProviderCatalogEntry::Readable(pdf) => Some((
                pdf.provider_locator.clone(),
                DiscoveredPdfSnapshot {
                    fingerprint: pdf.fingerprint.clone(),
                    size_bytes: pdf.size_bytes,
                },
            )),
            ProviderCatalogEntry::Placeholder { .. } => None,
        })
        .collect();
    let mut warnings = scan.warnings.iter().map(scan_warning).collect::<Vec<_>>();
    let mut scan_complete = scan.scan_complete;
    let (assets, mut errors, repository_complete) = read_assets(&repository_root, request.limits)?;
    scan_complete &= repository_complete;
    let (sources, source_errors, sources_complete) =
        read_sources(&repository_root, &assets, request.limits)?;
    errors.extend(source_errors);
    scan_complete &= sources_complete;
    if !repository_complete || !sources_complete {
        warnings.push(DiagnosticData {
            code: "repository_scan_limit_reached".into(),
            message: "The repository catalog scan reached a fixed limit.".into(),
            path: None,
        });
    }

    let lock_scan = read_lock_asset_ids(&repository_root, request.limits.max_entries, &deadline);
    scan_complete &= lock_scan.complete;
    if let Some(warning) = lock_scan.warning.clone() {
        warnings.push(warning);
    }
    let (assets_by_locator, locator_conflicts) = current_assets_by_locator(&assets, &mut errors);
    let assets_by_fingerprint = current_assets_by_fingerprint(&assets);
    let mut seen_assets = BTreeSet::new();
    let mut catalog = Vec::new();
    for provider_entry in &scan.entries {
        let (provider_locator, readable_pdf) = match provider_entry {
            ProviderCatalogEntry::Placeholder {
                provider_locator, ..
            } => (provider_locator.as_str(), None),
            ProviderCatalogEntry::Readable(pdf) => (pdf.provider_locator.as_str(), Some(pdf)),
        };
        let asset = assets_by_locator
            .get(provider_locator)
            .copied()
            .or_else(|| {
                readable_pdf.and_then(|pdf| {
                    assets_by_fingerprint
                        .get(&(
                            pdf.fingerprint.method.as_str(),
                            pdf.fingerprint.value.as_str(),
                            pdf.size_bytes,
                        ))
                        .copied()
                })
            });
        let Some(asset) = asset else {
            catalog.push(classify_catalog_item(
                provider_locator,
                None,
                CatalogEvidence {
                    asset_status: None,
                    source: SourceObservation::Absent,
                    blocker: if !lock_scan.complete {
                        CatalogBlocker::ActiveLock
                    } else if readable_pdf.is_some() {
                        CatalogBlocker::None
                    } else {
                        CatalogBlocker::ProviderMissing
                    },
                },
            ));
            continue;
        };
        seen_assets.insert(asset.id.clone());
        let blocker = if !lock_scan.complete {
            CatalogBlocker::ActiveLock
        } else if locator_conflicts.contains(provider_locator) {
            CatalogBlocker::StateMismatch
        } else {
            match readable_pdf {
                None => CatalogBlocker::ProviderMissing,
                Some(pdf) => blocker_for(
                    asset,
                    &sources,
                    lock_scan.asset_ids.contains(&asset.id),
                    pdf.size_bytes == asset.size_bytes && pdf.fingerprint == asset.fingerprint,
                ),
            }
        };
        catalog.push(classify_catalog_item(
            provider_locator,
            Some(asset.id.clone()),
            CatalogEvidence {
                asset_status: Some(asset.asset_status.clone()),
                source: source_for(asset, &sources),
                blocker,
            },
        ));
    }
    if scan.scan_complete {
        for asset in assets.iter().filter(|asset| {
            asset.asset_status != AssetStatus::Superseded && !seen_assets.contains(&asset.id)
        }) {
            catalog.push(classify_catalog_item(
                &asset.provider.locator,
                Some(asset.id.clone()),
                CatalogEvidence {
                    asset_status: Some(asset.asset_status.clone()),
                    source: source_for(asset, &sources),
                    blocker: if lock_scan.complete {
                        CatalogBlocker::ProviderMissing
                    } else {
                        CatalogBlocker::ActiveLock
                    },
                },
            ));
        }
    }
    let mut state_counts = count_states(&catalog);
    let invalid_registry_records = errors
        .iter()
        .filter(|error| {
            matches!(
                error.code.as_str(),
                "registry_invalid" | "path_not_portable" | "invalid_state_transition"
            )
        })
        .filter_map(|error| error.path.as_deref())
        .collect::<BTreeSet<_>>()
        .len() as u64;
    *state_counts.entry(UserState::Blocked).or_default() += invalid_registry_records;
    let projection = if project_for_read_only_output {
        project_catalog_items(catalog, request.limits.max_batch_items)
    } else {
        CatalogProjection {
            items: catalog,
            remaining: 0,
        }
    };
    let remaining = projection.remaining;
    let catalog = projection.items;
    if remaining > 0 {
        scan_complete = false;
        warnings.push(DiagnosticData {
            code: "actionable_limit_reached".into(),
            message: format!(
                "{} actionable inbox items shown; {remaining} remaining.",
                request.limits.max_batch_items
            ),
            path: None,
        });
    }
    warnings.sort_by(|left, right| left.code.cmp(&right.code).then(left.path.cmp(&right.path)));
    warnings.dedup();
    errors.sort_by(|left, right| left.code.cmp(&right.code).then(left.path.cmp(&right.path)));
    errors.dedup();
    let (primary_blocker, recommended_action) =
        select_status_decision(scan_complete, &catalog, &errors, &warnings);
    Ok(InboxAddScan {
        report: InboxScanResult {
            scan_complete,
            scan_limits: request.limits,
            items: catalog,
            errors,
            warnings,
            remaining,
            state_counts,
            primary_blocker,
            recommended_action,
        },
        snapshots,
    })
}

fn current_assets_by_fingerprint(
    assets: &[AssetRecord],
) -> HashMap<(&str, &str, u64), &AssetRecord> {
    assets
        .iter()
        .filter(|asset| asset.asset_status != AssetStatus::Superseded)
        .map(|asset| {
            (
                (
                    asset.fingerprint.method.as_str(),
                    asset.fingerprint.value.as_str(),
                    asset.size_bytes,
                ),
                asset,
            )
        })
        .collect()
}

fn current_assets_by_locator<'a>(
    assets: &'a [AssetRecord],
    errors: &mut Vec<DiagnosticData>,
) -> (HashMap<&'a str, &'a AssetRecord>, BTreeSet<&'a str>) {
    let mut by_locator = HashMap::new();
    let mut conflicts = BTreeSet::new();
    for asset in assets
        .iter()
        .filter(|asset| asset.asset_status != AssetStatus::Superseded)
    {
        if let Some(previous) = by_locator.insert(asset.provider.locator.as_str(), asset) {
            conflicts.insert(asset.provider.locator.as_str());
            errors.push(DiagnosticData {
                code: "duplicate_conflict".into(),
                message: "Multiple current Asset records share one provider locator.".into(),
                path: Some(format!("{};{}", previous.id, asset.id)),
            });
        }
    }
    (by_locator, conflicts)
}

fn blocker_for(
    asset: &AssetRecord,
    sources: &HashMap<String, SourceObservation>,
    locked: bool,
    provider_matches: bool,
) -> CatalogBlocker {
    if locked {
        return CatalogBlocker::ActiveLock;
    }
    if !provider_matches {
        return CatalogBlocker::ProviderChanged;
    }
    let source = source_for(asset, sources);
    let matches = match asset.asset_status {
        AssetStatus::Registered | AssetStatus::Extracted => source == SourceObservation::Absent,
        AssetStatus::ReviewPending => source == SourceObservation::ReviewPending,
        AssetStatus::Processed => source == SourceObservation::Approved,
        AssetStatus::Changed
        | AssetStatus::Missing
        | AssetStatus::Superseded
        | AssetStatus::Failed => true,
    };
    if matches {
        CatalogBlocker::None
    } else {
        CatalogBlocker::StateMismatch
    }
}

fn source_for(
    asset: &AssetRecord,
    sources: &HashMap<String, SourceObservation>,
) -> SourceObservation {
    sources
        .get(&asset.id)
        .copied()
        .unwrap_or(SourceObservation::Absent)
}

fn read_assets(
    repository_root: &Path,
    limits: ScanLimits,
) -> Result<(Vec<AssetRecord>, Vec<DiagnosticData>, bool), MkoError> {
    let directory = repository_root.join("assets/registry");
    read_flat_markdown(&directory, limits, |relative, input| {
        let document = parse_markdown::<AssetRecord>(input)?;
        let expected = format!("{}.md", document.metadata.id);
        if relative != expected {
            return Err(MkoError::new(
                "registry_conflict",
                "Registry filename and Asset ID disagree.",
            ));
        }
        let canonical_path = format!("assets/registry/{relative}");
        if let Some(issue) = validate_canonical_asset(&canonical_path, &document.metadata)
            .into_iter()
            .next()
        {
            return Err(MkoError::new(issue.code, issue.message));
        }
        Ok(document.metadata)
    })
}

fn read_sources(
    repository_root: &Path,
    assets: &[AssetRecord],
    limits: ScanLimits,
) -> Result<SourceScanResult, MkoError> {
    let directory = repository_root.join("sources");
    let asset_by_id = assets
        .iter()
        .map(|asset| (asset.id.as_str(), asset))
        .collect::<HashMap<_, _>>();
    let (documents, mut errors, complete) =
        read_flat_markdown(&directory, limits, |relative, input| {
            let document = parse_markdown::<SourceRecord>(input)?;
            let asset_id = document
                .metadata
                .relations
                .asset_ids
                .first()
                .ok_or_else(|| MkoError::new("source_invalid", "Source has no related Asset."))?;
            let asset = asset_by_id
                .get(asset_id.as_str())
                .ok_or_else(|| MkoError::new("relation_missing", "Source Asset is absent."))?;
            let path = format!("sources/{relative}");
            let observation =
                if validate_canonical_source(&path, &document.metadata, &document.body, asset)
                    .is_err()
                {
                    SourceObservation::Invalid
                } else {
                    match document.metadata.status {
                        SourceStatus::ReviewPending => SourceObservation::ReviewPending,
                        SourceStatus::Approved => SourceObservation::Approved,
                        SourceStatus::Rejected | SourceStatus::Stale | SourceStatus::Archived => {
                            SourceObservation::Invalid
                        }
                    }
                };
            Ok((asset_id.clone(), observation))
        })?;
    let mut observations = HashMap::new();
    for (asset_id, observation) in documents {
        if observations.insert(asset_id.clone(), observation).is_some() {
            observations.insert(asset_id.clone(), SourceObservation::Invalid);
            errors.push(DiagnosticData {
                code: "duplicate_conflict".into(),
                message: "Multiple Source documents relate to the same Asset.".into(),
                path: Some(asset_id),
            });
        }
    }
    Ok((observations, errors, complete))
}

fn read_flat_markdown<T>(
    directory: &Path,
    limits: ScanLimits,
    mut parse: impl FnMut(String, &str) -> Result<T, MkoError>,
) -> Result<(Vec<T>, Vec<DiagnosticData>, bool), MkoError> {
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new(), true));
        }
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "repository_unreadable",
                "Repository catalog path must be a non-link directory.",
            ));
        }
        Err(error) => {
            return Err(MkoError::new("repository_unreadable", error.to_string()));
        }
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(MkoError::new("repository_unreadable", error.to_string()));
        }
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("repository_unreadable", error.to_string()))?;
    paths.sort();
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut total_bytes = 0_u64;
    let mut complete = true;
    for (index, path) in paths.into_iter().enumerate() {
        if index as u64 >= limits.max_entries {
            complete = false;
            break;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| MkoError::new("repository_unreadable", error.to_string()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > limits.max_total_bytes {
            complete = false;
            break;
        }
        let relative = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| MkoError::new("invalid_path", "Repository filename is not UTF-8."))?
            .to_owned();
        let input = fs::read_to_string(&path)
            .map_err(|error| MkoError::new("repository_unreadable", error.to_string()))?;
        match parse(relative.clone(), &input) {
            Ok(value) => output.push(value),
            Err(error) => errors.push(DiagnosticData {
                code: error.code().into(),
                message: error.message().into(),
                path: Some(path.display().to_string()),
            }),
        }
    }
    Ok((output, errors, complete))
}

#[derive(Clone, Debug, Default)]
struct LockScan {
    asset_ids: BTreeSet<String>,
    complete: bool,
    warning: Option<DiagnosticData>,
}

fn read_lock_asset_ids(
    repository_root: &Path,
    max_entries: u64,
    deadline: &ScanDeadline<'_>,
) -> LockScan {
    read_lock_asset_ids_with_after_open(repository_root, max_entries, deadline, || {})
}

fn read_lock_asset_ids_with_after_open(
    repository_root: &Path,
    max_entries: u64,
    deadline: &ScanDeadline<'_>,
    after_open: impl FnOnce(),
) -> LockScan {
    if deadline.check().is_err() {
        return lock_scan_timeout();
    }
    let directory = match open_lock_directory_nofollow(repository_root) {
        Ok(Some(directory)) => directory,
        Ok(None) => {
            return LockScan {
                complete: true,
                ..LockScan::default()
            };
        }
        Err(error) => return incomplete_lock_scan(&error.to_string()),
    };
    after_open();
    let entries = match directory.entries() {
        Ok(entries) => entries,
        Err(error) => return incomplete_lock_scan(&error.to_string()),
    };
    let mut names = Vec::new();
    for entry in entries {
        if deadline.check().is_err() {
            return lock_scan_timeout();
        }
        let Ok(entry) = entry else {
            return incomplete_lock_scan("cannot enumerate every runtime lock");
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.ends_with(".lock") || name.ends_with(".lock.takeover") {
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => return incomplete_lock_scan("cannot classify a runtime lock record"),
            };
            if !file_type.is_file() || file_type.is_symlink() {
                return incomplete_lock_scan("a runtime lock record is not a regular file");
            }
            names.push(name);
            if names.len() as u64 > max_entries {
                return LockScan {
                    complete: false,
                    warning: Some(lock_scan_warning(
                        "The runtime lock scan reached its fixed record limit.",
                    )),
                    ..LockScan::default()
                };
            }
        }
    }
    if deadline.check().is_err() {
        return lock_scan_timeout();
    }
    names.sort();
    let mut ids = BTreeSet::new();
    for name in names {
        if let Some(id) = name.strip_suffix(".lock") {
            ids.insert(id.to_owned());
        } else if let Some(id) = name.strip_suffix(".lock.takeover") {
            ids.insert(id.to_owned());
        }
    }
    LockScan {
        asset_ids: ids,
        complete: true,
        warning: None,
    }
}

fn lock_scan_timeout() -> LockScan {
    LockScan {
        complete: false,
        warning: Some(lock_scan_warning(
            "The runtime lock scan reached its time limit.",
        )),
        ..LockScan::default()
    }
}

fn open_lock_directory_nofollow(repository_root: &Path) -> std::io::Result<Option<Dir>> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, true);
    let root = File::open_ambient_with(repository_root, &options, ambient_authority())?;
    let root_metadata = root.metadata()?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "repository root is not a non-link directory",
        ));
    }
    let mut directory = Dir::from_std_file(root.into_std());
    for component in [".knowledge-os", "runtime", "locks"] {
        let mut options = OpenOptions::new();
        options.read(true);
        configure_nofollow(&mut options, true);
        let file = match directory.open_with(component, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "runtime lock path contains a link or non-directory",
            ));
        }
        directory = Dir::from_std_file(file.into_std());
    }
    Ok(Some(directory))
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

fn incomplete_lock_scan(message: &str) -> LockScan {
    LockScan {
        complete: false,
        warning: Some(lock_scan_warning(message)),
        ..LockScan::default()
    }
}

fn lock_scan_warning(message: &str) -> DiagnosticData {
    DiagnosticData {
        code: "lock_scan_incomplete".into(),
        message: message.into(),
        path: None,
    }
}

fn canonical_readable_directory(
    path: &Path,
    code: &str,
    message: &str,
) -> Result<PathBuf, MkoError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| MkoError::new(code, message))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(code, message));
    }
    fs::canonicalize(path).map_err(|_| MkoError::new(code, message))
}

fn scan_warning(warning: &ProviderScanWarning) -> DiagnosticData {
    let message = match warning.code.as_str() {
        "scan_time_limit" => "The inbox scan reached its time limit.".into(),
        "scan_entry_limit" => "The inbox scan reached its entry limit.".into(),
        "scan_byte_limit" => "The inbox scan reached its aggregate byte limit.".into(),
        "scan_depth_limit" => "The inbox scan reached its depth limit.".into(),
        _ => warning.message.clone(),
    };
    DiagnosticData {
        code: if warning.code.starts_with("scan_") && warning.code.ends_with("_limit") {
            "scan_limit_reached".into()
        } else {
            warning.code.clone()
        },
        message,
        path: warning.provider_locator.clone(),
    }
}

fn count_states(items: &[CatalogItem]) -> BTreeMap<UserState, u64> {
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
    for item in items {
        *counts.entry(item.user_state.clone()).or_default() += 1;
    }
    counts
}

fn action_priority(action: &NextAction) -> u8 {
    match action {
        NextAction::Configure => 0,
        NextAction::Repair => 1,
        NextAction::Hydrate => 2,
        NextAction::Retry => 3,
        NextAction::Review => 4,
        NextAction::WriteDraft => 5,
        NextAction::Prepare => 6,
        NextAction::Add => 7,
        NextAction::None => 8,
    }
}

impl From<InboxScanResult> for InboxData {
    fn from(result: InboxScanResult) -> Self {
        Self {
            scan_complete: result.scan_complete,
            scan_limits: ScanLimitsData {
                max_entries: result.scan_limits.max_entries,
                max_total_bytes: result.scan_limits.max_total_bytes,
                max_elapsed_ms: result.scan_limits.max_elapsed_ms,
                max_depth: result.scan_limits.max_depth,
                max_batch_items: result.scan_limits.max_batch_items,
            },
            items: result
                .items
                .into_iter()
                .map(|item| InboxItemData {
                    provider_locator: item.provider_locator,
                    user_state: item.user_state,
                    asset_id: item.asset_id,
                    next_action: item.next_action,
                })
                .collect(),
            errors: result.errors,
            warnings: result.warnings,
        }
    }
}

#[cfg(test)]
mod lock_scan_tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::read_lock_asset_ids_with_after_open;
    use crate::provider_scan::{DEFAULT_SCAN_LIMITS, ElapsedClock, ScanDeadline};

    struct FixedElapsedClock;

    impl ElapsedClock for FixedElapsedClock {
        fn elapsed_ms(&self) -> u64 {
            0
        }
    }

    #[cfg(unix)]
    #[test]
    fn retained_lock_directory_ignores_path_swapped_to_external_symlink() {
        let repository = tempfile::tempdir().unwrap();
        let locks = repository.path().join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        fs::write(locks.join("internal.lock"), b"{}").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("external.lock"), b"{}").unwrap();
        let deadline = ScanDeadline::start(&FixedElapsedClock, DEFAULT_SCAN_LIMITS);

        let scan = read_lock_asset_ids_with_after_open(
            repository.path(),
            DEFAULT_SCAN_LIMITS.max_entries,
            &deadline,
            || {
                fs::rename(
                    &locks,
                    repository
                        .path()
                        .join(".knowledge-os/runtime/locks-original"),
                )
                .unwrap();
                symlink(outside.path(), &locks).unwrap();
            },
        );

        assert!(scan.complete);
        assert!(scan.asset_ids.contains("internal"));
        assert!(!scan.asset_ids.contains("external"));
    }

    #[test]
    fn expired_deadline_fails_closed_before_opening_a_missing_lock_directory() {
        let repository = tempfile::tempdir().unwrap();
        let clock = MutableElapsedClock::default();
        let deadline = ScanDeadline::start(
            &clock,
            crate::provider_scan::ScanLimits {
                max_elapsed_ms: 1,
                ..DEFAULT_SCAN_LIMITS
            },
        );
        clock.elapsed_ms.store(1, Ordering::Relaxed);

        let scan = super::read_lock_asset_ids(
            repository.path(),
            DEFAULT_SCAN_LIMITS.max_entries,
            &deadline,
        );

        assert_lock_timeout(scan);
    }

    #[test]
    fn expired_deadline_fails_closed_after_enumerating_an_empty_lock_directory() {
        let repository = tempfile::tempdir().unwrap();
        fs::create_dir_all(repository.path().join(".knowledge-os/runtime/locks")).unwrap();
        let clock = MutableElapsedClock::default();
        let deadline = ScanDeadline::start(
            &clock,
            crate::provider_scan::ScanLimits {
                max_elapsed_ms: 1,
                ..DEFAULT_SCAN_LIMITS
            },
        );
        clock.elapsed_ms.store(1, Ordering::Relaxed);

        let scan = super::read_lock_asset_ids(
            repository.path(),
            DEFAULT_SCAN_LIMITS.max_entries,
            &deadline,
        );

        assert_lock_timeout(scan);
    }

    #[derive(Default)]
    struct MutableElapsedClock {
        elapsed_ms: AtomicU64,
    }

    impl ElapsedClock for MutableElapsedClock {
        fn elapsed_ms(&self) -> u64 {
            self.elapsed_ms.load(Ordering::Relaxed)
        }
    }

    fn assert_lock_timeout(scan: super::LockScan) {
        assert!(!scan.complete);
        let warning = scan.warning.expect("timeout warning");
        assert_eq!(warning.code, "lock_scan_incomplete");
        assert_eq!(
            warning.message,
            "The runtime lock scan reached its time limit."
        );
    }
}
