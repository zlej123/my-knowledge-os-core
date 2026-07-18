use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use cap_std::{ambient_authority, fs::Dir};
use chrono_tz::Asia::Seoul;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::{
    CORE_VERSION,
    error::MkoError,
    front_matter::parse_markdown,
    hooks::PRE_COMMIT_SCRIPT,
    model::{
        AssetRecord, AssetStatus, Classification, LastSuccessfulStep, ReviewStatus, SourceRecord,
        SourceStatus,
    },
    pdf::{EXTRACTOR_NAME, EXTRACTOR_VERSION},
    prepare::{PROCESSOR_VERSION, PROMPT_VERSION},
    revision::calculate_source_revision,
    secret,
    state::transition_allowed,
};

const MAX_CHECK_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHECK_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CHECK_FILES: usize = 20_000;
const MAX_GIT_LIST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CheckRequest {
    repository_root: PathBuf,
    staged: bool,
}

impl CheckRequest {
    pub fn new(repository_root: impl AsRef<Path>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            staged: false,
        }
    }

    pub fn with_staged(mut self, staged: bool) -> Self {
        self.staged = staged;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckIssue {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_action: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub result: String,
    pub issues: Vec<CheckIssue>,
}

impl CheckReport {
    pub fn has_code(&self, code: &str) -> bool {
        self.issues.iter().any(|issue| issue.code == code)
    }

    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }
}

#[derive(Clone, Debug)]
struct RepositoryFile {
    path: String,
    bytes: Vec<u8>,
}

pub fn check_repository(request: CheckRequest) -> Result<CheckReport, MkoError> {
    let repository_root = fs::canonicalize(&request.repository_root).map_err(|error| {
        MkoError::new(
            "repository_root_invalid",
            format!("cannot resolve repository root: {error}"),
        )
    })?;
    if !repository_root.is_dir() {
        return Err(MkoError::new(
            "repository_root_invalid",
            "repository root must be a directory",
        ));
    }

    let (mut files, mut issues) = if request.staged {
        staged_files(&repository_root)?
    } else {
        working_tree_files(&repository_root)?
    };
    files.sort_by(|left, right| left.path.cmp(&right.path));
    inspect_files(&repository_root, &files, &mut issues);
    inspect_locks(&repository_root, &mut issues);
    inspect_hook(&repository_root, &files, &mut issues);
    sort_and_deduplicate(&mut issues);
    Ok(CheckReport {
        result: if issues.is_empty() {
            "ok".into()
        } else {
            "failed".into()
        },
        issues,
    })
}

