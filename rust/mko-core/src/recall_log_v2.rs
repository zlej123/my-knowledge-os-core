use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
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
};

const RECALL_LOG_RELATIVE_PATH: &str = "logs/recall.jsonl";
/// Bounds the read for `mko home`'s aggregate: a tail read, not a full-file
/// read, so a log that has grown over months still answers in bounded time
/// and never crashes the home screen (§5, D7).
const MAX_TAIL_READ_BYTES: u64 = 8 * 1024 * 1024;
pub const RECALL_METRICS_WINDOW_DAYS: i64 = 30;

/// Which path ran the search. D7's success measure is whether the *agent*
/// recalled before answering; the owner typing a term into the terminal or
/// the web UI is a search too, but it is not evidence that the recall
/// contract fired. Every line says which one it was, so the two are never
/// added together (§5, 2026-09-03 note).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallViaV2 {
    /// `mko find --recall`: the agent's recall-contract search.
    Agent,
    /// Plain `mko find` or the home menu's `지식 찾기`: the owner searching
    /// by hand. Also the reading for lines written before `via` existed —
    /// nothing in those lines can tell the two apart, so they are counted as
    /// the weaker claim rather than inflating the agent figure.
    #[default]
    Owner,
    /// The web UI's `/api/search`.
    Web,
}

impl RecallViaV2 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Owner => "owner",
            Self::Web => "web",
        }
    }
}

/// One line of `logs/recall.jsonl`. Field name is `surfaced`, not `cited`:
/// the Core knows only what search returned, never whether the agent's
/// answer actually cited it — that would need a second round-trip this
/// phase does not add (§5, D7). `via` (added 2026-09-03) says which path
/// searched; a line without it predates the marker and reads as `owner`.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecallLogEntryV2 {
    at: DateTime<Utc>,
    query: String,
    results: u64,
    surfaced: Vec<String>,
    #[serde(default)]
    via: RecallViaV2,
}

/// Recent-window aggregate. The `agent_*` figures are the headline: they
/// count only `via: agent` lines, because only those say the recall
/// contract fired. Owner and web searches are reported beside them, never
/// folded in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecallMetricsV2 {
    pub window_days: u32,
    /// Agent recalls (`mko find --recall`) in the window.
    pub agent_recall_count: u64,
    /// Agent recalls that returned nothing.
    pub agent_zero_result_count: u64,
    /// Records and notes search returned to the agent, summed over the
    /// window. Search results, not citations: the Core never learns what
    /// the answer used.
    pub agent_surfaced_total: u64,
    /// Plain `mko find` / home-menu searches, and every pre-marker line.
    pub owner_search_count: u64,
    /// Web UI `/api/search` calls.
    pub web_search_count: u64,
}

/// Appends one recall event. Holds the repository mutation lock for the
/// duration of the append (find becomes a mutating command, same as queue
/// or dashboard repair) and surfaces a lock conflict as an ordinary
/// `MkoError` the standard json-v2 error path already handles.
///
/// Callers must treat a failure here as non-fatal to the search results
/// themselves (§5): recall must not be lost to a logging error, but a
/// logging error must not swallow a result the owner is waiting on either.
pub fn append_recall_log_v2(
    repository_root: &Path,
    query: &str,
    results: u64,
    surfaced: &[String],
    via: RecallViaV2,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 recall log append",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let directory = repository_root.join("logs");
    ensure_real_directory(&directory)?;
    let path = directory.join("recall.jsonl");
    let entry = RecallLogEntryV2 {
        at: clock.now_utc(),
        query: query.to_owned(),
        results,
        surfaced: surfaced.to_vec(),
        via,
    };
    let mut line = serde_json::to_vec(&entry)
        .map_err(|error| MkoError::new("recall_log_invalid", error.to_string()))?;
    line.push(b'\n');

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    configure_nofollow(&mut options);
    let mut file = options
        .open(&path)
        .map_err(|error| MkoError::new("recall_log_write_failed", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("recall_log_write_failed", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "recall_log_invalid",
            "logs/recall.jsonl must be a regular non-symlink file",
        ));
    }
    file.write_all(&line)
        .map_err(|error| MkoError::new("recall_log_write_failed", error.to_string()))?;
    file.flush()
        .map_err(|error| MkoError::new("recall_log_write_failed", error.to_string()))
}

