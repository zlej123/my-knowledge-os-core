use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use crate::{
    canonical_source::validate_canonical_source,
    catalog::{
        CatalogBlocker, CatalogEvidence, CatalogItem, SourceObservation, classify_catalog_item,
    },
    error::MkoError,
    front_matter::parse_markdown,
    json_v1::{DiagnosticData, InboxData, InboxItemData, NextAction, ScanLimitsData, UserState},
    model::{AssetRecord, AssetStatus, SourceRecord, SourceStatus},
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderScanRequest, ProviderScanWarning, ScanLimits,
        scan_provider_pdfs,
    },
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

type SourceScanResult = (
    HashMap<String, SourceObservation>,
    Vec<DiagnosticData>,
    bool,
);

pub fn scan_inbox(
    request: InboxScanRequest,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<InboxScanResult, MkoError> {
    let repository_root = canonical_readable_directory(
        &request.repository_root,
        "repository_unreadable",
        "The repository is not readable.",
    )?;
    let scan = scan_provider_pdfs(
        ProviderScanRequest::new(&request.provider_root).with_limits(request.limits),
        elapsed_clock,
    )?;
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

    let active_locks = read_lock_asset_ids(&repository_root, request.limits.max_entries)?;
    let assets_by_locator = assets
        .iter()
        .map(|asset| (asset.provider.locator.as_str(), asset))
        .collect::<HashMap<_, _>>();
    let mut seen_assets = BTreeSet::new();
    let mut catalog = Vec::new();
    for pdf in &scan.pdfs {
        let Some(asset) = assets_by_locator
            .get(pdf.provider_locator.as_str())
            .copied()
        else {
            catalog.push(classify_catalog_item(
                &pdf.provider_locator,
                None,
                CatalogEvidence {
                    asset_status: None,
                    source: SourceObservation::Absent,
                    blocker: CatalogBlocker::None,
                },
            ));
            continue;
        };
        seen_assets.insert(asset.id.clone());
        let blocker = blocker_for(
            asset,
            &sources,
            active_locks.contains(&asset.id),
            pdf.size_bytes == asset.size_bytes && pdf.fingerprint == asset.fingerprint,
        );
        catalog.push(classify_catalog_item(
            &pdf.provider_locator,
            Some(asset.id.clone()),
            CatalogEvidence {
                asset_status: Some(asset.asset_status.clone()),
                source: source_for(asset, &sources),
                blocker,
            },
        ));
    }
    for asset in assets
        .iter()
        .filter(|asset| !seen_assets.contains(&asset.id))
    {
        catalog.push(classify_catalog_item(
            &asset.provider.locator,
            Some(asset.id.clone()),
            CatalogEvidence {
                asset_status: Some(asset.asset_status.clone()),
                source: source_for(asset, &sources),
                blocker: CatalogBlocker::ProviderMissing,
            },
        ));
    }
    catalog.sort_by(|left, right| left.provider_locator.cmp(&right.provider_locator));
    let state_counts = count_states(&catalog);
    let primary_blocker = errors.first().cloned().or_else(|| {
        catalog
            .iter()
            .find(|item| item.user_state == UserState::Blocked)
            .and_then(|item| item.diagnostic.clone())
    });
    let recommended_action = recommended_action(&catalog);
    let visible_limit = usize::try_from(request.limits.max_batch_items).unwrap_or(usize::MAX);
    let remaining = catalog.len().saturating_sub(visible_limit) as u64;
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
        catalog.truncate(visible_limit);
    }
    warnings.sort_by(|left, right| left.code.cmp(&right.code).then(left.path.cmp(&right.path)));
    warnings.dedup();
    errors.sort_by(|left, right| left.code.cmp(&right.code).then(left.path.cmp(&right.path)));
    errors.dedup();
    Ok(InboxScanResult {
        scan_complete,
        scan_limits: request.limits,
        items: catalog,
        errors,
        warnings,
        remaining,
        state_counts,
        primary_blocker,
        recommended_action,
    })
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

fn read_lock_asset_ids(
    repository_root: &Path,
    max_entries: u64,
) -> Result<BTreeSet<String>, MkoError> {
    let directory = repository_root.join(".knowledge-os/runtime/locks");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "lock_read_failed",
                "lock path is not a directory",
            ));
        }
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    };
    let mut ids = BTreeSet::new();
    for entry in entries.take(usize::try_from(max_entries).unwrap_or(usize::MAX)) {
        let entry = entry.map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(id) = name.strip_suffix(".lock") {
            ids.insert(id.to_owned());
        } else if let Some(id) = name.strip_suffix(".lock.takeover") {
            ids.insert(id.to_owned());
        }
    }
    Ok(ids)
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

fn recommended_action(items: &[CatalogItem]) -> NextAction {
    const PRIORITY: [NextAction; 8] = [
        NextAction::Configure,
        NextAction::Repair,
        NextAction::Hydrate,
        NextAction::Retry,
        NextAction::Review,
        NextAction::WriteDraft,
        NextAction::Prepare,
        NextAction::Add,
    ];
    PRIORITY
        .into_iter()
        .find(|action| items.iter().any(|item| &item.next_action == action))
        .unwrap_or(NextAction::None)
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
