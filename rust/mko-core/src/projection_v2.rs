use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
};

use cap_std::fs::Dir;
use serde::{Deserialize, Deserializer, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{
        AtomicWriteResult, write_new, write_replace_capability_compare_exchange_validated_at_commit,
    },
    clock::SystemClock,
    config_v2::PerspectiveV2,
    error::MkoError,
    front_matter::parse_markdown,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    revision_v2::{canonical_json_sha256, sha256_digest},
    safe_yaml::validate_yaml_input,
};

const MANIFEST_PATH: &str = ".mko/generated-manifest.yaml";
const MANIFEST_BYTE_LIMIT: u64 = 1024 * 1024;
const PROJECTION_BYTE_LIMIT: u64 = 8 * 1024 * 1024;
const RECOVERY_DIFF_BYTE_LIMIT: u64 = 80 * 1024 * 1024;

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionRecordTypeV2 {
    Source,
    Knowledge,
}

impl ProjectionRecordTypeV2 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Knowledge => "knowledge",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStateV2 {
    Unreviewed,
    Deferred,
    ChangesRequested,
    RevisedUnreviewed,
    Approved,
    Blocked,
}

impl ProjectionStateV2 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Deferred => "deferred",
            Self::ChangesRequested => "changes_requested",
            Self::RevisedUnreviewed => "revised_unreviewed",
            Self::Approved => "approved",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectionInputV2 {
    pub record_type: ProjectionRecordTypeV2,
    pub id: String,
    pub title: String,
    pub current_revision: String,
    pub review_head_id: Option<String>,
    pub derived_state: ProjectionStateV2,
    pub domain: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub perspectives: Vec<PerspectiveV2>,
    pub tags: Vec<String>,
    pub record_link: String,
    pub asset_link: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionInputWireV2 {
    record_type: ProjectionRecordTypeV2,
    id: String,
    title: String,
    current_revision: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    review_head_id: Option<String>,
    derived_state: ProjectionStateV2,
    domain: String,
    #[serde(default)]
    perspectives: Vec<PerspectiveV2>,
    tags: Vec<String>,
    record_link: String,
    asset_link: String,
}

impl<'de> Deserialize<'de> for ProjectionInputV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectionInputWireV2::deserialize(deserializer)?;
        let input = Self {
            record_type: wire.record_type,
            id: wire.id,
            title: wire.title,
            current_revision: wire.current_revision,
            review_head_id: wire.review_head_id,
            derived_state: wire.derived_state,
            domain: wire.domain,
            perspectives: wire.perspectives,
            tags: wire.tags,
            record_link: wire.record_link,
            asset_link: wire.asset_link,
        };
        validate_input(&input).map_err(serde::de::Error::custom)?;
        Ok(input)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedProjectionV2 {
    pub bytes: Vec<u8>,
    pub projection_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionWriteOutcomeV2 {
    Created,
    Existing,
    Updated,
    RepairRequired,
    Repaired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionRecoveryV2 {
    pub modified_digest: String,
    pub backup_path: PathBuf,
    pub expected_path: PathBuf,
    pub diff_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionWriteResultV2 {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub projection_digest: String,
    pub outcome: ProjectionWriteOutcomeV2,
    pub recovery: Option<ProjectionRecoveryV2>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionSnapshotStatusV2 {
    Missing,
    Current,
    Stale,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProjectionMetadataV2 {
    projection_schema_version: u32,
    record_type: ProjectionRecordTypeV2,
    record_id: String,
    title: String,
    current_revision: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    review_head_id: Option<String>,
    derived_state: ProjectionStateV2,
    domain: String,
    #[serde(default)]
    perspectives: Vec<PerspectiveV2>,
    tags: Vec<String>,
    record_link: String,
    asset_link: String,
    projection_digest: String,
}

pub(crate) fn projection_snapshot_status_v2(
    repository_root: &Path,
    expected: &ProjectionInputV2,
) -> Result<ProjectionSnapshotStatusV2, MkoError> {
    validate_input(expected)?;
    let rendered = render_projection_unchecked(expected)?;
    let path = repository_root.join(projection_relative_path_unchecked(expected));
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectionSnapshotStatusV2::Missing);
        }
        Err(error) => return Err(MkoError::new("projection_read_failed", error.to_string())),
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            return Ok(ProjectionSnapshotStatusV2::Stale);
        }
        Ok(_) => {}
    }
    let Ok(bytes) = read_regular_projection(&path) else {
        return Ok(ProjectionSnapshotStatusV2::Stale);
    };
    // Currentness is defined against bytes freshly rendered from canonical
    // record content plus authoritative Review events. Re-rendering values
    // parsed from this projection would let a self-consistent but semantically
    // false projection validate itself.
    if rendered.bytes == bytes {
        Ok(ProjectionSnapshotStatusV2::Current)
    } else {
        Ok(ProjectionSnapshotStatusV2::Stale)
    }
}

pub(crate) fn read_current_projection_input_v2(
    repository_root: &Path,
    record_type: ProjectionRecordTypeV2,
    record_id: &str,
) -> Result<ProjectionInputV2, MkoError> {
    let expected_prefix = format!("personal-{}-", record_type.as_str());
    validate_prefixed_hex(record_id, &expected_prefix, "record ID")?;
    let probe = ProjectionInputV2 {
        record_type,
        id: record_id.to_owned(),
        title: "projection probe".into(),
        current_revision: format!("sha256:{}", "0".repeat(64)),
        review_head_id: None,
        derived_state: ProjectionStateV2::Unreviewed,
        domain: "projection probe".into(),
        perspectives: Vec::new(),
        tags: Vec::new(),
        record_link: "projection-probe".into(),
        asset_link: "projection-probe".into(),
    };
    let path = repository_root.join(projection_relative_path_unchecked(&probe));
    if matches!(
        fs::symlink_metadata(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Err(MkoError::new(
            "projection_not_found",
            "the current generated projection is missing",
        ));
    }
    let bytes = read_regular_projection(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| projection_invalid("projection must be valid UTF-8"))?;
    let parsed = parse_markdown::<StoredProjectionMetadataV2>(text)
        .map_err(|error| projection_invalid(error.to_string()))?;
    let metadata = parsed.metadata;
    if metadata.projection_schema_version != 2
        || metadata.record_type != record_type
        || metadata.record_id != record_id
    {
        return Err(projection_invalid(
            "projection does not identify the requested record",
        ));
    }
    let input = ProjectionInputV2 {
        record_type: metadata.record_type,
        id: metadata.record_id,
        title: metadata.title,
        current_revision: metadata.current_revision,
        review_head_id: metadata.review_head_id,
        derived_state: metadata.derived_state,
        domain: metadata.domain,
        perspectives: metadata.perspectives,
        tags: metadata.tags,
        record_link: metadata.record_link,
        asset_link: metadata.asset_link,
    };
    let rendered = render_projection_v2(&input)?;
    if rendered.projection_digest != metadata.projection_digest || rendered.bytes != bytes {
        return Err(MkoError::new(
            "projection_snapshot_changed",
            "the current projection is stale or user-modified",
        ));
    }
    Ok(input)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GeneratedManifestV2 {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dashboard_files: Vec<GeneratedProjectionEntryV2>,
    projections: Vec<GeneratedProjectionEntryV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GeneratedProjectionEntryV2 {
    path: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    content_digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GeneratedFileEntryV2 {
    pub path: String,
    pub content_digest: Option<String>,
}

pub(crate) fn generated_dashboard_entries_v2(
    repository_root: &Path,
) -> Result<Vec<GeneratedFileEntryV2>, MkoError> {
    let path = repository_root.join(MANIFEST_PATH);
    if matches!(
        fs::symlink_metadata(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Ok(Vec::new());
    }
    let (manifest, _) = read_manifest(repository_root)?;
    Ok(manifest
        .dashboard_files
        .into_iter()
        .map(|entry| GeneratedFileEntryV2 {
            path: entry.path,
            content_digest: entry.content_digest,
        })
        .collect())
}

pub(crate) fn generated_projection_entries_v2(
    repository_root: &Path,
) -> Result<Vec<GeneratedFileEntryV2>, MkoError> {
    let path = repository_root.join(MANIFEST_PATH);
    if matches!(
        fs::symlink_metadata(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Ok(Vec::new());
    }
    let (manifest, _) = read_manifest(repository_root)?;
    Ok(manifest
        .projections
        .into_iter()
        .map(|entry| GeneratedFileEntryV2 {
            path: entry.path,
            content_digest: entry.content_digest,
        })
        .collect())
}

pub(crate) fn set_generated_dashboard_digest_locked_v2(
    repository_root: &Path,
    relative: &str,
    content_digest: &str,
) -> Result<(), MkoError> {
    if !is_canonical_dashboard_path(relative) || validate_digest(content_digest).is_err() {
        return Err(MkoError::new(
            "dashboard_manifest_invalid",
            "dashboard manifest entry is not a canonical managed file",
        ));
    }
    let manifest_path = repository_root.join(MANIFEST_PATH);
    let (mut manifest, manifest_bytes) = match fs::symlink_metadata(&manifest_path) {
        Ok(_) => read_manifest(repository_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            GeneratedManifestV2 {
                schema_version: 2,
                dashboard_files: Vec::new(),
                projections: Vec::new(),
            },
            Vec::new(),
        ),
        Err(error) => {
            return Err(MkoError::new(
                "projection_manifest_invalid",
                error.to_string(),
            ));
        }
    };
    if let Some(entry) = manifest
        .dashboard_files
        .iter_mut()
        .find(|entry| entry.path == relative)
    {
        entry.content_digest = Some(content_digest.to_owned());
    } else {
        manifest.dashboard_files.push(GeneratedProjectionEntryV2 {
            path: relative.to_owned(),
            content_digest: Some(content_digest.to_owned()),
        });
        manifest
            .dashboard_files
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    if manifest_bytes.is_empty() {
        let bytes = render_manifest(&manifest)?;
        let publication = write_new(&manifest_path, &bytes, |_| {
            Err(MkoError::new(
                "projection_manifest_snapshot_changed",
                "the generated manifest appeared while dashboard ownership was claimed",
            ))
        })
        .map_err(map_projection_atomic_error)?;
        if publication != AtomicWriteResult::Created {
            return Err(MkoError::new(
                "projection_manifest_snapshot_changed",
                "the generated manifest changed while dashboard ownership was claimed",
            ));
        }
    } else {
        let _ = write_manifest(repository_root, &manifest_bytes, &manifest)?;
    }
    Ok(())
}

pub fn projection_relative_path_v2(input: &ProjectionInputV2) -> Result<String, MkoError> {
    validate_input(input)?;
    Ok(projection_relative_path_unchecked(input))
}

fn projection_relative_path_unchecked(input: &ProjectionInputV2) -> String {
    format!(
        "views/records/{}-{}.md",
        input.record_type.as_str(),
        input.id
    )
}

pub fn render_projection_v2(input: &ProjectionInputV2) -> Result<RenderedProjectionV2, MkoError> {
    validate_input(input)?;
    render_projection_unchecked(input)
}

fn render_projection_unchecked(
    input: &ProjectionInputV2,
) -> Result<RenderedProjectionV2, MkoError> {
    let projection_digest = canonical_json_sha256(input)?;
    let title = normalize(&input.title);
    let domain = normalize(&input.domain);
    let record_link = normalize(&input.record_link);
    let asset_link = normalize(&input.asset_link);
    let tags = input
        .tags
        .iter()
        .map(|tag| normalize(tag))
        .collect::<Vec<_>>();
    let perspectives = serde_json::to_string(&input.perspectives)
        .map_err(|error| MkoError::new("projection_invalid", error.to_string()))?;
    let perspectives_line = if input.perspectives.is_empty() {
        String::new()
    } else {
        format!("perspectives: {perspectives}\n")
    };
    let review_head = input
        .review_head_id
        .as_deref()
        .map(normalize)
        .map(|value| json_string(&value))
        .transpose()?
        .unwrap_or_else(|| "null".into());
    let tags = serde_json::to_string(&tags)
        .map_err(|error| MkoError::new("projection_invalid", error.to_string()))?;
    let heading = title.replace('\n', " ");
    let text = format!(
        "---\nprojection_schema_version: 2\nrecord_type: {}\nrecord_id: {}\ntitle: {}\ncurrent_revision: {}\nreview_head_id: {}\nderived_state: {}\ndomain: {}\n{}tags: {}\nrecord_link: {}\nasset_link: {}\nprojection_digest: {}\n---\n\n# {}\n\n- Record: [[{}]]\n- Asset: [[{}]]\n- Current revision: `{}`\n",
        input.record_type.as_str(),
        json_string(&input.id)?,
        json_string(&title)?,
        json_string(&input.current_revision)?,
        review_head,
        input.derived_state.as_str(),
        json_string(&domain)?,
        perspectives_line,
        tags,
        json_string(&record_link)?,
        json_string(&asset_link)?,
        json_string(&projection_digest)?,
        heading,
        record_link,
        asset_link,
        input.current_revision,
    );
    Ok(RenderedProjectionV2 {
        bytes: text.into_bytes(),
        projection_digest,
    })
}

pub fn write_projection_v2(
    repository_root: &Path,
    input: &ProjectionInputV2,
) -> Result<ProjectionWriteResultV2, MkoError> {
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "write v2 projection",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    write_projection_locked(repository_root, input)
}

pub fn repair_projection_v2(
    repository_root: &Path,
    input: &ProjectionInputV2,
    expected_modified_digest: &str,
) -> Result<ProjectionWriteResultV2, MkoError> {
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "repair v2 projection",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    validate_repository_layout(repository_root)?;
    validate_input(input)?;
    let relative = projection_relative_path_unchecked(input);
    let rendered = render_projection_unchecked(input)?;
    let path = repository_root.join(&relative);
    let (mut manifest, manifest_bytes) = read_manifest(repository_root)?;
    let entry_index = owned_entry_index(&manifest, &relative)?;
    let current = read_regular_projection(&path)?;
    let current_digest = sha256_digest(&current);
    if current_digest != expected_modified_digest {
        return Err(MkoError::new(
            "projection_snapshot_changed",
            "the user-modified projection changed after recovery was prepared",
        ));
    }
    let expected_digest = sha256_digest(&rendered.bytes);
    let recovery = recovery_paths(repository_root, input, &current_digest, &expected_digest);
    validate_prepared_recovery(&recovery, &current, &rendered.bytes)?;
    replace_exact(&path, &current, &rendered.bytes)?;
    manifest.projections[entry_index].content_digest = Some(sha256_digest(&rendered.bytes));
    let _ = write_manifest(repository_root, &manifest_bytes, &manifest)?;
    Ok(result(
        path,
        rendered,
        ProjectionWriteOutcomeV2::Repaired,
        Some(recovery),
    ))
}

pub(crate) fn write_projection_locked(
    repository_root: &Path,
    input: &ProjectionInputV2,
) -> Result<ProjectionWriteResultV2, MkoError> {
    validate_repository_layout(repository_root)?;
    validate_input(input)?;
    let relative = projection_relative_path_unchecked(input);
    let rendered = render_projection_unchecked(input)?;
    let path = repository_root.join(&relative);
    let (mut manifest, manifest_bytes, entry_index) =
        claim_projection_path(repository_root, &relative)?;
    let recorded_digest = manifest.projections[entry_index].content_digest.clone();

    let outcome = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let publication = write_new(&path, &rendered.bytes, |existing| {
                let actual = read_regular_projection(existing)?;
                if actual == rendered.bytes {
                    Ok(())
                } else {
                    Err(MkoError::new(
                        "projection_snapshot_changed",
                        "the missing projection was created with different bytes",
                    ))
                }
            })
            .map_err(map_projection_atomic_error)?;
            match publication {
                AtomicWriteResult::Created => ProjectionWriteOutcomeV2::Created,
                AtomicWriteResult::Existing => ProjectionWriteOutcomeV2::Existing,
            }
        }
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            let actual = read_regular_projection(&path)?;
            if actual == rendered.bytes {
                ProjectionWriteOutcomeV2::Existing
            } else if recorded_digest.as_deref() == Some(&sha256_digest(&actual)) {
                replace_exact(&path, &actual, &rendered.bytes)?;
                ProjectionWriteOutcomeV2::Updated
            } else {
                let recovery = prepare_recovery(repository_root, input, &actual, &rendered.bytes)?;
                return Ok(result(
                    path,
                    rendered,
                    ProjectionWriteOutcomeV2::RepairRequired,
                    Some(recovery),
                ));
            }
        }
        Ok(_) => {
            return Err(MkoError::new(
                "projection_destination_invalid",
                "the projection destination must be a non-symlink regular file",
            ));
        }
        Err(error) => {
            return Err(MkoError::new("projection_write_failed", error.to_string()));
        }
    };

    manifest.projections[entry_index].content_digest = Some(sha256_digest(&rendered.bytes));
    let _ = write_manifest(repository_root, &manifest_bytes, &manifest)?;
    Ok(result(path, rendered, outcome, None))
}

pub(crate) fn prepare_projection_recovery_locked_v2(
    repository_root: &Path,
    input: &ProjectionInputV2,
    actual: &[u8],
) -> Result<ProjectionRecoveryV2, MkoError> {
    validate_repository_layout(repository_root)?;
    validate_input(input)?;
    let rendered = render_projection_unchecked(input)?;
    prepare_recovery(repository_root, input, actual, &rendered.bytes)
}

fn result(
    path: PathBuf,
    rendered: RenderedProjectionV2,
    outcome: ProjectionWriteOutcomeV2,
    recovery: Option<ProjectionRecoveryV2>,
) -> ProjectionWriteResultV2 {
    ProjectionWriteResultV2 {
        path,
        bytes: rendered.bytes,
        projection_digest: rendered.projection_digest,
        outcome,
        recovery,
    }
}

fn validate_input(input: &ProjectionInputV2) -> Result<(), MkoError> {
    let expected_prefix = format!("personal-{}-", input.record_type.as_str());
    validate_prefixed_hex(&input.id, &expected_prefix, "record ID")?;
    validate_prefixed_hex(&input.current_revision, "sha256:", "current revision")?;
    if let Some(review_head) = &input.review_head_id {
        validate_prefixed_hex(review_head, "personal-review-", "review head ID")?;
    }
    if input.title.is_empty() || input.title.chars().count() > 4096 {
        return Err(projection_invalid("title must be non-empty and bounded"));
    }
    if input.domain.is_empty()
        || input.domain.chars().count() > 256
        || input.perspectives.len() > PerspectiveV2::all().len()
        || input.tags.len() > 256
    {
        return Err(projection_invalid(
            "domain, perspectives, or tags are invalid",
        ));
    }
    if !input.perspectives.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(projection_invalid("perspectives must be sorted and unique"));
    }
    if input.record_type == ProjectionRecordTypeV2::Source && !input.perspectives.is_empty() {
        return Err(projection_invalid(
            "Source projections cannot carry Knowledge perspectives",
        ));
    }
    if input
        .tags
        .iter()
        .any(|tag| tag.is_empty() || tag.chars().count() > 256)
    {
        return Err(projection_invalid("tags must be non-empty and bounded"));
    }
    validate_logical_link(&input.record_link)?;
    validate_logical_link(&input.asset_link)
}

fn validate_prefixed_hex(value: &str, prefix: &str, label: &str) -> Result<(), MkoError> {
    let hash = value
        .strip_prefix(prefix)
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| projection_invalid(format!("{label} is invalid")))?;
    let _ = hash;
    Ok(())
}

fn validate_logical_link(value: &str) -> Result<(), MkoError> {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        || !is_safe_relative_path(value)
    {
        return Err(projection_invalid(
            "projection links must be safe relative paths",
        ));
    }
    Ok(())
}

fn is_safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_repository_layout(repository_root: &Path) -> Result<(), MkoError> {
    validate_real_directory(repository_root)?;
    for relative in [
        ".mko",
        "views",
        "views/records",
        "recovery",
        "recovery/manual-edits",
    ] {
        validate_real_directory(&repository_root.join(relative))?;
    }
    Ok(())
}

fn validate_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(MkoError::new(
            "projection_destination_invalid",
            "a managed projection path is a symlink or non-directory",
        )),
        Err(error) => Err(MkoError::new(
            "projection_destination_invalid",
            error.to_string(),
        )),
    }
}

fn read_manifest(repository_root: &Path) -> Result<(GeneratedManifestV2, Vec<u8>), MkoError> {
    let path = repository_root.join(MANIFEST_PATH);
    let bytes = read_regular_file(&path, "projection_manifest_invalid", MANIFEST_BYTE_LIMIT)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| MkoError::new("projection_manifest_invalid", error.to_string()))?;
    validate_yaml_input(text)
        .map_err(|error| MkoError::new("projection_manifest_invalid", error.message()))?;
    let manifest: GeneratedManifestV2 = serde_saphyr::from_str(text)
        .map_err(|error| MkoError::new("projection_manifest_invalid", error.to_string()))?;
    if manifest.schema_version != 2 {
        return Err(MkoError::new(
            "projection_manifest_invalid",
            "generated manifest schema_version must be 2",
        ));
    }
    let mut paths = HashSet::new();
    for entry in &manifest.projections {
        if !is_canonical_projection_path(&entry.path)
            || !paths.insert(entry.path.as_str())
            || entry
                .content_digest
                .as_deref()
                .is_some_and(|digest| validate_digest(digest).is_err())
        {
            return Err(MkoError::new(
                "projection_manifest_invalid",
                "generated manifest contains an invalid or duplicate projection entry",
            ));
        }
    }
    let mut dashboard_paths = HashSet::new();
    for entry in &manifest.dashboard_files {
        if !is_canonical_dashboard_path(&entry.path)
            || !dashboard_paths.insert(entry.path.as_str())
            || entry
                .content_digest
                .as_deref()
                .is_none_or(|digest| validate_digest(digest).is_err())
        {
            return Err(MkoError::new(
                "projection_manifest_invalid",
                "generated manifest contains an invalid dashboard file entry",
            ));
        }
    }
    Ok((manifest, bytes))
}

fn owned_entry_index(manifest: &GeneratedManifestV2, relative: &str) -> Result<usize, MkoError> {
    manifest
        .projections
        .iter()
        .position(|entry| entry.path == relative)
        .ok_or_else(|| {
            MkoError::new(
                "projection_path_unowned",
                "the generated manifest does not own this projection path",
            )
        })
}

fn claim_projection_path(
    repository_root: &Path,
    relative: &str,
) -> Result<(GeneratedManifestV2, Vec<u8>, usize), MkoError> {
    let manifest_path = repository_root.join(MANIFEST_PATH);
    let (mut manifest, mut manifest_bytes) = match fs::symlink_metadata(&manifest_path) {
        Ok(_) => read_manifest(repository_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let manifest = GeneratedManifestV2 {
                schema_version: 2,
                dashboard_files: Vec::new(),
                projections: vec![GeneratedProjectionEntryV2 {
                    path: relative.into(),
                    content_digest: None,
                }],
            };
            let bytes = render_manifest(&manifest)?;
            let publication = write_new(&manifest_path, &bytes, |_| {
                Err(MkoError::new(
                    "projection_manifest_snapshot_changed",
                    "the generated manifest appeared while its first projection was claimed",
                ))
            })
            .map_err(map_projection_atomic_error)?;
            if publication != AtomicWriteResult::Created {
                return Err(MkoError::new(
                    "projection_manifest_snapshot_changed",
                    "the generated manifest changed while its first projection was claimed",
                ));
            }
            return Ok((manifest, bytes, 0));
        }
        Err(error) => {
            return Err(MkoError::new(
                "projection_manifest_invalid",
                error.to_string(),
            ));
        }
    };

    if let Some(index) = manifest
        .projections
        .iter()
        .position(|entry| entry.path == relative)
    {
        return Ok((manifest, manifest_bytes, index));
    }

    manifest.projections.push(GeneratedProjectionEntryV2 {
        path: relative.into(),
        content_digest: None,
    });
    manifest
        .projections
        .sort_by(|left, right| left.path.cmp(&right.path));
    manifest_bytes = write_manifest(repository_root, &manifest_bytes, &manifest)?;
    let index = owned_entry_index(&manifest, relative)?;
    Ok((manifest, manifest_bytes, index))
}

fn is_canonical_dashboard_path(value: &str) -> bool {
    matches!(
        value,
        "HOME.md" | "views/review-queue.base" | "views/knowledge-library.base"
    )
}

fn is_canonical_projection_path(value: &str) -> bool {
    let Some(filename) = value.strip_prefix("views/records/") else {
        return false;
    };
    if !is_safe_relative_path(value) || filename.contains('/') {
        return false;
    }
    let Some(stem) = filename.strip_suffix(".md") else {
        return false;
    };
    [
        ("source-", "personal-source-"),
        ("knowledge-", "personal-knowledge-"),
    ]
    .into_iter()
    .any(|(path_prefix, id_prefix)| {
        stem.strip_prefix(path_prefix)
            .is_some_and(|id| validate_prefixed_hex(id, id_prefix, "record ID").is_ok())
    })
}

fn write_manifest(
    repository_root: &Path,
    expected: &[u8],
    manifest: &GeneratedManifestV2,
) -> Result<Vec<u8>, MkoError> {
    let replacement = render_manifest(manifest)?;
    if replacement == expected {
        return Ok(replacement);
    }
    let directory =
        Dir::open_ambient_dir(repository_root.join(".mko"), cap_std::ambient_authority())
            .map_err(|error| MkoError::new("projection_write_failed", error.to_string()))?;
    write_replace_capability_compare_exchange_validated_at_commit(
        &directory,
        Path::new("generated-manifest.yaml"),
        expected,
        &replacement,
        || Ok(()),
        || Ok(()),
    )
    .map_err(map_projection_atomic_error)?;
    Ok(replacement)
}

fn render_manifest(manifest: &GeneratedManifestV2) -> Result<Vec<u8>, MkoError> {
    serde_saphyr::to_string(manifest)
        .map_err(|error| MkoError::new("projection_manifest_invalid", error.to_string()))
        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n").into_bytes())
}

fn replace_exact(path: &Path, expected: &[u8], replacement: &[u8]) -> Result<(), MkoError> {
    let parent = path
        .parent()
        .ok_or_else(|| MkoError::new("projection_write_failed", "projection has no parent"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| MkoError::new("projection_write_failed", "projection has no filename"))?;
    let directory = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|error| MkoError::new("projection_write_failed", error.to_string()))?;
    write_replace_capability_compare_exchange_validated_at_commit(
        &directory,
        Path::new(filename),
        expected,
        replacement,
        || Ok(()),
        || Ok(()),
    )
    .map_err(map_projection_atomic_error)
}

fn prepare_recovery(
    repository_root: &Path,
    input: &ProjectionInputV2,
    actual: &[u8],
    expected: &[u8],
) -> Result<ProjectionRecoveryV2, MkoError> {
    let modified_digest = sha256_digest(actual);
    let expected_digest = sha256_digest(expected);
    let recovery = recovery_paths(repository_root, input, &modified_digest, &expected_digest);
    let directory = recovery
        .backup_path
        .parent()
        .ok_or_else(|| MkoError::new("projection_recovery_failed", "invalid recovery path"))?;
    ensure_real_directory(directory)?;
    publish_recovery_file(&recovery.backup_path, actual, PROJECTION_BYTE_LIMIT)?;
    publish_recovery_file(&recovery.expected_path, expected, PROJECTION_BYTE_LIMIT)?;
    let diff = deterministic_diff(expected, actual);
    publish_recovery_file(&recovery.diff_path, &diff, RECOVERY_DIFF_BYTE_LIMIT)?;
    Ok(recovery)
}

fn ensure_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_real_directory(path)
        }
        Err(error) => Err(MkoError::new(
            "projection_recovery_failed",
            error.to_string(),
        )),
    }
}