/// Recent-window recall metrics for `mko home` (§5, D7): how many agent
/// recalls happened, how many came back empty, how many results they
/// returned, and — separately — how often the owner or the web UI searched,
/// over the last `RECALL_METRICS_WINDOW_DAYS` days. A single bounded tail
/// read; malformed or partial lines are skipped rather than failing the
/// whole aggregate, because a corrupt log line must never take down the
/// home screen.
pub fn recall_metrics_v2(
    repository_root: &Path,
    now: DateTime<Utc>,
) -> Result<RecallMetricsV2, MkoError> {
    let path = repository_root.join(RECALL_LOG_RELATIVE_PATH);
    let mut metrics = RecallMetricsV2 {
        window_days: RECALL_METRICS_WINDOW_DAYS as u32,
        ..Default::default()
    };
    let text = match read_tail(&path, MAX_TAIL_READ_BYTES) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(metrics),
        Err(_) => return Ok(metrics),
    };
    let cutoff = now - Duration::days(RECALL_METRICS_WINDOW_DAYS);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<RecallLogEntryV2>(line) else {
            continue;
        };
        if entry.at < cutoff || entry.at > now {
            continue;
        }
        match entry.via {
            RecallViaV2::Agent => {
                metrics.agent_recall_count += 1;
                if entry.results == 0 {
                    metrics.agent_zero_result_count += 1;
                }
                metrics.agent_surfaced_total += entry.surfaced.len() as u64;
            }
            RecallViaV2::Owner => metrics.owner_search_count += 1,
            RecallViaV2::Web => metrics.web_search_count += 1,
        }
    }
    Ok(metrics)
}

/// Reads up to `max_bytes` from the end of the file, then drops whatever
/// partial line the seek landed inside so every remaining line is complete.
/// `Ok(None)` means no log exists yet, which is the ordinary state before
/// the first `mko find`.
fn read_tail(path: &Path, max_bytes: u64) -> Result<Option<String>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MkoError::new("recall_log_unreadable", error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("recall_log_unreadable", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "recall_log_invalid",
            "logs/recall.jsonl must be a regular non-symlink file",
        ));
    }
    let length = metadata.len();
    let start = length.saturating_sub(max_bytes);
    if start > 0 {
        file.seek(SeekFrom::Start(start))
            .map_err(|error| MkoError::new("recall_log_unreadable", error.to_string()))?;
    }
    let mut bytes = Vec::with_capacity((length - start).min(max_bytes) as usize);
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("recall_log_unreadable", error.to_string()))?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        // The seek almost certainly landed mid-line; drop that partial
        // prefix rather than risk parsing a truncated JSON object.
        text = match text.find('\n') {
            Some(index) => text[index + 1..].to_owned(),
            None => String::new(),
        };
    }
    Ok(Some(text))
}

