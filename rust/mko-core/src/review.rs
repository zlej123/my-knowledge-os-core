use std::{
    io::{self, IsTerminal, Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    approve::{
        ApprovalObserver, ApproveSourceResult, GitSnapshot, GitSnapshotProvider,
        prepare_locked_publication, publish_approved_source_under_lock,
    },
    clock::Clock,
    error::MkoError,
    front_matter::parse_markdown,
    model::{ReviewStatus, SourceRecord, SourceStatus},
};
use cap_std::{ambient_authority, fs::Dir};

const MAX_SOURCE_ENTRIES: usize = 1024;
const MAX_SOURCE_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSelection {
    pub source_id: String,
    pub title: String,
    pub source_path: String,
    pub revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSnapshot {
    pub selection: ReviewSelection,
    pub source_bytes: Vec<u8>,
    pub asset_bytes: Vec<u8>,
    pub working_diff: Vec<u8>,
    pub staged_diff: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewOutcome {
    Deferred,
    Approved(ApproveSourceResult),
}

pub trait ReviewTerminal {
    fn stdin_is_terminal(&self) -> bool;
    fn stdout_is_terminal(&self) -> bool;
    fn write_all(&mut self, text: &str) -> io::Result<()>;
    fn read_line(&mut self, output: &mut String) -> io::Result<usize>;
}

pub fn review(repository_root: &Path) -> Result<ReviewOutcome, MkoError> {
    review_and_approve(
        repository_root,
        &mut SystemReviewTerminal,
        &SystemGitSnapshotProvider,
        &crate::clock::SystemClock,
        &mut NoopReviewObserver,
    )
}

pub fn list_pending_sources(repository_root: &Path) -> Result<Vec<ReviewSelection>, MkoError> {
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
    let sources = repository
        .open_dir("sources")
        .map_err(|error| MkoError::new("source_path_invalid", error.to_string()))?;
    let mut filenames = sources
        .entries()
        .map_err(|_| source_scan_error())?
        .take(MAX_SOURCE_ENTRIES + 1)
        .map(|entry| {
            let entry = entry.map_err(|_| source_scan_error())?;
            let file_type = entry.file_type().map_err(|_| source_scan_error())?;
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(source_scan_error());
            }
            entry
                .file_name()
                .into_string()
                .map_err(|_| source_scan_error())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if filenames.len() > MAX_SOURCE_ENTRIES {
        return Err(source_scan_error());
    }
    filenames.sort();
    let mut total = 0u64;
    let mut pending = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    for filename in filenames {
        if Path::new(&filename)
            .extension()
            .and_then(|value| value.to_str())
            != Some("md")
        {
            continue;
        }
        let metadata = sources
            .symlink_metadata(&filename)
            .map_err(|_| source_scan_error())?;
        total = total
            .checked_add(metadata.len())
            .filter(|total| *total <= MAX_SOURCE_SCAN_BYTES)
            .ok_or_else(source_scan_error)?;
        let mut file = sources.open(&filename).map_err(|_| source_scan_error())?;
        let opened = file.metadata().map_err(|_| source_scan_error())?;
        if !opened.is_file() || opened.len() != metadata.len() {
            return Err(source_scan_error());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(metadata.len() + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| source_scan_error())?;
        if bytes.len() as u64 != metadata.len() {
            return Err(source_scan_error());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| source_scan_error())?;
        let source = parse_markdown::<SourceRecord>(text)
            .map_err(|_| source_scan_error())?
            .metadata;
        if !seen_ids.insert(source.id.clone()) {
            return Err(MkoError::new(
                "source_conflict",
                "multiple Sources use the same ID",
            ));
        }
        if source.status == SourceStatus::ReviewPending
            && source.review.status == ReviewStatus::Pending
        {
            pending.push(ReviewSelection {
                source_id: source.id,
                title: source.title,
                source_path: format!("sources/{filename}"),
                revision: source.content_revision,
            });
        }
    }
    pending.sort_by(|left, right| {
        (&left.title, &left.source_id, &left.source_path).cmp(&(
            &right.title,
            &right.source_id,
            &right.source_path,
        ))
    });
    Ok(pending)
}

pub fn review_and_approve(
    repository_root: &Path,
    terminal: &mut dyn ReviewTerminal,
    git: &dyn GitSnapshotProvider,
    clock: &dyn Clock,
    observer: &mut dyn ApprovalObserver,
) -> Result<ReviewOutcome, MkoError> {
    if !terminal.stdin_is_terminal() || !terminal.stdout_is_terminal() {
        return Err(confirmation_error());
    }
    let pending = list_pending_sources(repository_root)?;
    if pending.is_empty() {
        return Err(MkoError::new(
            "source_not_found",
            "no pending Source is available for review",
        ));
    }
    terminal
        .write_all("검토 대기 Source:\n")
        .map_err(terminal_error)?;
    for (index, source) in pending.iter().enumerate() {
        terminal
            .write_all(&format!(
                "{}. {} | {} | {} | {}\n",
                index + 1,
                escape_terminal_text(&source.title),
                source.source_id,
                escape_terminal_text(&source.source_path),
                source.revision,
            ))
            .map_err(terminal_error)?;
    }
    terminal
        .write_all("검토할 번호를 입력하세요:\n> ")
        .map_err(terminal_error)?;
    let selected_index = read_trimmed_line(terminal)?
        .parse::<usize>()
        .ok()
        .filter(|index| (1..=pending.len()).contains(index))
        .ok_or_else(|| MkoError::new("review_selection_invalid", "목록의 번호를 입력하세요"))?;
    let listed = pending
        .get(selected_index - 1)
        .ok_or_else(|| MkoError::new("review_selection_invalid", "목록의 번호를 입력하세요"))?;

    let (held_lock, locked) =
        prepare_locked_publication(repository_root, &listed.source_id, "mko review", git, clock)?;
    let snapshot = ReviewSnapshot {
        selection: ReviewSelection {
            source_id: locked.source.id.clone(),
            title: locked.source.title.clone(),
            source_path: locked.source_path.clone(),
            revision: locked.revision.clone(),
        },
        source_bytes: locked.source_snapshot.bytes.clone(),
        asset_bytes: locked.asset_bytes.clone(),
        working_diff: locked.git_snapshot.working.clone(),
        staged_diff: locked.git_snapshot.staged.clone(),
    };
    display_snapshot(terminal, &snapshot)?;
    let confirmation = read_trimmed_line(terminal)?;
    if confirmation == "DEFER" {
        return Ok(ReviewOutcome::Deferred);
    }
    let exact = format!(
        "APPROVE {} {}",
        snapshot.selection.source_id, snapshot.selection.revision
    );
    if confirmation != exact {
        return Err(confirmation_error());
    }
    publish_approved_source_under_lock(locked, &held_lock, git, clock, observer)
        .map(ReviewOutcome::Approved)
}

fn display_snapshot(
    terminal: &mut dyn ReviewTerminal,
    snapshot: &ReviewSnapshot,
) -> Result<(), MkoError> {
    let source = std::str::from_utf8(&snapshot.source_bytes)
        .map_err(|_| MkoError::new("source_unreadable", "Source must be UTF-8"))?;
    let asset = std::str::from_utf8(&snapshot.asset_bytes)
        .map_err(|_| MkoError::new("registry_unreadable", "Asset must be UTF-8"))?;
    let working = std::str::from_utf8(&snapshot.working_diff)
        .map_err(|_| git_error("Git working diff is not UTF-8"))?;
    let staged = std::str::from_utf8(&snapshot.staged_diff)
        .map_err(|_| git_error("Git staged diff is not UTF-8"))?;
    terminal
        .write_all(&format!(
            "\n=== SOURCE {} {} ===\n{}\n=== ASSET ===\n{}\n=== 작업 변경분 ===\n{}\n=== 스테이지 변경분 ===\n{}\n보류하려면 DEFER, 승인하려면 정확히 입력하세요: APPROVE {} {}\n> ",
            snapshot.selection.source_id,
            snapshot.selection.revision,
            escape_terminal_text(source),
            escape_terminal_text(asset),
            escape_terminal_text(working),
            escape_terminal_text(staged),
            snapshot.selection.source_id,
            snapshot.selection.revision,
        ))
        .map_err(terminal_error)
}

fn read_trimmed_line(terminal: &mut dyn ReviewTerminal) -> Result<String, MkoError> {
    let mut line = String::new();
    terminal.read_line(&mut line).map_err(terminal_error)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

pub struct SystemGitSnapshotProvider;

impl GitSnapshotProvider for SystemGitSnapshotProvider {
    fn snapshot(
        &self,
        repository_root: &Path,
        source_path: &Path,
        asset_path: &Path,
    ) -> Result<GitSnapshot, MkoError> {
        validate_git_path(source_path)?;
        validate_git_path(asset_path)?;
        let mut budget = MAX_GIT_OUTPUT_BYTES;
        let unmerged = run_git(
            repository_root,
            &["ls-files", "-u", "--"],
            &[source_path, asset_path],
            &mut budget,
        )?;
        if !unmerged.is_empty() {
            return Err(git_error("review paths have an unmerged Git state"));
        }
        let working = run_git(
            repository_root,
            &["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--"],
            &[source_path, asset_path],
            &mut budget,
        )?;
        let staged = run_git(
            repository_root,
            &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
            ],
            &[source_path, asset_path],
            &mut budget,
        )?;
        Ok(GitSnapshot { working, staged })
    }
}

fn run_git(
    repository_root: &Path,
    fixed_args: &[&str],
    paths: &[&Path],
    budget: &mut usize,
) -> Result<Vec<u8>, MkoError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository_root)
        .args(fixed_args)
        .args(paths)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| git_error("Git could not be started"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| git_error("Git output unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| git_error("Git output unavailable"))?;
    let limit = *budget;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| git_error("Git status unavailable"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(git_error("Git review snapshot timed out"));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| git_error("Git output unavailable"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| git_error("Git output unavailable"))??;
    let consumed = stdout
        .len()
        .checked_add(stderr.len())
        .filter(|size| *size <= *budget)
        .ok_or_else(|| git_error("Git review output exceeded its limit"))?;
    *budget -= consumed;
    std::str::from_utf8(&stdout).map_err(|_| git_error("Git output is not UTF-8"))?;
    std::str::from_utf8(&stderr).map_err(|_| git_error("Git output is not UTF-8"))?;
    if !status.success() {
        return Err(git_error("Git review command failed"));
    }
    Ok(stdout)
}

fn read_bounded(mut reader: impl Read, max: usize) -> Result<Vec<u8>, MkoError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| git_error("Git output unavailable"))?;
    if bytes.len() > max {
        return Err(git_error("Git review output exceeded its limit"));
    }
    Ok(bytes)
}

fn validate_git_path(path: &Path) -> Result<(), MkoError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(git_error("review path is not repository-relative"));
    }
    path.to_str()
        .ok_or_else(|| git_error("review path is not UTF-8"))?;
    Ok(())
}

fn escape_terminal_text(input: &str) -> String {
    let mut output = String::new();
    for character in input.chars() {
        let unsafe_format = character.is_control() && !matches!(character, '\n' | '\t')
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            );
        if unsafe_format {
            output.extend(character.escape_default());
        } else {
            output.push(character);
        }
    }
    output
}

fn source_scan_error() -> MkoError {
    MkoError::new(
        "source_scan_limit",
        "Source discovery exceeded a bounded or regular-file input limit",
    )
}

fn confirmation_error() -> MkoError {
    MkoError::new(
        "human_confirmation_required",
        "review requires interactive stdin/stdout and the exact approval token",
    )
}

fn terminal_error(error: io::Error) -> MkoError {
    MkoError::new("human_confirmation_required", error.to_string())
}

fn git_error(message: &str) -> MkoError {
    MkoError::new("git_snapshot_unavailable", message)
}

struct SystemReviewTerminal;

impl ReviewTerminal for SystemReviewTerminal {
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

struct NoopReviewObserver;

impl ApprovalObserver for NoopReviewObserver {
    fn before_publication(&mut self) -> io::Result<()> {
        Ok(())
    }
}