fn recovery_paths(
    repository_root: &Path,
    input: &ProjectionInputV2,
    modified_digest: &str,
    expected_digest: &str,
) -> ProjectionRecoveryV2 {
    let modified = modified_digest.replace(':', "-");
    let expected = expected_digest.replace(':', "-");
    let directory = repository_root.join(format!(
        "recovery/manual-edits/{}-{}-{modified}-to-{expected}",
        input.record_type.as_str(),
        input.id
    ));
    ProjectionRecoveryV2 {
        modified_digest: modified_digest.into(),
        backup_path: directory.join("projection.original.md"),
        expected_path: directory.join("projection.expected.md"),
        diff_path: directory.join("projection.diff"),
    }
}

fn publish_recovery_file(path: &Path, bytes: &[u8], limit: u64) -> Result<(), MkoError> {
    write_new(path, bytes, |existing| {
        let current = read_regular_file(existing, "projection_recovery_failed", limit)?;
        if current == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "projection_recovery_failed",
                "an existing recovery artifact has different bytes",
            ))
        }
    })
    .map(|_| ())
    .map_err(|error| MkoError::new("projection_recovery_failed", error.message()))
}

fn validate_prepared_recovery(
    recovery: &ProjectionRecoveryV2,
    current: &[u8],
    expected: &[u8],
) -> Result<(), MkoError> {
    let backup = read_regular_file(
        &recovery.backup_path,
        "projection_repair_unprepared",
        PROJECTION_BYTE_LIMIT,
    )?;
    if backup != current {
        return Err(MkoError::new(
            "projection_repair_unprepared",
            "the prepared recovery backup does not match the current projection",
        ));
    }
    let prepared_expected = read_regular_file(
        &recovery.expected_path,
        "projection_repair_unprepared",
        PROJECTION_BYTE_LIMIT,
    )?;
    if prepared_expected != expected {
        return Err(MkoError::new(
            "projection_repair_unprepared",
            "the prepared recovery target does not match the requested projection",
        ));
    }
    let _ = read_regular_file(
        &recovery.diff_path,
        "projection_repair_unprepared",
        RECOVERY_DIFF_BYTE_LIMIT,
    )?;
    Ok(())
}