fn ensure_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| MkoError::new("recall_log_path_invalid", error.to_string()))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(MkoError::new(
                    "recall_log_path_invalid",
                    "logs must be a real directory",
                ))
            }
        }
        Err(error) => Err(MkoError::new("recall_log_write_failed", error.to_string())),
    }
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_NONBLOCK: i32 = 0x800;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NOFOLLOW: i32 = 0x100;
    const O_NONBLOCK: i32 = 0x4;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clock::Clock, scaffold_v2::scaffold_personal_kb_v2};
    use tempfile::tempdir;

    #[derive(Clone, Copy)]
    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.0
        }
    }

    fn clock(rfc3339: &str) -> FixedClock {
        FixedClock(DateTime::parse_from_rfc3339(rfc3339).unwrap().into())
    }

    #[test]
    fn append_writes_one_line_per_query_and_metrics_aggregate_the_window() {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();

        append_recall_log_v2(
            root.path(),
            "학습률 개선",
            2,
            &[
                "personal-knowledge-aaaa".into(),
                "personal-source-bbbb".into(),
            ],
            RecallViaV2::Agent,
            &clock("2026-08-01T00:00:00Z"),
        )
        .unwrap();
        append_recall_log_v2(
            root.path(),
            "no matches at all",
            0,
            &[],
            RecallViaV2::Agent,
            &clock("2026-08-02T00:00:00Z"),
        )
        .unwrap();
        // Outside the 30-day window as of "now" below.
        append_recall_log_v2(
            root.path(),
            "ancient query",
            5,
            &["personal-knowledge-old".into()],
            RecallViaV2::Agent,
            &clock("2026-01-01T00:00:00Z"),
        )
        .unwrap();

        let text = fs::read_to_string(root.path().join("logs/recall.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 3);
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["via"], "agent");

        let metrics = recall_metrics_v2(
            root.path(),
            DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
                .unwrap()
                .into(),
        )
        .unwrap();
        assert_eq!(metrics.agent_recall_count, 2);
        assert_eq!(metrics.agent_zero_result_count, 1);
        assert_eq!(metrics.agent_surfaced_total, 2);
        assert_eq!(metrics.owner_search_count, 0);
        assert_eq!(metrics.web_search_count, 0);
    }

    // The headline is agent recalls only: an owner typing into the terminal
    // or the web UI is a search, not evidence that the recall contract
    // fired. Each path is written with its own marker and counted apart.
    #[test]
    fn metrics_count_agent_owner_and_web_searches_separately() {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();

        append_recall_log_v2(
            root.path(),
            "agent hit",
            1,
            &["personal-knowledge-aaaa".into()],
            RecallViaV2::Agent,
            &clock("2026-08-01T00:00:00Z"),
        )
        .unwrap();
        append_recall_log_v2(
            root.path(),
            "owner miss",
            0,
            &[],
            RecallViaV2::Owner,
            &clock("2026-08-01T01:00:00Z"),
        )
        .unwrap();
        append_recall_log_v2(
            root.path(),
            "owner hit",
            3,
            &["a".into(), "b".into(), "c".into()],
            RecallViaV2::Owner,
            &clock("2026-08-01T02:00:00Z"),
        )
        .unwrap();
        append_recall_log_v2(
            root.path(),
            "web miss",
            0,
            &[],
            RecallViaV2::Web,
            &clock("2026-08-01T03:00:00Z"),
        )
        .unwrap();

        let text = fs::read_to_string(root.path().join("logs/recall.jsonl")).unwrap();
        let vias = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["via"].clone())
            .collect::<Vec<_>>();
        assert_eq!(vias, vec!["agent", "owner", "owner", "web"]);

        let metrics = recall_metrics_v2(
            root.path(),
            DateTime::parse_from_rfc3339("2026-08-02T00:00:00Z")
                .unwrap()
                .into(),
        )
        .unwrap();
        assert_eq!(metrics.agent_recall_count, 1);
        assert_eq!(metrics.agent_zero_result_count, 0);
        assert_eq!(metrics.agent_surfaced_total, 1);
        // Owner and web misses and results never leak into the agent figures.
        assert_eq!(metrics.owner_search_count, 2);
        assert_eq!(metrics.web_search_count, 1);
    }

    // Lines written before `via` existed carry no marker. They were written
    // by every `mko find`, owner and agent alike, so nothing in them supports
    // the stronger claim: they read as owner searches.
    #[test]
    fn lines_without_via_parse_as_owner_searches() {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();
        fs::create_dir_all(root.path().join("logs")).unwrap();
        fs::write(
            root.path().join("logs/recall.jsonl"),
            concat!(
                "{\"at\":\"2026-08-01T00:00:00Z\",\"query\":\"legacy hit\",\"results\":2,",
                "\"surfaced\":[\"personal-knowledge-aaaa\",\"personal-source-bbbb\"]}\n",
                "{\"at\":\"2026-08-01T01:00:00Z\",\"query\":\"legacy miss\",\"results\":0,",
                "\"surfaced\":[]}\n",
            ),
        )
        .unwrap();

        let metrics = recall_metrics_v2(
            root.path(),
            DateTime::parse_from_rfc3339("2026-08-02T00:00:00Z")
                .unwrap()
                .into(),
        )
        .unwrap();
        assert_eq!(metrics.owner_search_count, 2);
        assert_eq!(metrics.agent_recall_count, 0);
        assert_eq!(metrics.agent_zero_result_count, 0);
        assert_eq!(metrics.agent_surfaced_total, 0);
        assert_eq!(metrics.web_search_count, 0);
    }

    #[test]
    fn metrics_survive_a_corrupt_line_and_a_missing_log() {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();

        let missing = recall_metrics_v2(root.path(), Utc::now()).unwrap();
        assert_eq!(missing.agent_recall_count, 0);
        assert_eq!(missing.owner_search_count, 0);

        append_recall_log_v2(
            root.path(),
            "valid",
            1,
            &["personal-knowledge-aaaa".into()],
            RecallViaV2::Agent,
            &clock("2026-08-01T00:00:00Z"),
        )
        .unwrap();
        let log_path = root.path().join("logs/recall.jsonl");
        let mut existing = fs::read_to_string(&log_path).unwrap();
        existing.push_str("not json at all\n");
        existing.push_str("{\"at\":\"2026-08-02T00:00:00Z\"}\n");
        // An unknown marker is a corrupt line too, not a silent owner search.
        existing.push_str(
            "{\"at\":\"2026-08-02T00:00:00Z\",\"query\":\"x\",\"results\":0,\"surfaced\":[],\"via\":\"robot\"}\n",
        );
        fs::write(&log_path, existing).unwrap();

        let metrics = recall_metrics_v2(
            root.path(),
            DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
                .unwrap()
                .into(),
        )
        .unwrap();
        assert_eq!(metrics.agent_recall_count, 1);
        assert_eq!(metrics.owner_search_count, 0);
        assert_eq!(metrics.web_search_count, 0);
    }
}