fn inspect_files(repository_root: &Path, files: &[RepositoryFile], issues: &mut Vec<CheckIssue>) {
    let mut assets = BTreeMap::<String, (String, AssetRecord)>::new();
    let mut sources = Vec::<(String, SourceRecord, String)>::new();
    let mut collision_keys = BTreeMap::<String, String>::new();

    for file in files {
        let collision_key = file.path.nfc().collect::<String>().to_lowercase();
        if let Some(other) = collision_keys.insert(collision_key, file.path.clone()) {
            issues.push(issue(
                "path_collision",
                Some(&file.path),
                None,
                "repository paths collide after case and Unicode normalization",
                Some(format!("rename either {other} or {}", file.path)),
            ));
        }
        for finding in secret::scan(Path::new(&file.path), &file.bytes) {
            issues.push(issue(
                "secret_detected",
                Some(&file.path),
                Some(&finding.rule),
                "content matches a protected credential rule; the value is redacted",
                None,
            ));
        }
        if has_conflict_marker(&file.bytes) {
            issues.push(issue(
                "git_conflict",
                Some(&file.path),
                None,
                "file contains Git conflict markers",
                Some("resolve the conflict and stage the resolved file".into()),
            ));
        }
        if file.path.starts_with("assets/registry/") && file.path.ends_with(".md") {
            match parse_utf8_markdown::<AssetRecord>(file) {
                Ok((asset, _)) => {
                    validate_asset(&file.path, &asset, issues);
                    if let Some((other, _)) =
                        assets.insert(asset.id.clone(), (file.path.clone(), asset))
                    {
                        issues.push(issue(
                            "duplicate_conflict",
                            Some(&file.path),
                            None,
                            "multiple Registry files declare the same Asset ID",
                            Some(format!(
                                "compare with {other} and keep only the canonical record"
                            )),
                        ));
                    }
                }
                Err(error) => issues.push(parse_issue(&file.path, error)),
            }
        } else if file.path.starts_with("sources/") && file.path.ends_with(".md") {
            match parse_utf8_markdown::<SourceRecord>(file) {
                Ok((source, body)) => sources.push((file.path.clone(), source, body)),
                Err(error) => issues.push(parse_issue(&file.path, error)),
            }
        }
    }

    let mut fingerprints = BTreeMap::<String, String>::new();
    for (id, (path, asset)) in &assets {
        if let Some(other) = fingerprints.insert(asset.fingerprint.value.clone(), id.clone())
            && other != *id
        {
            issues.push(issue(
                "duplicate_conflict",
                Some(path),
                None,
                "different Asset IDs declare the same fingerprint",
                Some(format!(
                    "use the canonical content-addressed Asset ID instead of {id}"
                )),
            ));
        }
        if let Some(old) = asset.supersedes.as_deref()
            && !assets.contains_key(old)
        {
            issues.push(issue(
                "relation_missing",
                Some(path),
                None,
                "Asset supersedes a Registry record that is absent",
                None,
            ));
        }
    }

    let mut source_assets = BTreeMap::<String, String>::new();
    for (path, source, body) in &sources {
        validate_source(repository_root, path, source, body, &assets, issues);
        if let Some(asset_id) = source.relations.asset_ids.first()
            && let Some(other) = source_assets.insert(asset_id.clone(), path.clone())
        {
            issues.push(issue(
                "relation_conflict",
                Some(path),
                None,
                "more than one canonical Source relates to the same Asset",
                Some(format!(
                    "compare with {other} and keep one canonical Source"
                )),
            ));
        }
    }

    for (asset_id, (path, asset)) in &assets {
        if asset.asset_status == AssetStatus::Changed
            && assets
                .values()
                .any(|(_, successor)| successor.supersedes.as_deref() == Some(asset_id))
        {
            issues.push(issue(
                "lineage_repair_needed",
                Some(path),
                None,
                "an authoritative successor exists but the old Asset is still changed",
                Some(format!(
                    "mko asset repair-lineage --repo \"{}\" --asset-id {asset_id}",
                    repository_root.display()
                )),
            ));
        }
        let related = sources
            .iter()
            .find(|(_, source, _)| source.relations.asset_ids.as_slice() == [asset_id.as_str()]);
        if asset.asset_status == AssetStatus::ReviewPending {
            let valid = related.is_some_and(|(_, source, body)| {
                calculate_source_revision(source, body).is_ok_and(|actual| {
                    source.status == SourceStatus::ReviewPending
                        && source.review.status == ReviewStatus::Pending
                        && source.content_revision == actual
                })
            });
            if !valid {
                issues.push(issue(
                    "relation_missing",
                    Some(path),
                    None,
                    "review_pending Asset does not have one current pending Source",
                    Some("restore or regenerate the canonical pending Source".into()),
                ));
            }
        }
        if asset.asset_status == AssetStatus::Processed {
            let valid = related.is_some_and(|(_, source, body)| {
                let actual = calculate_source_revision(source, body).ok();
                source.status == SourceStatus::Approved
                    && source.review.status == ReviewStatus::Approved
                    && source.review.approved_revision.as_ref() == actual.as_ref()
            });
            if !valid {
                issues.push(issue(
                    "source_state_mismatch",
                    Some(path),
                    None,
                    "processed Asset does not have one current approved Source",
                    Some(repair_action(repository_root, asset_id)),
                ));
            }
        }
    }
}

