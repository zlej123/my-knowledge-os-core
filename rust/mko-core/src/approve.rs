use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    atomic::write_replace_checked,
    canonical_source::validate_canonical_source,
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::AssetLock,
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    registry::{mark_asset_processed_with_clock, read_asset},
};
use cap_std::{ambient_authority, fs::Dir, time::SystemTime};

const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

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
    validate_source_id(&request.source_id)?;
    let repository_root = fs::canonicalize(&request.repository_root).map_err(|error| {
        MkoError::new(
            "repository_root_invalid",
            format!("cannot resolve repository: {error}"),
        )
    })?;
    let (initial, _) = find_source(&repository_root, &request.source_id)?;
    let asset_id = initial
        .relations
        .asset_ids
        .first()
        .filter(|_| initial.relations.asset_ids.len() == 1)
        .cloned()
        .ok_or_else(|| MkoError::new("relation_missing", "Source must relate to one Asset"))?;
    let _lock = AssetLock::acquire(
        &repository_root,
        &asset_id,
        "mko human approve-source",
        clock,
        false,
    )?;
    let (_, path) = find_source(&repository_root, &request.source_id)?;
    let source_filename = path
        .file_name()
        .ok_or_else(|| MkoError::new("source_path_invalid", "Source filename is missing"))?
        .to_owned();
    let sources = open_sources_directory(&repository_root)?;
    let expected = read_source_snapshot(&sources, Path::new(&source_filename))?;
    let input = std::str::from_utf8(&expected.bytes)
        .map_err(|_| MkoError::new("source_unreadable", "Source must be UTF-8"))?
        .to_owned();
    let parsed = parse_markdown::<SourceRecord>(&input)?;
    let mut source = parsed.metadata;
    if source.status != SourceStatus::ReviewPending || source.review.status != ReviewStatus::Pending
    {
        return Err(MkoError::new(
            "source_not_approvable",
            "only a pending Source can be approved",
        ));
    }
    if source.relations.asset_ids.as_slice() != [asset_id.as_str()] {
        return Err(MkoError::new(
            "relation_mismatch",
            "Source relation changed during approval",
        ));
    }
    let asset = read_asset(&repository_root, &asset_id)?;
    let source_path = repository_relative(&repository_root, &path)?;
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
    let line_count = input.lines().count();
    let git_summary = git_diff_summary(&repository_root, &source_path);
    terminal
        .write_all(&format!(
            "Source ID: {}\nCurrent revision: {}\nStatus: review_pending -> approved\nSource bytes: {}\nSource lines: {}\nGit diff: {}\nType exactly: APPROVE {}\n> ",
            source.id,
            revision,
            input.len(),
            line_count,
            git_summary,
            source.id
        ))
        .map_err(terminal_error)?;
    let mut confirmation = String::new();
    terminal
        .read_line(&mut confirmation)
        .map_err(terminal_error)?;
    let confirmation = confirmation.trim_end_matches(['\r', '\n']);
    if confirmation != format!("APPROVE {}", source.id) {
        return Err(confirmation_error());
    }
    revalidate_expected_snapshot(
        &sources,
        Path::new(&source_filename),
        &expected,
        &source_path,
        &asset,
        &source.id,
        &revision,
    )?;

    source.status = SourceStatus::Approved;
    source.content_revision = revision.clone();
    source.review.status = ReviewStatus::Approved;
    source.review.approved_revision = Some(revision.clone());
    source.review.reviewed_at = Some(clock.now_utc());
    source.updated_at = clock.now_utc();
    ensure_regular_file(&path)?;
    let document = render_markdown(&source, &parsed.body)?;
    observer.before_publication().map_err(terminal_error)?;
    write_replace_checked(&path, document.as_bytes(), || {
        revalidate_expected_snapshot(
            &sources,
            Path::new(&source_filename),
            &expected,
            &source_path,
            &asset,
            &source.id,
            &revision,
        )
    })
    .map_err(|error| {
        if error.code() == "registry_destination_invalid" {
            source_changed_error()
        } else {
            error
        }
    })?;

    let published = read_source_snapshot(&sources, Path::new(&source_filename))
        .map_err(|_| source_changed_error())?;
    if published.bytes != document.as_bytes() {
        return Err(source_changed_error());
    }
    let published_text =
        std::str::from_utf8(&published.bytes).map_err(|_| source_changed_error())?;
    let published = parse_markdown::<SourceRecord>(published_text)?;
    validate_canonical_source(&source_path, &published.metadata, &published.body, &asset)?;

    // Source publication is the first authoritative write. If the Asset transition fails,
    // `mko check` reports a repairable source_state_mismatch without losing the approval.
    mark_asset_processed_with_clock(&repository_root, &asset_id, clock)?;
    Ok(ApproveSourceResult {
        source_id: source.id,
        revision,
        source_path,
    })
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

fn find_source(
    repository_root: &Path,
    source_id: &str,
) -> Result<(SourceRecord, PathBuf), MkoError> {
    let sources = repository_root.join("sources");
    let metadata = fs::symlink_metadata(&sources)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "source_path_invalid",
            "sources must be a real directory",
        ));
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(&sources)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?
    {
        let entry = entry.map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        ensure_regular_file(&path)?;
        let input = read_bounded(&path)?;
        let document = parse_markdown::<SourceRecord>(&input)?;
        if document.metadata.id == source_id {
            matches.push((document.metadata, path));
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

fn read_bounded(path: &Path) -> Result<String, MkoError> {
    let metadata = fs::metadata(path)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(MkoError::new(
            "source_too_large",
            "Source exceeds the 2 MiB approval limit",
        ));
    }
    fs::read_to_string(path).map_err(|error| MkoError::new("source_unreadable", error.to_string()))
}

#[derive(Clone)]
struct SourceSnapshot {
    bytes: Vec<u8>,
    len: u64,
    modified: Option<SystemTime>,
}

fn open_sources_directory(repository_root: &Path) -> Result<Dir, MkoError> {
    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|error| MkoError::new("source_path_invalid", error.to_string()))?;
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

fn ensure_regular_file(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "source_path_invalid",
            "Source must be a regular file",
        ));
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

fn repository_relative(root: &Path, path: &Path) -> Result<String, MkoError> {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| MkoError::new("source_path_invalid", "Source path escaped repository"))
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

fn git_diff_summary(repository_root: &Path, source_path: &str) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["diff", "--numstat", "--", source_path])
        .output();
    let Ok(output) = output else {
        return "unavailable".into();
    };
    if !output.status.success() {
        return "unavailable".into();
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
    format!("+{added}/-{deleted} working-tree lines")
}
