use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    atomic::write_replace_capability_validated_at_commit,
    canonical_source::validate_canonical_source,
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::AssetLock,
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    state::transition_asset,
};
use cap_std::{ambient_authority, fs::Dir, time::SystemTime};
use sha2::{Digest, Sha256};

const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SOURCE_ENTRIES: usize = 1024;
const MAX_SOURCE_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SOURCE_FILENAME_BYTES: usize = 255;

#[derive(Clone, Debug)]
pub struct ApproveSourceRequest {
    repository_root: PathBuf,
    source_id: String,
}

impl ApproveSourceRequest {
    pub fn new(repository_root: impl AsRef<Path>, source_id: impl Into<String>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            source_id: source_id.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApproveSourceResult {
    pub source_id: String,
    pub revision: String,
    pub source_path: String,
}

pub trait ApprovalTerminal {
    fn stdin_is_terminal(&self) -> bool;
    fn stdout_is_terminal(&self) -> bool;
    fn write_all(&mut self, text: &str) -> io::Result<()>;
    fn read_line(&mut self, output: &mut String) -> io::Result<usize>;
}

pub trait ApprovalObserver {
    fn before_publication(&mut self) -> io::Result<()>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitSnapshot {
    pub working: Vec<u8>,
    pub staged: Vec<u8>,
}

pub trait GitSnapshotProvider {
    fn snapshot(
        &self,
        repository_root: &Path,
        source_path: &Path,
        asset_path: &Path,
    ) -> Result<GitSnapshot, MkoError>;
}

pub(crate) struct LockedPublicationRequest {
    pub(crate) repository_root: PathBuf,
    pub(crate) repository: Dir,
    pub(crate) sources: Dir,
    pub(crate) registry: Dir,
    pub(crate) source_filename: String,
    pub(crate) source_path: String,
    pub(crate) asset_path: String,
    pub(crate) asset_filename: String,
    pub(crate) source: SourceRecord,
    pub(crate) source_body: String,
    pub(crate) source_snapshot: SourceSnapshot,
    pub(crate) asset: crate::model::AssetRecord,
    pub(crate) asset_bytes: Vec<u8>,
    pub(crate) asset_body: String,
    pub(crate) revision: String,
    pub(crate) git_snapshot: GitSnapshot,
    pub(crate) working_digest: String,
    pub(crate) staged_digest: String,
}

struct EmptyGitSnapshotProvider;

impl GitSnapshotProvider for EmptyGitSnapshotProvider {
    fn snapshot(
        &self,
        _repository_root: &Path,
        _source_path: &Path,
        _asset_path: &Path,
    ) -> Result<GitSnapshot, MkoError> {
        // The frozen v0.1 `human approve-source` contract allowed approval
        // outside a Git repository and displayed only a best-effort summary.
        // Its explicit compatibility provider therefore binds an empty Git
        // snapshot, while the v0.2 `review` command requires real bounded diffs.
        Ok(GitSnapshot::default())
    }
}

pub fn approve_source(request: ApproveSourceRequest) -> Result<ApproveSourceResult, MkoError> {
    let mut terminal = SystemApprovalTerminal;
    approve_source_with_terminal_and_clock(request, &mut terminal, &SystemClock)
}

pub fn approve_source_with_terminal_and_clock(
    request: ApproveSourceRequest,
    terminal: &mut dyn ApprovalTerminal,
    clock: &dyn Clock,
) -> Result<ApproveSourceResult, MkoError> {
    approve_source_with_terminal_clock_and_observer(request, terminal, clock, &mut NoopObserver)
}

pub fn approve_source_with_terminal_clock_and_observer(
    request: ApproveSourceRequest,
    terminal: &mut dyn ApprovalTerminal,
    clock: &dyn Clock,
    observer: &mut dyn ApprovalObserver,
) -> Result<ApproveSourceResult, MkoError> {
    if !terminal.stdin_is_terminal() || !terminal.stdout_is_terminal() {
        return Err(confirmation_error());
    }
    let (held_lock, locked) = prepare_locked_publication(
        &request.repository_root,
        &request.source_id,
        "mko human approve-source",
        &EmptyGitSnapshotProvider,
        clock,
    )?;
    let input = std::str::from_utf8(&locked.source_snapshot.bytes)
        .map_err(|_| MkoError::new("source_unreadable", "Source must be UTF-8"))?;
    let line_count = input.lines().count();
    let git_summary = git_diff_summary(
        &locked.repository_root,
        &locked.source_path,
        input.len(),
        line_count,
    );
    terminal
        .write_all(&format!(
            "Source ID: {}\nCurrent revision: {}\nStatus: review_pending -> approved\nSource bytes: {}\nSource lines: {}\nGit diff: {}\nType exactly: APPROVE {}\n> ",
            locked.source.id,
            locked.revision,
            input.len(),
            line_count,
            git_summary,
            locked.source.id
        ))
        .map_err(terminal_error)?;
    let mut confirmation = String::new();
    terminal
        .read_line(&mut confirmation)
        .map_err(terminal_error)?;
    let confirmation = confirmation.trim_end_matches(['\r', '\n']);
    if confirmation != format!("APPROVE {}", locked.source.id) {
        return Err(confirmation_error());
    }
    publish_approved_source_under_lock(
        locked,
        &held_lock,
        &EmptyGitSnapshotProvider,
        clock,
        observer,
    )
}

pub(crate) fn prepare_locked_publication(
    repository_root: &Path,
    source_id: &str,
    command: &str,
    git: &dyn GitSnapshotProvider,
    clock: &dyn Clock,
) -> Result<(AssetLock, LockedPublicationRequest), MkoError> {
    validate_source_id(source_id)?;
    let repository_root = fs::canonicalize(repository_root).map_err(|error| {
        MkoError::new(
            "repository_root_invalid",
            format!("cannot resolve repository: {error}"),
        )
    })?;
    let repository = open_repository(&repository_root)?;
    let initial_sources = open_sources_directory(&repository)?;
    let initial = discover_source(&initial_sources, source_id)?;
    let asset_id = single_asset_id(&initial.record)?;
    let held_lock = AssetLock::acquire(&repository_root, &asset_id, command, clock, false)?;
    let sources = open_sources_directory(&repository)?;
    let discovered = discover_source(&sources, source_id)?;
    let asset_id_after_lock = single_asset_id(&discovered.record)?;
    if asset_id_after_lock != asset_id {
        return Err(MkoError::new(
            "relation_mismatch",
            "Source relation changed while acquiring its Asset lock",
        ));
    }
    let source_filename = discovered.filename;
    let source_path = format!("sources/{source_filename}");
    let asset_path = format!("assets/registry/{asset_id}.md");
    let input = std::str::from_utf8(&discovered.snapshot.bytes)
        .map_err(|_| MkoError::new("source_unreadable", "Source must be UTF-8"))?;
    let parsed = parse_markdown::<SourceRecord>(input)?;
    let source = parsed.metadata;
    if source.status != SourceStatus::ReviewPending || source.review.status != ReviewStatus::Pending
    {
        return Err(MkoError::new(
            "source_not_approvable",
            "only a pending Source can be approved",
        ));
    }
    let registry = open_registry_directory(&repository)?;
    let asset_filename = format!("{asset_id}.md");
    let asset_bytes = read_regular_capability_file(
        &registry,
        Path::new(&asset_filename),
        MAX_SOURCE_BYTES,
        "registry_unreadable",
    )?;
    let asset_text = std::str::from_utf8(&asset_bytes)
        .map_err(|_| MkoError::new("registry_unreadable", "Asset must be UTF-8"))?;
    let asset_document = parse_markdown::<crate::model::AssetRecord>(asset_text)?;
    let asset = asset_document.metadata;
    if asset.id != asset_id {
        return Err(MkoError::new(
            "registry_conflict",
            "registry filename and Asset ID disagree",
        ));
    }
    let revision = validate_canonical_source(&source_path, &source, &parsed.body, &asset)?;
    if asset.asset_status != AssetStatus::ReviewPending {
        return Err(MkoError::new(
            "source_state_mismatch",
            format!(
                "repair the Asset first: mko source repair-state --repo \"{}\" --asset-id {asset_id}",
                repository_root.display()
            ),
        ));
    }
    let git_snapshot = stable_git_snapshot(
        git,
        &repository_root,
        Path::new(&source_path),
        Path::new(&asset_path),
    )?;
    let working_digest = bytes_digest(&git_snapshot.working);
    let staged_digest = bytes_digest(&git_snapshot.staged);
    Ok((
        held_lock,
        LockedPublicationRequest {
            repository_root,
            repository,
            sources,
            registry,
            source_filename,
            source_path,
            asset_path,
            asset_filename,
            source,
            source_body: parsed.body,
            source_snapshot: discovered.snapshot,
            asset,
            asset_bytes,
            asset_body: asset_document.body,
            revision,
            git_snapshot,
            working_digest,
            staged_digest,
        },
    ))
}

pub(crate) fn publish_approved_source_under_lock(
    mut request: LockedPublicationRequest,
    held_lock: &AssetLock,
    git: &dyn GitSnapshotProvider,
    clock: &dyn Clock,
    observer: &mut dyn ApprovalObserver,
) -> Result<ApproveSourceResult, MkoError> {
    held_lock.assert_owned_for(&request.repository_root, &request.asset.id)?;
    let source_id = request.source.id.clone();
    let revision = request.revision.clone();
    request.source.status = SourceStatus::Approved;
    request.source.content_revision = revision.clone();
    request.source.review.status = ReviewStatus::Approved;
    request.source.review.approved_revision = Some(revision.clone());
    request.source.review.reviewed_at = Some(clock.now_utc());
    request.source.updated_at = clock.now_utc();
    let document = render_markdown(&request.source, &request.source_body)?;

    write_replace_capability_validated_at_commit(
        &request.sources,
        Path::new(&request.source_filename),
        document.as_bytes(),
        || observer.before_publication().map_err(terminal_error),
        || validate_locked_snapshot(&request, git),
    )
    .map_err(map_publication_error)?;

    let public_sources =
        open_sources_directory(&request.repository).map_err(|_| source_changed_error())?;
    let published = read_source_snapshot(&public_sources, Path::new(&request.source_filename))
        .map_err(|_| source_changed_error())?;
    if published.bytes != document.as_bytes() {
        return Err(source_changed_error());
    }
    let published_text =
        std::str::from_utf8(&published.bytes).map_err(|_| source_changed_error())?;
    let published =
        parse_markdown::<SourceRecord>(published_text).map_err(|_| source_changed_error())?;
    let public_revision = validate_canonical_source(
        &request.source_path,
        &published.metadata,
        &published.body,
        &request.asset,
    )
    .map_err(|_| source_changed_error())?;
    if published.metadata.id != source_id
        || published.metadata.status != SourceStatus::Approved
        || published.metadata.review.status != ReviewStatus::Approved
        || public_revision != revision
    {
        return Err(source_changed_error());
    }

    // Source publication is authoritative first. The Asset transition is a
    // capability-relative expected-byte CAS. Failure leaves the approved Source
    // and pending Asset in the existing, explicitly repairable mismatch state.
    let mut processed_asset = request.asset.clone();
    transition_asset(
        &mut processed_asset,
        AssetStatus::Processed,
        clock.now_utc(),
    )?;
    let processed_document = render_markdown(&processed_asset, &request.asset_body)?;
    write_replace_capability_validated_at_commit(
        &request.registry,
        Path::new(&request.asset_filename),
        processed_document.as_bytes(),
        || Ok(()),
        || {
            let current = read_regular_capability_file(
                &request.registry,
                Path::new(&request.asset_filename),
                MAX_SOURCE_BYTES,
                "registry_unreadable",
            )
            .map_err(|_| asset_changed_error())?;
            if current == request.asset_bytes {
                Ok(())
            } else {
                Err(asset_changed_error())
            }
        },
    )?;
    Ok(ApproveSourceResult {
        source_id,
        revision,
        source_path: request.source_path,
    })
}

fn validate_locked_snapshot(
    request: &LockedPublicationRequest,
    git: &dyn GitSnapshotProvider,
) -> Result<(), MkoError> {
    revalidate_expected_snapshot(
        &request.sources,
        Path::new(&request.source_filename),
        &request.source_snapshot,
        &request.source_path,
        &request.asset,
        &request.source.id,
        &request.revision,
    )?;
    let asset_bytes = read_regular_capability_file(
        &request.registry,
        Path::new(&request.asset_filename),
        MAX_SOURCE_BYTES,
        "registry_unreadable",
    )
    .map_err(|_| asset_changed_error())?;
    if asset_bytes != request.asset_bytes {
        return Err(asset_changed_error());
    }
    let current = stable_git_snapshot(
        git,
        &request.repository_root,
        Path::new(&request.source_path),
        Path::new(&request.asset_path),
    )?;
    if current != request.git_snapshot
        || bytes_digest(&current.working) != request.working_digest
        || bytes_digest(&current.staged) != request.staged_digest
    {
        return Err(git_changed_error());
    }
    Ok(())
}

fn stable_git_snapshot(
    git: &dyn GitSnapshotProvider,
    repository_root: &Path,
    source_path: &Path,
    asset_path: &Path,
) -> Result<GitSnapshot, MkoError> {
    let first = git.snapshot(repository_root, source_path, asset_path)?;
    let second = git.snapshot(repository_root, source_path, asset_path)?;
    if first == second {
        Ok(first)
    } else {
        Err(MkoError::new(
            "git_snapshot_unstable",
            "Git changed while the review snapshot was being collected",
        ))
    }
}

fn single_asset_id(source: &SourceRecord) -> Result<String, MkoError> {
    source
        .relations
        .asset_ids
        .first()
        .filter(|_| source.relations.asset_ids.len() == 1)
        .cloned()
        .ok_or_else(|| MkoError::new("relation_missing", "Source must relate to one Asset"))
}

fn bytes_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn map_publication_error(error: MkoError) -> MkoError {
    if error.code() == "registry_destination_invalid" {
        source_changed_error()
    } else {
        error
    }
}

fn asset_changed_error() -> MkoError {
    MkoError::new(
        "asset_changed_during_approval",
        "Asset changed after it was presented for approval; nothing was published",
    )
}

fn git_changed_error() -> MkoError {
    MkoError::new(
        "git_snapshot_changed_during_approval",
        "Git diff changed after it was presented for approval; nothing was published",
    )
}

struct NoopObserver;

impl ApprovalObserver for NoopObserver {
    fn before_publication(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct SystemApprovalTerminal;

impl ApprovalTerminal for SystemApprovalTerminal {
    fn stdin_is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn stdout_is_terminal(&self) -> bool {
        io::stdout().is_terminal()
    }

    fn write_all(&mut self, text: &str) -> io::Result<()> {
        let mut output = io::stdout().lock();
        output.write_all(text.as_bytes())?;
        output.flush()
    }

    fn read_line(&mut self, output: &mut String) -> io::Result<usize> {
        io::stdin().read_line(output)
    }
}

struct DiscoveredSource {
    record: SourceRecord,
    filename: String,
    snapshot: SourceSnapshot,
}

fn discover_source(directory: &Dir, source_id: &str) -> Result<DiscoveredSource, MkoError> {
    let mut filenames = Vec::new();
    for entry in directory.entries().map_err(|_| source_scan_error())? {
        if filenames.len() >= MAX_SOURCE_ENTRIES {
            return Err(source_scan_error());
        }
        let entry = entry.map_err(|_| source_scan_error())?;
        let filename = entry
            .file_name()
            .into_string()
            .map_err(|_| source_scan_error())?;
        if filename.len() > MAX_SOURCE_FILENAME_BYTES
            || filename.is_empty()
            || filename == "."
            || filename == ".."
            || filename.contains(['/', '\\'])
        {
            return Err(source_scan_error());
        }
        let file_type = entry.file_type().map_err(|_| source_scan_error())?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(source_scan_error());
        }
        filenames.push(filename);
    }
    filenames.sort_unstable();

    let mut matches = Vec::new();
    let mut scanned_bytes = 0u64;
    for filename in filenames {
        if Path::new(&filename)
            .extension()
            .and_then(|value| value.to_str())
            != Some("md")
        {
            continue;
        }
        let snapshot = read_source_snapshot(directory, Path::new(&filename))
            .map_err(|_| source_scan_error())?;
        scanned_bytes = scanned_bytes
            .checked_add(snapshot.bytes.len() as u64)
            .filter(|total| *total <= MAX_SOURCE_SCAN_BYTES)
            .ok_or_else(source_scan_error)?;
        let input = std::str::from_utf8(&snapshot.bytes).map_err(|_| source_scan_error())?;
        let document = parse_markdown::<SourceRecord>(input).map_err(|_| source_scan_error())?;
        if document.metadata.id == source_id {
            matches.push(DiscoveredSource {
                record: document.metadata,
                filename,
                snapshot,
            });
        }
    }
    match matches.len() {
        0 => Err(MkoError::new(
            "source_not_found",
            "canonical Source was not found",
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(MkoError::new(
            "source_conflict",
            "multiple Sources use the same ID",
        )),
    }
}

fn source_scan_error() -> MkoError {
    MkoError::new(
        "source_scan_limit",
        "Source discovery exceeded a bounded or regular-file input limit",
    )
}

#[derive(Clone)]
pub(crate) struct SourceSnapshot {
    pub(crate) bytes: Vec<u8>,
    len: u64,
    modified: Option<SystemTime>,
}

fn open_repository(repository_root: &Path) -> Result<Dir, MkoError> {
    Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|error| MkoError::new("source_path_invalid", error.to_string()))
}

fn open_sources_directory(repository: &Dir) -> Result<Dir, MkoError> {
    let metadata = repository
        .symlink_metadata("sources")
        .map_err(|error| MkoError::new("source_path_invalid", error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MkoError::new(
            "source_path_invalid",
            "sources must be a retained real directory",
        ));
    }
    repository
        .open_dir("sources")
        .map_err(|error| MkoError::new("source_path_invalid", error.to_string()))
}

fn open_registry_directory(repository: &Dir) -> Result<Dir, MkoError> {
    let assets = repository
        .symlink_metadata("assets")
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
    if assets.file_type().is_symlink() || !assets.is_dir() {
        return Err(MkoError::new(
            "registry_unreadable",
            "assets must be a retained real directory",
        ));
    }
    let assets = repository
        .open_dir("assets")
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
    let registry = assets
        .symlink_metadata("registry")
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))?;
    if registry.file_type().is_symlink() || !registry.is_dir() {
        return Err(MkoError::new(
            "registry_unreadable",
            "registry must be a retained real directory",
        ));
    }
    assets
        .open_dir("registry")
        .map_err(|error| MkoError::new("registry_unreadable", error.to_string()))
}

fn read_regular_capability_file(
    directory: &Dir,
    filename: &Path,
    max_bytes: u64,
    error_code: &str,
) -> Result<Vec<u8>, MkoError> {
    let path_metadata = directory
        .symlink_metadata(filename)
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(MkoError::new(
            error_code,
            "record must remain a regular file",
        ));
    }
    let mut file = directory
        .open(filename)
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(MkoError::new(error_code, "record exceeds its byte limit"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(error_code, error.to_string()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(MkoError::new(error_code, "record exceeds its byte limit"));
    }
    Ok(bytes)
}

fn read_source_snapshot(directory: &Dir, filename: &Path) -> Result<SourceSnapshot, MkoError> {
    let path_metadata = directory
        .symlink_metadata(filename)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(MkoError::new(
            "source_path_invalid",
            "Source must remain a regular file in the retained sources directory",
        ));
    }
    let mut file = directory
        .open(filename)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(MkoError::new(
            "source_too_large",
            "Source is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(MkoError::new(
            "source_too_large",
            "Source exceeds the 2 MiB approval limit",
        ));
    }
    Ok(SourceSnapshot {
        bytes,
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn revalidate_expected_snapshot(
    directory: &Dir,
    filename: &Path,
    expected: &SourceSnapshot,
    source_path: &str,
    asset: &crate::model::AssetRecord,
    expected_source_id: &str,
    expected_revision: &str,
) -> Result<(), MkoError> {
    let current = read_source_snapshot(directory, filename).map_err(|_| source_changed_error())?;
    if current.bytes != expected.bytes
        || current.len != expected.len
        || current.modified != expected.modified
    {
        return Err(source_changed_error());
    }
    let text = std::str::from_utf8(&current.bytes).map_err(|_| source_changed_error())?;
    let parsed = parse_markdown::<SourceRecord>(text).map_err(|_| source_changed_error())?;
    let revision = validate_canonical_source(source_path, &parsed.metadata, &parsed.body, asset)
        .map_err(|_| source_changed_error())?;
    if parsed.metadata.id != expected_source_id || revision != expected_revision {
        return Err(source_changed_error());
    }
    Ok(())
}

fn validate_source_id(source_id: &str) -> Result<(), MkoError> {
    let hash = source_id.strip_prefix("personal-source-");
    if hash.is_none_or(|hash| {
        hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }) {
        return Err(MkoError::new(
            "source_id_invalid",
            "invalid content-addressed Source ID",
        ));
    }
    Ok(())
}

fn confirmation_error() -> MkoError {
    MkoError::new(
        "human_confirmation_required",
        "approval requires interactive stdin/stdout and the exact confirmation phrase",
    )
}

fn terminal_error(error: io::Error) -> MkoError {
    MkoError::new("human_confirmation_required", error.to_string())
}

fn source_changed_error() -> MkoError {
    MkoError::new(
        "source_changed_during_approval",
        "Source changed after it was presented for approval; nothing was overwritten",
    )
}

fn git_diff_summary(
    repository_root: &Path,
    source_path: &str,
    byte_count: usize,
    line_count: usize,
) -> String {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            source_path,
        ])
        .output();
    let Ok(status) = status else {
        return "unavailable".into();
    };
    if !status.status.success() || status.stdout.len() > 64 * 1024 {
        return "unavailable".into();
    }
    if String::from_utf8_lossy(&status.stdout)
        .lines()
        .any(|line| line.starts_with("?? "))
    {
        return format!("untracked (+{line_count} lines, +{byte_count} bytes)");
    }

    let working = git_numstat(repository_root, source_path, false);
    let staged = git_numstat(repository_root, source_path, true);
    match (working, staged) {
        (Some((working_added, working_deleted)), Some((staged_added, staged_deleted))) => format!(
            "tracked (working +{working_added}/-{working_deleted} lines; staged +{staged_added}/-{staged_deleted} lines; {byte_count} current bytes)"
        ),
        _ => "unavailable".into(),
    }
}

fn git_numstat(repository_root: &Path, source_path: &str, staged: bool) -> Option<(u64, u64)> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repository_root).arg("diff");
    if staged {
        command.arg("--cached");
    }
    let output = command
        .args(["--numstat", "--", source_path])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut added = 0u64;
    let mut deleted = 0u64;
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(left) = fields.next() else { continue };
        let Some(right) = fields.next() else { continue };
        if let (Ok(left), Ok(right)) = (left.parse::<u64>(), right.parse::<u64>()) {
            added = added.saturating_add(left);
            deleted = deleted.saturating_add(right);
        }
    }
    Some((added, deleted))
}