fn validate_asset(path: &str, asset: &AssetRecord, issues: &mut Vec<CheckIssue>) {
    let hash = valid_prefixed_hash(&asset.id, "personal-asset-");
    let expected_path = format!("assets/registry/{}.md", asset.id);
    if hash.is_none()
        || path != expected_path
        || asset.record_type != "asset"
        || asset.schema_version != 1
        || asset.scope != "personal"
        || asset.classification != Classification::Personal
        || asset.asset_class != "document"
        || asset.media_type != "application/pdf"
        || asset.provider.r#type != "google-drive-stream"
        || asset.fingerprint.method != "sha256"
        || hash.is_some_and(|hash| asset.fingerprint.value != format!("sha256:{hash}"))
    {
        issues.push(issue(
            "registry_invalid",
            Some(path),
            None,
            "Asset identity, Scope, schema, path, or fingerprint is not canonical",
            None,
        ));
    }
    if !portable_relative_path(&asset.provider.locator) {
        issues.push(issue(
            "path_not_portable",
            Some(path),
            None,
            "Provider locator must be a portable relative path",
            None,
        ));
    }
    if matches!(
        asset.asset_status,
        AssetStatus::Changed | AssetStatus::Missing | AssetStatus::Failed
    ) && asset.durable_state_history.is_empty()
    {
        issues.push(issue(
            "invalid_state_transition",
            Some(path),
            None,
            "recoverable Asset state has no durable checkpoint",
            None,
        ));
    }
    for pair in asset.durable_state_history.windows(2) {
        if !transition_allowed(pair[0].clone(), pair[1].clone()) {
            issues.push(issue(
                "invalid_state_transition",
                Some(path),
                None,
                "durable state history contains a forbidden transition",
                None,
            ));
            break;
        }
    }
    let step_matches = match asset.asset_status {
        AssetStatus::Registered => asset.last_successful_step == LastSuccessfulStep::Registered,
        AssetStatus::Extracted => asset.last_successful_step == LastSuccessfulStep::Extracted,
        AssetStatus::ReviewPending => asset.last_successful_step == LastSuccessfulStep::Drafted,
        AssetStatus::Processed => asset.last_successful_step == LastSuccessfulStep::Reviewed,
        _ => true,
    };
    if !step_matches {
        issues.push(issue(
            "invalid_state_transition",
            Some(path),
            None,
            "Asset state disagrees with its last successful step",
            None,
        ));
    }
}

fn validate_source(
    repository_root: &Path,
    path: &str,
    source: &SourceRecord,
    body: &str,
    assets: &BTreeMap<String, (String, AssetRecord)>,
    issues: &mut Vec<CheckIssue>,
) {
    let source_hash = valid_prefixed_hash(&source.id, "personal-source-");
    if source_hash.is_none()
        || source.record_type != "source"
        || source.schema_version != 1
        || source.scope != "personal"
        || source.relations.asset_ids.len() != 1
        || !portable_relative_path(path)
        || !canonical_source_path(path, source)
        || source.generation.extractor_name != EXTRACTOR_NAME
        || source.generation.extractor_version != EXTRACTOR_VERSION
        || source.generation.core_version != CORE_VERSION
        || source.generation.processor_version != PROCESSOR_VERSION
        || source.generation.prompt_version != PROMPT_VERSION
    {
        issues.push(issue(
            "source_invalid",
            Some(path),
            None,
            "Source identity, Scope, schema, path, or canonical relation is invalid",
            None,
        ));
    }
    let actual = match calculate_source_revision(source, body) {
        Ok(revision) => revision,
        Err(error) => {
            issues.push(parse_issue(path, error));
            return;
        }
    };
    if source.content_revision != actual {
        issues.push(issue(
            "revision_mismatch",
            Some(path),
            None,
            "stored content_revision does not match recomputed semantic content",
            Some("review the semantic diff before approving the current revision".into()),
        ));
    }
    if source.review.status == ReviewStatus::Approved
        && source.review.approved_revision.as_deref() != Some(actual.as_str())
    {
        issues.push(issue(
            "approval_stale",
            Some(path),
            None,
            "approval is not bound to the recomputed current revision",
            Some("run the human-only approval command after reviewing the current diff".into()),
        ));
    }
    let review_valid = match source.status {
        SourceStatus::ReviewPending => {
            source.review.status == ReviewStatus::Pending
                && source.review.approved_revision.is_none()
                && source.review.reviewed_at.is_none()
        }
        SourceStatus::Approved => {
            source.review.status == ReviewStatus::Approved
                && source.review.reviewed_at.is_some()
                && source.review.approved_revision.as_deref() == Some(actual.as_str())
        }
        SourceStatus::Rejected => {
            source.review.status == ReviewStatus::Rejected
                && source.review.approved_revision.is_none()
                && source.review.reviewed_at.is_some()
        }
        SourceStatus::Stale | SourceStatus::Archived => match source.review.status {
            ReviewStatus::Pending => {
                source.review.approved_revision.is_none() && source.review.reviewed_at.is_none()
            }
            ReviewStatus::Approved => {
                source.review.approved_revision.is_some() && source.review.reviewed_at.is_some()
            }
            ReviewStatus::Rejected => {
                source.review.approved_revision.is_none() && source.review.reviewed_at.is_some()
            }
        },
    };
    if !review_valid {
        issues.push(issue(
            "review_invalid",
            Some(path),
            None,
            "Source status and review metadata are inconsistent",
            None,
        ));
    }

    if let Some(asset_id) = source.relations.asset_ids.first() {
        let Some((_, asset)) = assets.get(asset_id) else {
            issues.push(issue(
                "relation_missing",
                Some(path),
                None,
                "Source relation targets an absent Asset Registry record",
                None,
            ));
            return;
        };
        if source.id != asset.id.replacen("asset", "source", 1)
            || source.generation.asset_fingerprint != asset.fingerprint.value
        {
            issues.push(issue(
                "relation_mismatch",
                Some(path),
                None,
                "Source ID or generation fingerprint disagrees with its Asset",
                None,
            ));
        }
        let expected = match source.status {
            SourceStatus::ReviewPending => Some(AssetStatus::ReviewPending),
            SourceStatus::Approved => Some(AssetStatus::Processed),
            _ => None,
        };
        if expected
            .as_ref()
            .is_some_and(|state| *state != asset.asset_status)
        {
            issues.push(issue(
                "source_state_mismatch",
                Some(path),
                None,
                "Source publication state and Asset durable state disagree",
                Some(repair_action(repository_root, asset_id)),
            ));
        }
    }
}

