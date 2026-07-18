use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

use crate::{
    atomic::write_replace,
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::AssetLock,
    model::{AssetStatus, ReviewStatus, SourceRecord, SourceStatus},
    registry::{mark_asset_processed_with_clock, read_asset},
    revision::calculate_source_revision,
};

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

pub fn approve_source(request: ApproveSourceRequest) -> Result<ApproveSourceResult, MkoError> {
    let mut terminal = SystemApprovalTerminal;
    approve_source_with_terminal_and_clock(request, &mut terminal, &SystemClock)
}

pub fn approve_source_with_terminal_and_clock(
    request: ApproveSourceRequest,
    terminal: &mut dyn ApprovalTerminal,
    clock: &dyn Clock,
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
    let input = read_bounded(&path)?;
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
    if source.id != asset.id.replacen("asset", "source", 1)
        || source.generation.asset_fingerprint != asset.fingerprint.value
    {
        return Err(MkoError::new(
            "relation_mismatch",
            "Source identity or fingerprint disagrees with its Asset",
        ));
    }
    if asset.asset_status != AssetStatus::ReviewPending {
        return Err(MkoError::new(
            "source_state_mismatch",
            format!(
                "repair the Asset first: mko source repair-state --repo \"{}\" --asset-id {asset_id}",
                repository_root.display()
            ),
        ));
    }
    let revision = calculate_source_revision(&source, &parsed.body)?;
    terminal
        .write_all(&format!(
            "Source ID: {}\nCurrent revision: {}\nReview state: pending -> approved\nType exactly: APPROVE {}\n> ",
            source.id, revision, source.id
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

    source.status = SourceStatus::Approved;
    source.content_revision = revision.clone();
    source.review.status = ReviewStatus::Approved;
    source.review.approved_revision = Some(revision.clone());
    source.review.reviewed_at = Some(clock.now_utc());
    source.updated_at = clock.now_utc();
    ensure_regular_file(&path)?;
    let document = render_markdown(&source, &parsed.body)?;
    write_replace(&path, document.as_bytes())?;

    // Source publication is the first authoritative write. If the Asset transition fails,
    // `mko check` reports a repairable source_state_mismatch without losing the approval.
    mark_asset_processed_with_clock(&repository_root, &asset_id, clock)?;
    Ok(ApproveSourceResult {
        source_id: source.id,
        revision,
        source_path: repository_relative(&repository_root, &path)?,
    })
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