fn read_regular_projection(path: &Path) -> Result<Vec<u8>, MkoError> {
    read_regular_file(
        path,
        "projection_destination_invalid",
        PROJECTION_BYTE_LIMIT,
    )
}

fn read_regular_file(path: &Path, code: &str, limit: u64) -> Result<Vec<u8>, MkoError> {
    read_regular_file_with_hook(path, code, limit, || {})
}

fn read_regular_file_with_hook<H>(
    path: &Path,
    code: &str,
    limit: u64,
    before_open: H,
) -> Result<Vec<u8>, MkoError>
where
    H: FnOnce(),
{
    before_open();
    let mut options = OpenOptions::new();
    options.read(true);
    configure_projection_open(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            code,
            "managed path must be a non-symlink regular file",
        ));
    }
    if metadata.len() > limit {
        return Err(MkoError::new(code, "managed file exceeds its byte limit"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(MkoError::new(code, "managed file exceeds its byte limit"));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn configure_projection_open(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_projection_open(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_projection_open(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_projection_open(_: &mut OpenOptions) {}

fn deterministic_diff(expected: &[u8], actual: &[u8]) -> Vec<u8> {
    format!(
        "--- generated\n+++ user-modified\n@@ -1 +1 @@\n-{}\n+{}\n",
        escape_bytes(expected),
        escape_bytes(actual)
    )
    .into_bytes()
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes {
        match byte {
            b'\\' => output.push_str("\\\\"),
            b'\n' => output.push_str("\\n"),
            b'\r' => output.push_str("\\r"),
            b'\t' => output.push_str("\\t"),
            0x20..=0x7e => output.push(char::from(*byte)),
            _ => output.push_str(&format!("\\x{byte:02x}")),
        }
    }
    output
}

fn json_string(value: &str) -> Result<String, MkoError> {
    serde_json::to_string(value)
        .map_err(|error| MkoError::new("projection_invalid", error.to_string()))
}

fn normalize(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn validate_digest(value: &str) -> Result<(), MkoError> {
    validate_prefixed_hex(value, "sha256:", "digest")
}

fn projection_invalid(message: impl Into<String>) -> MkoError {
    MkoError::new("projection_invalid", message)
}

fn map_projection_atomic_error(error: MkoError) -> MkoError {
    let code = match error.code() {
        "registry_destination_invalid" => "projection_destination_invalid",
        "registry_snapshot_changed" => "projection_snapshot_changed",
        "registry_not_found" => "projection_not_found",
        "registry_locked" => "projection_locked",
        "registry_write_failed" => "projection_write_failed",
        _ => return error,
    };
    MkoError::new(code, error.message())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::tempdir;

    use super::{PROJECTION_BYTE_LIMIT, read_regular_file_with_hook};

    #[test]
    fn swap_to_symlink_before_open_is_rejected_without_reading_target() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("projection.md");
        let target = temporary.path().join("outside.md");
        fs::write(&path, b"original").unwrap();
        fs::write(&target, b"outside secret").unwrap();

        let error = read_regular_file_with_hook(
            &path,
            "projection_destination_invalid",
            PROJECTION_BYTE_LIMIT,
            || {
                fs::remove_file(&path).unwrap();
                symlink(&target, &path).unwrap();
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "projection_destination_invalid");
        assert_eq!(fs::read(target).unwrap(), b"outside secret");
    }
}