fn working_tree_files(root: &Path) -> Result<(Vec<RepositoryFile>, Vec<CheckIssue>), MkoError> {
    let mut files = Vec::new();
    let mut issues = Vec::new();
    let mut total = 0u64;
    let directory = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
    collect_directory(&directory, "", &mut files, &mut issues, &mut total)?;
    Ok((files, issues))
}

fn collect_directory(
    directory: &Dir,
    prefix: &str,
    files: &mut Vec<RepositoryFile>,
    issues: &mut Vec<CheckIssue>,
    total: &mut u64,
) -> Result<(), MkoError> {
    let mut entries = directory
        .entries()
        .map_err(|error| MkoError::new("check_failed", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_CHECK_FILES {
            issues.push(limit_issue(
                None,
                "repository file count exceeds the check limit",
            ));
            return Ok(());
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            issues.push(issue(
                "path_not_portable",
                None,
                None,
                "repository path is not UTF-8",
                None,
            ));
            continue;
        };
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if !portable_relative_path(&relative) {
            issues.push(issue(
                "path_not_portable",
                Some(&relative),
                None,
                "repository path is not portable",
                None,
            ));
            continue;
        }
        if relative == ".git"
            || relative.starts_with(".git/")
            || relative == "target"
            || relative.starts_with("target/")
            || relative == ".knowledge-os/runtime"
            || relative.starts_with(".knowledge-os/runtime/")
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
        if file_type.is_symlink() {
            issues.push(issue(
                "symlink_not_allowed",
                Some(&relative),
                None,
                "repository checks do not follow symbolic links",
                Some("replace the symlink with a regular repository file or directory".into()),
            ));
        } else if file_type.is_dir() {
            let child = entry
                .open_dir()
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
            collect_directory(&child, &relative, files, issues, total)?;
        } else if file_type.is_file() {
            let file = entry
                .open()
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
            let metadata = file
                .metadata()
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
            if metadata.len() > MAX_CHECK_FILE_BYTES {
                issues.push(limit_issue(
                    Some(&relative),
                    "file exceeds the 2 MiB check limit",
                ));
                continue;
            }
            if total.saturating_add(metadata.len()) > MAX_CHECK_TOTAL_BYTES {
                issues.push(limit_issue(
                    Some(&relative),
                    "repository input exceeds the 32 MiB aggregate check limit",
                ));
                continue;
            }
            let mut bytes = Vec::new();
            file.take(MAX_CHECK_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| MkoError::new("check_failed", error.to_string()))?;
            if bytes.len() as u64 > MAX_CHECK_FILE_BYTES {
                issues.push(limit_issue(
                    Some(&relative),
                    "file changed beyond the 2 MiB check limit",
                ));
                continue;
            }
            *total += bytes.len() as u64;
            files.push(RepositoryFile {
                path: relative,
                bytes,
            });
        }
    }
    Ok(())
}

fn staged_files(root: &Path) -> Result<(Vec<RepositoryFile>, Vec<CheckIssue>), MkoError> {
    let output = run_git_bounded(root, &["ls-files", "--stage", "-z"], MAX_GIT_LIST_BYTES)?;
    let mut files = Vec::new();
    let mut issues = Vec::new();
    let mut total = 0u64;
    let mut seen = BTreeSet::new();
    for entry in output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        if files.len() >= MAX_CHECK_FILES {
            issues.push(limit_issue(
                None,
                "Git index file count exceeds the check limit",
            ));
            break;
        }
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            issues.push(issue(
                "git_index_invalid",
                None,
                None,
                "malformed Git index entry",
                None,
            ));
            continue;
        };
        let header = String::from_utf8_lossy(&entry[..tab]);
        let mut fields = header.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let _oid = fields.next().unwrap_or_default();
        let stage = fields.next().unwrap_or_default();
        let Ok(path) = std::str::from_utf8(&entry[tab + 1..]) else {
            issues.push(issue(
                "path_not_portable",
                None,
                None,
                "Git index path is not UTF-8",
                None,
            ));
            continue;
        };
        if !portable_relative_path(path) {
            issues.push(issue(
                "path_not_portable",
                Some(path),
                None,
                "Git index path is not portable",
                None,
            ));
            continue;
        }
        if stage != "0" {
            issues.push(issue(
                "git_conflict",
                Some(path),
                None,
                "Git index contains an unmerged entry",
                Some("resolve the index conflict before committing".into()),
            ));
            continue;
        }
        if !seen.insert(path.to_owned()) {
            continue;
        }
        if mode == "120000" {
            issues.push(issue(
                "symlink_not_allowed",
                Some(path),
                None,
                "staged symbolic links are not allowed",
                None,
            ));
            continue;
        }
        if mode != "100644" && mode != "100755" {
            issues.push(issue(
                "path_not_portable",
                Some(path),
                None,
                "unsupported staged file mode",
                None,
            ));
            continue;
        }
        let object = format!(":0:{path}");
        let size_output = run_git_bounded(root, &["cat-file", "-s", &object], 64)?;
        let size = String::from_utf8_lossy(&size_output)
            .trim()
            .parse::<u64>()
            .map_err(|_| MkoError::new("git_index_invalid", "invalid staged blob size"))?;
        if size > MAX_CHECK_FILE_BYTES {
            issues.push(limit_issue(
                Some(path),
                "staged file exceeds the 2 MiB check limit",
            ));
            continue;
        }
        if total.saturating_add(size) > MAX_CHECK_TOTAL_BYTES {
            issues.push(limit_issue(
                Some(path),
                "staged input exceeds the 32 MiB aggregate check limit",
            ));
            continue;
        }
        let bytes = run_git_bounded(root, &["cat-file", "blob", &object], size + 1)?;
        total += bytes.len() as u64;
        files.push(RepositoryFile {
            path: path.into(),
            bytes,
        });
    }
    Ok((files, issues))
}

fn run_git_bounded(root: &Path, arguments: &[&str], limit: u64) -> Result<Vec<u8>, MkoError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| MkoError::new("git_unavailable", "Git stdout was not captured"))?
        .take(limit + 1)
        .read_to_end(&mut output)
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    let status = child
        .wait()
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    if !status.success() {
        return Err(MkoError::new(
            "git_index_invalid",
            "cannot read the staged Git index",
        ));
    }
    if output.len() as u64 > limit {
        return Err(MkoError::new(
            "check_input_too_large",
            "Git command output exceeds its bounded transport",
        ));
    }
    Ok(output)
}

fn inspect_locks(repository_root: &Path, issues: &mut Vec<CheckIssue>) {
    let locks = repository_root.join(".knowledge-os/runtime/locks");
    let Ok(metadata) = fs::symlink_metadata(&locks) else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        issues.push(issue(
            "runtime_path_invalid",
            Some(".knowledge-os/runtime/locks"),
            None,
            "runtime lock path is not a real directory",
            None,
        ));
        return;
    }
    if let Ok(entries) = fs::read_dir(locks) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".lock") || name.ends_with(".lock.takeover") {
                issues.push(issue(
                    "lock_held",
                    Some(&format!(".knowledge-os/runtime/locks/{}", name)),
                    None,
                    "an Asset operation lock exists",
                    Some("verify the owner process before using --clear-stale-lock".into()),
                ));
            }
        }
    }
}

fn inspect_hook(repository_root: &Path, files: &[RepositoryFile], issues: &mut Vec<CheckIssue>) {
    let hook = files
        .iter()
        .find(|file| file.path == ".githooks/pre-commit");
    if hook.is_none_or(|file| file.bytes != PRE_COMMIT_SCRIPT.as_bytes()) {
        issues.push(issue(
            "hook_missing",
            Some(".githooks/pre-commit"),
            None,
            "managed pre-commit hook is absent or differs from the v0.1 script",
            Some(format!(
                "mko hooks install --repo \"{}\"",
                repository_root.display()
            )),
        ));
    }
    let configured = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|value| value.trim() == ".githooks");
    if !configured {
        issues.push(issue(
            "hook_not_configured",
            Some(".git/config"),
            None,
            "Git core.hooksPath is not configured as .githooks",
            Some(format!(
                "mko hooks install --repo \"{}\"",
                repository_root.display()
            )),
        ));
    }
}

fn parse_utf8_markdown<T>(file: &RepositoryFile) -> Result<(T, String), MkoError>
where
    T: serde::de::DeserializeOwned,
{
    let input = std::str::from_utf8(&file.bytes)
        .map_err(|_| MkoError::new("schema_invalid", "record must be UTF-8"))?;
    let parsed = parse_markdown::<T>(input)?;
    Ok((parsed.metadata, parsed.body))
}

fn valid_prefixed_hash<'a>(id: &'a str, prefix: &str) -> Option<&'a str> {
    id.strip_prefix(prefix).filter(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn canonical_source_path(path: &str, source: &SourceRecord) -> bool {
    let Some(hash) = valid_prefixed_hash(&source.id, "personal-source-") else {
        return false;
    };
    let Some(filename) = path.strip_prefix("sources/") else {
        return false;
    };
    if filename.contains('/') {
        return false;
    }
    let date = source
        .created_at
        .with_timezone(&Seoul)
        .date_naive()
        .to_string();
    let prefix = format!("{date}-");
    let suffix = format!("-{}.md", &hash[..12]);
    let Some(slug) = filename
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(&suffix))
    else {
        return false;
    };
    !slug.is_empty()
        && slug.len() <= 96
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn portable_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && !path.contains('\\')
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn has_conflict_marker(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().any(|line| {
        line.starts_with("<<<<<<< ") || line == "=======" || line.starts_with(">>>>>>> ")
    })
}

fn parse_issue(path: &str, error: MkoError) -> CheckIssue {
    issue(error.code(), Some(path), None, error.message(), None)
}

fn limit_issue(path: Option<&str>, message: &str) -> CheckIssue {
    issue("check_input_too_large", path, None, message, None)
}

fn repair_action(repository_root: &Path, asset_id: &str) -> String {
    format!(
        "mko source repair-state --repo \"{}\" --asset-id {asset_id}",
        repository_root.display()
    )
}

fn issue(
    code: impl Into<String>,
    path: Option<&str>,
    rule: Option<&str>,
    message: impl Into<String>,
    safe_action: Option<String>,
) -> CheckIssue {
    CheckIssue {
        code: code.into(),
        path: path.map(str::to_owned),
        rule: rule.map(str::to_owned),
        message: message.into(),
        safe_action,
    }
}

fn sort_and_deduplicate(issues: &mut Vec<CheckIssue>) {
    issues.sort_by(|left, right| {
        (&left.code, &left.path, &left.rule, &left.message).cmp(&(
            &right.code,
            &right.path,
            &right.rule,
            &right.message,
        ))
    });
    issues.dedup();
}
