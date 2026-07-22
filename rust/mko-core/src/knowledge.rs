use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, OpenOptionsExt},
};
use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::model::AssetRecord;
use crate::{
    atomic::{AtomicWriteResult, write_replace_capability_compare_exchange_validated_at_commit},
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    path_policy::canonical_directory,
    prepare::{PreparedSourceBundle, load_prepared_source_bundle},
    provider_scan::{ElapsedClock, MonotonicElapsedClock, ScanDeadline, ScanLimits},
    registry::read_asset,
};

pub const MAX_KNOWLEDGE_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_KNOWLEDGE_STRING_BYTES: usize = 64 * 1024;
const MAX_KNOWLEDGE_SLUG_BYTES: usize = 96;
const MAX_KNOWLEDGE_ENTRIES: usize = 1024;
const MAX_KNOWLEDGE_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const MAX_KNOWLEDGE_SCAN_ELAPSED_MS: u64 = 5_000;
const KNOWLEDGE_READ_CHUNK_BYTES: usize = 64 * 1024;
const DEFAULT_KNOWLEDGE_SCAN_LIMITS: ScanLimits = ScanLimits {
    max_entries: MAX_KNOWLEDGE_ENTRIES as u64,
    max_total_bytes: MAX_KNOWLEDGE_SCAN_BYTES,
    max_elapsed_ms: MAX_KNOWLEDGE_SCAN_ELAPSED_MS,
    max_depth: 1,
    max_batch_items: MAX_KNOWLEDGE_ENTRIES as u64,
};
static NEXT_KNOWLEDGE_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConceptKind {
    Definition,
    Formula,
    Concept,
    Result,
    Theorem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Concept {
    pub id: String,
    pub name: String,
    pub kind: ConceptKind,
    pub body: String,
    pub tags: Vec<String>,
    pub locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConceptInput {
    pub name: String,
    pub kind: ConceptKind,
    pub body: String,
    pub tags: Vec<String>,
    pub locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeResponse {
    pub synthesis: String,
    pub concepts: Vec<ConceptInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Unreviewed,
    Reviewed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeReview {
    pub status: ReviewState,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeGeneration {
    pub processor_version: String,
    pub prompt_version: String,
    pub asset_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRecord {
    pub id: String,
    pub record_type: String,
    pub schema_version: u32,
    pub asset_id: String,
    pub title: String,
    pub review: KnowledgeReview,
    pub content_revision: String,
    pub approved_revision: Option<String>,
    pub generation: KnowledgeGeneration,
    pub concepts: Vec<Concept>,
}

pub fn parse_knowledge_response(input: &[u8]) -> Result<KnowledgeResponse, MkoError> {
    if input.len() > MAX_KNOWLEDGE_RESPONSE_BYTES {
        return Err(schema_error("knowledge response exceeds 1 MiB"));
    }
    let response: KnowledgeResponse = serde_json::from_slice(input)
        .map_err(|error| schema_error(format!("invalid knowledge response: {error}")))?;
    Ok(response)
}

pub fn normalize_and_validate_knowledge(response: &mut KnowledgeResponse) -> Result<(), MkoError> {
    normalize_string(&mut response.synthesis)?;
    if response.synthesis.trim().is_empty() {
        return Err(schema_error("synthesis must not be empty"));
    }
    for concept in &mut response.concepts {
        normalize_string(&mut concept.name)?;
        if concept.name.trim().is_empty() || concept.name.contains('\n') {
            return Err(schema_error("concept name must be a non-empty single line"));
        }
        normalize_string(&mut concept.body)?;
        if concept.body.trim().is_empty() {
            return Err(schema_error("concept body must not be empty"));
        }
        for tag in &mut concept.tags {
            normalize_string(tag)?;
        }
        if let Some(locator) = &mut concept.locator {
            normalize_string(locator)?;
        }
    }
    validate_aggregate_knowledge_limits(response)?;
    Ok(())
}

fn validate_aggregate_knowledge_limits(response: &KnowledgeResponse) -> Result<(), MkoError> {
    let names_joined = response
        .concepts
        .iter()
        .map(|concept| concept.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    validate_semantic_size(&names_joined)?;
    let tags_joined = response
        .concepts
        .iter()
        .flat_map(|concept| concept.tags.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    validate_semantic_size(&tags_joined)?;
    Ok(())
}

fn normalize_string(value: &mut String) -> Result<(), MkoError> {
    *value = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    validate_semantic_size(value)
}

fn validate_semantic_size(value: &str) -> Result<(), MkoError> {
    if value.len() > MAX_KNOWLEDGE_STRING_BYTES {
        return Err(schema_error("normalized knowledge section exceeds 64 KiB"));
    }
    Ok(())
}

fn schema_error(message: impl Into<String>) -> MkoError {
    MkoError::new("semantic_schema_invalid", message)
}

#[derive(Clone, Debug)]
pub struct WriteKnowledgeRequest {
    repository_root: PathBuf,
    bundle_path: Option<PathBuf>,
    asset_id: String,
    response: Vec<u8>,
    replace: bool,
}

impl WriteKnowledgeRequest {
    pub fn new(
        repository_root: impl AsRef<Path>,
        asset_id: impl Into<String>,
        response: Vec<u8>,
    ) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            bundle_path: None,
            asset_id: asset_id.into(),
            response,
            replace: false,
        }
    }

    pub fn with_bundle(mut self, bundle_path: impl AsRef<Path>) -> Self {
        self.bundle_path = Some(bundle_path.as_ref().to_path_buf());
        self
    }

    pub fn with_replace(mut self, replace: bool) -> Self {
        self.replace = replace;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteKnowledgeResult {
    pub result: String,
    pub knowledge_id: String,
    pub knowledge_path: String,
    pub content_revision: String,
}

struct KnowledgeDocument {
    record: KnowledgeRecord,
    body: String,
    path: PathBuf,
    filename: String,
    snapshot: Vec<u8>,
}

struct KnowledgeDirectory {
    path: PathBuf,
    repository: Dir,
    directory: Dir,
    identity: StableKnowledgeDirectoryIdentity,
}

pub trait KnowledgeMutationObserver {
    fn after_knowledge_directory_metadata(&mut self) -> Result<(), MkoError> {
        Ok(())
    }

    fn before_publication(&mut self) -> Result<(), MkoError>;
}

#[doc(hidden)]
pub trait KnowledgeScanObserver {
    fn before_entry_open(&mut self, _filename: &Path) -> Result<(), MkoError> {
        Ok(())
    }

    fn after_entry_metadata(&mut self, _filename: &Path) -> Result<(), MkoError> {
        Ok(())
    }

    fn after_read_chunk(&mut self, _bytes_read: usize) -> Result<(), MkoError> {
        Ok(())
    }
}

impl KnowledgeScanObserver for () {}

struct NoopKnowledgeMutationObserver;

impl KnowledgeMutationObserver for NoopKnowledgeMutationObserver {
    fn before_publication(&mut self) -> Result<(), MkoError> {
        Ok(())
    }
}

pub fn write_knowledge_note(
    request: WriteKnowledgeRequest,
) -> Result<WriteKnowledgeResult, MkoError> {
    write_knowledge_note_with_clock(request, &SystemClock)
}

pub fn write_knowledge_note_with_clock(
    request: WriteKnowledgeRequest,
    clock: &dyn Clock,
) -> Result<WriteKnowledgeResult, MkoError> {
    write_knowledge_note_with_clock_and_observer(request, clock, &mut NoopKnowledgeMutationObserver)
}

pub fn write_knowledge_note_with_clock_and_observer(
    request: WriteKnowledgeRequest,
    clock: &dyn Clock,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<WriteKnowledgeResult, MkoError> {
    let scan_clock = MonotonicElapsedClock::start();
    write_knowledge_note_with_clocks_and_observer(request, clock, &scan_clock, observer)
}

#[doc(hidden)]
pub fn write_knowledge_note_with_clocks_and_observer(
    request: WriteKnowledgeRequest,
    _clock: &dyn Clock,
    scan_clock: &dyn ElapsedClock,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<WriteKnowledgeResult, MkoError> {
    let repository_root = canonical_directory(&request.repository_root, "repository_root_invalid")?;
    let mut response = parse_knowledge_response(&request.response)?;
    normalize_and_validate_knowledge(&mut response)?;
    let asset = read_asset(&repository_root, &request.asset_id)
        .map_err(|error| MkoError::new("asset_not_found", error.message()))?;
    let bundle_path = request.bundle_path.as_deref().ok_or_else(|| {
        MkoError::new(
            "bundle_required",
            "writing knowledge requires a prepared Source bundle",
        )
    })?;
    let bundle = load_prepared_source_bundle(&request.repository_root, bundle_path)?;
    validate_knowledge_bundle(&bundle, &asset, &request.asset_id)?;

    let hash = asset.id.strip_prefix("personal-asset-").ok_or_else(|| {
        MkoError::new(
            "asset_id_invalid",
            "asset ID must be a content-addressed asset ID",
        )
    })?;
    let expected_id = format!("personal-knowledge-{hash}");

    let scan_deadline = ScanDeadline::start(scan_clock, DEFAULT_KNOWLEDGE_SCAN_LIMITS);
    let knowledge = existing_knowledge_directory_with_observer(&repository_root, observer)?;
    let existing = knowledge
        .as_ref()
        .map(|knowledge| {
            find_knowledge_in_directory_with_deadline(knowledge, &expected_id, &scan_deadline)
        })
        .transpose()?
        .flatten();
    let approved_revision = existing
        .as_ref()
        .and_then(|document| document.record.approved_revision.clone());

    let body = render_knowledge_body(&asset.title, &response);
    let concepts = assign_concept_ids(&response.concepts);
    let mut record = KnowledgeRecord {
        id: expected_id.clone(),
        record_type: "knowledge".into(),
        schema_version: 1,
        asset_id: asset.id.clone(),
        title: asset.title.clone(),
        review: KnowledgeReview {
            status: ReviewState::Unreviewed,
            reviewed_at: None,
        },
        content_revision: String::new(),
        approved_revision,
        generation: KnowledgeGeneration {
            processor_version: "knowledge-v1".into(),
            prompt_version: "codex-knowledge-v1".into(),
            asset_fingerprint: asset.fingerprint.value.clone(),
        },
        concepts,
    };
    record.content_revision = calculate_knowledge_revision(&record, &body)?;

    if let Some(existing) = existing {
        validate_document_revision(&existing, &existing.record.content_revision)?;
        if record.content_revision == existing.record.content_revision {
            return Ok(WriteKnowledgeResult {
                result: "existing".into(),
                knowledge_id: existing.record.id,
                knowledge_path: repository_relative(&repository_root, &existing.path)?,
                content_revision: existing.record.content_revision,
            });
        }
        if !request.replace {
            return Err(MkoError::new(
                "replace_required",
                "regenerating an existing knowledge note requires --replace",
            ));
        }
        let document = render_markdown(&record, &body)?;
        let knowledge = knowledge.as_ref().ok_or_else(knowledge_changed_error)?;
        observer.before_publication()?;
        write_replace_capability_compare_exchange_validated_at_commit(
            &knowledge.directory,
            Path::new(&existing.filename),
            &existing.snapshot,
            document.as_bytes(),
            || Ok(()),
            || {
                validate_expected_knowledge_snapshot(
                    &knowledge.directory,
                    Path::new(&existing.filename),
                    &existing.snapshot,
                    &existing.record.content_revision,
                )
            },
        )
        .map_err(map_knowledge_publication_error)?;
        verify_public_knowledge_publication(
            knowledge,
            Path::new(&existing.filename),
            document.as_bytes(),
        )?;
        return Ok(WriteKnowledgeResult {
            result: "replaced".into(),
            knowledge_id: record.id,
            knowledge_path: repository_relative(&repository_root, &existing.path)?,
            content_revision: record.content_revision,
        });
    }

    let knowledge_dir = knowledge_directory_with_observer(&repository_root, observer)?;
    let filename = knowledge_filename(asset.created_at, &asset.title, &expected_id)?;
    let document = render_markdown(&record, &body)?;
    observer.before_publication()?;
    match write_new_knowledge_capability(
        &knowledge_dir.directory,
        Path::new(&filename),
        document.as_bytes(),
        &record,
        &body,
    )? {
        result @ (AtomicWriteResult::Created | AtomicWriteResult::Existing) => {
            verify_public_knowledge_publication(
                &knowledge_dir,
                Path::new(&filename),
                document.as_bytes(),
            )?;
            Ok(WriteKnowledgeResult {
                result: match result {
                    AtomicWriteResult::Created => "created",
                    AtomicWriteResult::Existing => "existing",
                }
                .into(),
                knowledge_id: record.id,
                knowledge_path: format!("knowledge/{filename}"),
                content_revision: record.content_revision,
            })
        }
    }
}

fn validate_knowledge_bundle(
    bundle: &PreparedSourceBundle,
    asset: &crate::model::AssetRecord,
    requested_asset_id: &str,
) -> Result<(), MkoError> {
    if bundle.asset_id != requested_asset_id
        || bundle.asset_id != asset.id
        || bundle.source_id != asset.id.replacen("asset", "source", 1)
        || bundle.fingerprint != asset.fingerprint
        || bundle.title_hint != asset.title
        || bundle.logical_path != asset.provider.locator
    {
        return Err(MkoError::new(
            "bundle_invalid",
            "prepared Source bundle does not match the requested Asset Registry record",
        ));
    }
    Ok(())
}

fn assign_concept_ids(inputs: &[ConceptInput]) -> Vec<Concept> {
    let mut used = HashSet::new();
    inputs
        .iter()
        .map(|input| {
            let base = slugify_concept(&input.name);
            let mut candidate = base.clone();
            let mut suffix = 2;
            while used.contains(&candidate) {
                candidate = format!("{base}-{suffix}");
                suffix += 1;
            }
            used.insert(candidate.clone());
            Concept {
                id: candidate,
                name: input.name.clone(),
                kind: input.kind.clone(),
                body: input.body.clone(),
                tags: input.tags.clone(),
                locator: input.locator.clone(),
            }
        })
        .collect()
}

fn slugify_concept(name: &str) -> String {
    let mut slug = String::new();
    let mut previous_hyphen = false;
    for character in name.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_hyphen = false;
        } else if !previous_hyphen && !slug.is_empty() {
            slug.push('-');
            previous_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "concept".into()
    } else {
        slug
    }
}

fn concept_kind_label(kind: &ConceptKind) -> &'static str {
    match kind {
        ConceptKind::Definition => "definition",
        ConceptKind::Formula => "formula",
        ConceptKind::Concept => "concept",
        ConceptKind::Result => "result",
        ConceptKind::Theorem => "theorem",
    }
}

fn render_knowledge_body(title: &str, response: &KnowledgeResponse) -> String {
    let mut concepts_section = String::new();
    if response.concepts.is_empty() {
        concepts_section.push_str("_No concepts extracted._\n");
    }
    for (index, concept) in response.concepts.iter().enumerate() {
        if index > 0 {
            concepts_section.push('\n');
        }
        let locator = concept
            .locator
            .as_deref()
            .map(|locator| format!("  \u{b7}  {}", canonical_section_text(locator)))
            .unwrap_or_default();
        concepts_section.push_str(&format!(
            "### {}  \u{b7}  {}{}\n{}\n",
            canonical_section_text(&concept.name),
            concept_kind_label(&concept.kind),
            locator,
            canonical_section_text(&concept.body),
        ));
    }
    format!(
        "# {} \u{2014} Knowledge\n\n## Synthesis\n{}\n\n## Concepts\n{}",
        canonical_section_text(title),
        canonical_section_text(&response.synthesis),
        concepts_section,
    )
    .replace("\r\n", "\n")
    .replace('\r', "\n")
    .nfc()
    .collect()
}

fn canonical_section_text(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let whitespace_len = line.len() - line.trim_start_matches([' ', '\t']).len();
            let content = &line[whitespace_len..];
            let structural = content.trim_end_matches([' ', '\t']);
            let hash_heading = structural.starts_with('#');
            let fence = structural.starts_with("```") || structural.starts_with("~~~");
            let setext_or_rule = !structural.is_empty()
                && (structural.bytes().all(|byte| byte == b'=')
                    || structural.bytes().all(|byte| byte == b'-'));
            if hash_heading || fence || setext_or_rule {
                format!("{}\\{}", &line[..whitespace_len], content)
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn calculate_knowledge_revision(record: &KnowledgeRecord, body: &str) -> Result<String, MkoError> {
    let canonical = CanonicalKnowledgeRevision {
        title: normalize_revision_string(&record.title),
        asset_id: normalize_revision_string(&record.asset_id),
        concepts: record.concepts.iter().map(CanonicalConcept::from).collect(),
        body: normalize_revision_body(body),
    };
    let serialized = serde_json::to_vec(&canonical)
        .map_err(|error| MkoError::new("revision_invalid", error.to_string()))?;
    let digest = Sha256::digest(serialized);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

#[derive(Serialize)]
struct CanonicalKnowledgeRevision {
    title: String,
    asset_id: String,
    concepts: Vec<CanonicalConcept>,
    body: String,
}

#[derive(Serialize)]
struct CanonicalConcept {
    id: String,
    name: String,
    kind: ConceptKind,
    body: String,
    tags: Vec<String>,
    locator: Option<String>,
}

impl From<&Concept> for CanonicalConcept {
    fn from(concept: &Concept) -> Self {
        let mut tags: Vec<String> = concept
            .tags
            .iter()
            .map(|tag| normalize_revision_string(tag))
            .collect();
        tags.sort();
        tags.dedup();
        Self {
            id: concept.id.clone(),
            name: normalize_revision_string(&concept.name),
            kind: concept.kind.clone(),
            body: normalize_revision_string(&concept.body),
            tags,
            locator: concept.locator.as_deref().map(normalize_revision_string),
        }
    }
}

fn normalize_revision_string(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn normalize_revision_body(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<_> = normalized.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n")).nfc().collect()
    }
}

fn knowledge_filename(
    now: DateTime<Utc>,
    title: &str,
    knowledge_id: &str,
) -> Result<String, MkoError> {
    let hash = knowledge_id
        .strip_prefix("personal-knowledge-")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| {
            MkoError::new(
                "knowledge_id_invalid",
                "invalid content-addressed knowledge ID",
            )
        })?;
    let mut slug = String::new();
    let mut previous_hyphen = false;
    for byte in title.to_ascii_lowercase().bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            slug.push(char::from(byte));
            previous_hyphen = false;
        } else if !previous_hyphen && !slug.is_empty() {
            slug.push('-');
            previous_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(MAX_KNOWLEDGE_SLUG_BYTES);
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug = "document".into();
    }
    let date = now.with_timezone(&Seoul).date_naive();
    Ok(format!("{date}-{slug}-{}.md", &hash[..12]))
}

fn knowledge_directory_with_observer(
    repository_root: &Path,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<KnowledgeDirectory, MkoError> {
    if let Some(knowledge) = existing_knowledge_directory_with_observer(repository_root, observer)?
    {
        return Ok(knowledge);
    }
    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|_| knowledge_path_error())?;
    match repository.create_dir("knowledge") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(knowledge_path_error()),
    }
    open_knowledge_directory(repository_root, &repository, observer)?
        .ok_or_else(knowledge_path_error)
}

fn existing_knowledge_directory_with_observer(
    repository_root: &Path,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<Option<KnowledgeDirectory>, MkoError> {
    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|_| knowledge_path_error())?;
    open_knowledge_directory(repository_root, &repository, observer)
}

fn open_knowledge_directory(
    repository_root: &Path,
    repository: &Dir,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<Option<KnowledgeDirectory>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, true);
    let file = match repository.open_with("knowledge", &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(knowledge_path_error()),
    };
    let metadata = file.metadata().map_err(|_| knowledge_path_error())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(knowledge_path_error());
    }
    let identity = stable_knowledge_directory_identity(&file)?;
    observer.after_knowledge_directory_metadata()?;
    let public_file = repository
        .open_with("knowledge", &options)
        .map_err(|_| knowledge_path_error())?;
    let public_metadata = public_file.metadata().map_err(|_| knowledge_path_error())?;
    if !public_metadata.is_dir()
        || public_metadata.file_type().is_symlink()
        || stable_knowledge_directory_identity(&public_file)? != identity
    {
        return Err(knowledge_path_error());
    }
    let directory = Dir::from_std_file(file.into_std());
    Ok(Some(KnowledgeDirectory {
        path: repository_root.join("knowledge"),
        repository: repository.try_clone().map_err(|_| knowledge_path_error())?,
        directory,
        identity,
    }))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableKnowledgeDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
type StableKnowledgeDirectoryIdentity = mko_windows_acl::FileIdentity;

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableKnowledgeDirectoryIdentity;

#[cfg(unix)]
fn stable_knowledge_directory_identity(
    file: &cap_std::fs::File,
) -> Result<StableKnowledgeDirectoryIdentity, MkoError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .try_clone()
        .and_then(|file| file.into_std().metadata())
        .map_err(|_| knowledge_path_error())?;
    Ok(StableKnowledgeDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_knowledge_directory_identity(
    file: &cap_std::fs::File,
) -> Result<StableKnowledgeDirectoryIdentity, MkoError> {
    let file = file
        .try_clone()
        .map(cap_std::fs::File::into_std)
        .map_err(|_| knowledge_path_error())?;
    mko_windows_acl::file_identity(&file).map_err(|_| knowledge_path_error())
}

#[cfg(not(any(unix, windows)))]
fn stable_knowledge_directory_identity(
    _: &cap_std::fs::File,
) -> Result<StableKnowledgeDirectoryIdentity, MkoError> {
    Err(knowledge_path_error())
}

fn find_knowledge_in_directory_with_deadline(
    knowledge: &KnowledgeDirectory,
    knowledge_id: &str,
    deadline: &ScanDeadline<'_>,
) -> Result<Option<KnowledgeDocument>, MkoError> {
    let mut matches = read_knowledge_documents_from_directory_with_scan(
        knowledge,
        DEFAULT_KNOWLEDGE_SCAN_LIMITS,
        deadline,
        &mut (),
    )?
    .into_iter()
    .filter(|document| document.record.id == knowledge_id);
    let found = matches.next();
    if matches.next().is_some() {
        return Err(MkoError::new(
            "knowledge_conflict",
            "multiple knowledge notes use the same ID",
        ));
    }
    Ok(found)
}

fn read_knowledge_documents(repository_root: &Path) -> Result<Vec<KnowledgeDocument>, MkoError> {
    let clock = MonotonicElapsedClock::start();
    read_knowledge_documents_with_scan(
        repository_root,
        DEFAULT_KNOWLEDGE_SCAN_LIMITS,
        &clock,
        &mut (),
    )
}

fn read_knowledge_documents_with_scan(
    repository_root: &Path,
    limits: ScanLimits,
    clock: &dyn ElapsedClock,
    observer: &mut dyn KnowledgeScanObserver,
) -> Result<Vec<KnowledgeDocument>, MkoError> {
    let deadline = ScanDeadline::start(clock, limits);
    deadline.check()?;
    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|_| knowledge_path_error())?;
    let Some(knowledge) = open_knowledge_directory(
        repository_root,
        &repository,
        &mut NoopKnowledgeMutationObserver,
    )?
    else {
        deadline.check()?;
        return Ok(Vec::new());
    };
    read_knowledge_documents_from_directory_with_scan(&knowledge, limits, &deadline, observer)
}

fn read_knowledge_documents_from_directory_with_scan(
    knowledge: &KnowledgeDirectory,
    limits: ScanLimits,
    deadline: &ScanDeadline<'_>,
    observer: &mut dyn KnowledgeScanObserver,
) -> Result<Vec<KnowledgeDocument>, MkoError> {
    deadline.check()?;
    let mut filenames = Vec::new();
    let mut collision_names = HashSet::new();
    for entry in knowledge
        .directory
        .entries()
        .map_err(|_| knowledge_scan_error())?
    {
        deadline.check()?;
        if filenames.len() as u64 >= limits.max_entries {
            return Err(knowledge_scan_error());
        }
        let entry = entry.map_err(|_| knowledge_scan_error())?;
        let file_type = entry.file_type().map_err(|_| knowledge_scan_error())?;
        if file_type.is_symlink() {
            return Err(knowledge_path_error());
        }
        let filename = entry.file_name();
        let collision_name = filename
            .to_str()
            .ok_or_else(knowledge_path_error)?
            .nfc()
            .collect::<String>()
            .to_lowercase();
        if !collision_names.insert(collision_name) {
            return Err(MkoError::new(
                "path_collision",
                "knowledge contains a case or Unicode-normalization collision",
            ));
        }
        filenames.push(filename);
    }
    deadline.check()?;
    filenames.sort();
    let mut total = 0u64;
    let mut documents = Vec::new();
    for filename in filenames {
        deadline.check()?;
        if Path::new(&filename)
            .extension()
            .and_then(|value| value.to_str())
            != Some("md")
        {
            continue;
        }
        observer.before_entry_open(Path::new(&filename))?;
        deadline.check()?;
        let remaining = limits
            .max_total_bytes
            .checked_sub(total)
            .ok_or_else(knowledge_scan_error)?;
        let snapshot = read_knowledge_snapshot_for_scan(
            &knowledge.directory,
            Path::new(&filename),
            remaining,
            deadline,
            observer,
        )?;
        total = total
            .checked_add(snapshot.len() as u64)
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or_else(knowledge_scan_error)?;
        let input = std::str::from_utf8(&snapshot)
            .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        let parsed = parse_markdown::<KnowledgeRecord>(input)
            .map_err(|error| existing_knowledge_error(error.message()))?;
        documents.push(KnowledgeDocument {
            record: parsed.metadata,
            body: parsed.body,
            path: knowledge.path.join(&filename),
            filename: filename.into_string().map_err(|_| knowledge_scan_error())?,
            snapshot,
        });
    }
    deadline.check()?;
    Ok(documents)
}

fn read_knowledge_snapshot_for_scan(
    directory: &Dir,
    filename: &Path,
    max_bytes: u64,
    deadline: &ScanDeadline<'_>,
    observer: &mut dyn KnowledgeScanObserver,
) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    let mut file = directory
        .open_with(filename, &options)
        .map_err(|_| knowledge_path_error())?;
    let metadata = file.metadata().map_err(|_| knowledge_scan_error())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        return Err(knowledge_scan_error());
    }
    observer.after_entry_metadata(filename)?;
    deadline.check()?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; KNOWLEDGE_READ_CHUNK_BYTES];
    loop {
        deadline.check()?;
        let remaining_with_sentinel = max_bytes
            .saturating_add(1)
            .saturating_sub(bytes.len() as u64);
        if remaining_with_sentinel == 0 {
            return Err(knowledge_scan_error());
        }
        let chunk_limit = remaining_with_sentinel.min(chunk.len() as u64) as usize;
        let bytes_read = Read::read(&mut file, &mut chunk[..chunk_limit])
            .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        if bytes_read > 0 {
            observer.after_read_chunk(bytes_read)?;
        }
        deadline.check()?;
        if bytes_read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..bytes_read]);
        if bytes.len() as u64 > max_bytes {
            return Err(knowledge_scan_error());
        }
    }
    Ok(bytes)
}

fn read_knowledge_snapshot(directory: &Dir, filename: &Path) -> Result<Vec<u8>, MkoError> {
    read_knowledge_snapshot_with_before_open(directory, filename, || {})
}

fn read_knowledge_snapshot_with_before_open<F>(
    directory: &Dir,
    filename: &Path,
    before_open: F,
) -> Result<Vec<u8>, MkoError>
where
    F: FnOnce(),
{
    let path_metadata = directory
        .symlink_metadata(filename)
        .map_err(|_| knowledge_scan_error())?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(knowledge_path_error());
    }
    before_open();
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, false);
    let mut file = directory
        .open_with(filename, &options)
        .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
    let metadata = file.metadata().map_err(|_| knowledge_scan_error())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_KNOWLEDGE_SCAN_BYTES
    {
        return Err(knowledge_scan_error());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_KNOWLEDGE_SCAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
    if bytes.len() as u64 > MAX_KNOWLEDGE_SCAN_BYTES {
        return Err(knowledge_scan_error());
    }
    Ok(bytes)
}

fn verify_public_knowledge_publication(
    knowledge: &KnowledgeDirectory,
    filename: &Path,
    expected_bytes: &[u8],
) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options, true);
    let file = knowledge
        .repository
        .open_with("knowledge", &options)
        .map_err(|_| knowledge_publication_error())?;
    let metadata = file.metadata().map_err(|_| knowledge_publication_error())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || stable_knowledge_directory_identity(&file).map_err(|_| knowledge_publication_error())?
            != knowledge.identity
    {
        return Err(knowledge_publication_error());
    }
    let public_directory = Dir::from_std_file(file.into_std());
    let published = read_knowledge_snapshot(&public_directory, filename)
        .map_err(|_| knowledge_publication_error())?;
    if published != expected_bytes {
        return Err(knowledge_publication_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_DIRECTORY: i32 = 0x10_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    const O_DIRECTORY: i32 = 0x10_0000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK | if directory { O_DIRECTORY } else { 0 });
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

fn knowledge_scan_error() -> MkoError {
    MkoError::new(
        "knowledge_scan_limit",
        "knowledge discovery exceeded a bounded or regular-file input limit",
    )
}

fn write_new_knowledge_capability(
    directory: &Dir,
    filename: &Path,
    bytes: &[u8],
    expected: &KnowledgeRecord,
    expected_body: &str,
) -> Result<AtomicWriteResult, MkoError> {
    let filename = filename
        .to_str()
        .ok_or_else(|| MkoError::new("knowledge_write_failed", "invalid Knowledge filename"))?;
    let temporary = format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        NEXT_KNOWLEDGE_TEMP.fetch_add(1, Ordering::Relaxed)
    );
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = directory
            .open_with(&temporary, &options)
            .map_err(|error| MkoError::new("knowledge_write_failed", error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| MkoError::new("knowledge_write_failed", error.to_string()))?;
        file.sync_all()
            .map_err(|error| MkoError::new("knowledge_write_failed", error.to_string()))?;
        drop(file);
        match directory.hard_link(&temporary, directory, filename) {
            Ok(()) => {
                directory
                    .remove_file(&temporary)
                    .map_err(|error| MkoError::new("knowledge_write_failed", error.to_string()))?;
                sync_knowledge_directory(directory)?;
                Ok(AtomicWriteResult::Created)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_existing_knowledge(
                    directory,
                    Path::new(filename),
                    expected,
                    expected_body,
                )?;
                Ok(AtomicWriteResult::Existing)
            }
            Err(error) => Err(MkoError::new("knowledge_write_failed", error.to_string())),
        }
    })();
    if result.is_err() || matches!(&result, Ok(AtomicWriteResult::Existing)) {
        let _ = directory.remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_knowledge_directory(directory: &Dir) -> Result<(), MkoError> {
    directory
        .try_clone()
        .and_then(|directory| directory.into_std_file().sync_all())
        .map_err(|error| MkoError::new("knowledge_write_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_knowledge_directory(_directory: &Dir) -> Result<(), MkoError> {
    // Windows has no supported POSIX-equivalent parent-directory fsync in this safe API layer.
    // File content is flushed before linking the public entry, but parent-entry crash durability
    // is not claimed. This matches the shared capability publisher's platform contract.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_knowledge_directory(_directory: &Dir) -> Result<(), MkoError> {
    Ok(())
}

fn validate_existing_knowledge(
    directory: &Dir,
    filename: &Path,
    expected: &KnowledgeRecord,
    expected_body: &str,
) -> Result<(), MkoError> {
    let bytes = read_knowledge_snapshot(directory, filename)?;
    let input = std::str::from_utf8(&bytes)
        .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
    let existing = parse_markdown::<KnowledgeRecord>(input)
        .map_err(|error| existing_knowledge_error(error.message()))?;
    if existing.metadata.id != expected.id
        || existing.metadata.content_revision != expected.content_revision
        || calculate_knowledge_revision(&existing.metadata, &existing.body)?
            != expected.content_revision
        || calculate_knowledge_revision(expected, expected_body)? != expected.content_revision
    {
        return Err(MkoError::new(
            "knowledge_conflict",
            "deterministic knowledge path contains different content",
        ));
    }
    Ok(())
}

fn validate_document_revision(
    document: &KnowledgeDocument,
    expected_revision: &str,
) -> Result<(), MkoError> {
    let actual = calculate_knowledge_revision(&document.record, &document.body)?;
    if document.record.content_revision != actual || actual != expected_revision {
        return Err(knowledge_changed_error());
    }
    Ok(())
}

fn validate_expected_knowledge_snapshot(
    directory: &Dir,
    filename: &Path,
    expected_bytes: &[u8],
    expected_revision: &str,
) -> Result<(), MkoError> {
    let current =
        read_knowledge_snapshot(directory, filename).map_err(|_| knowledge_changed_error())?;
    if current != expected_bytes {
        return Err(knowledge_changed_error());
    }
    let input = std::str::from_utf8(&current).map_err(|_| knowledge_changed_error())?;
    let parsed = parse_markdown::<KnowledgeRecord>(input).map_err(|_| knowledge_changed_error())?;
    let actual = calculate_knowledge_revision(&parsed.metadata, &parsed.body)
        .map_err(|_| knowledge_changed_error())?;
    if parsed.metadata.content_revision != actual || actual != expected_revision {
        return Err(knowledge_changed_error());
    }
    Ok(())
}

fn map_knowledge_publication_error(error: MkoError) -> MkoError {
    if matches!(
        error.code(),
        "registry_destination_invalid" | "registry_not_found" | "registry_snapshot_changed"
    ) {
        knowledge_changed_error()
    } else {
        error
    }
}

fn knowledge_changed_error() -> MkoError {
    MkoError::new(
        "knowledge_revision_mismatch",
        "knowledge note changed after selection; nothing was published",
    )
}

fn repository_relative(repository_root: &Path, path: &Path) -> Result<String, MkoError> {
    path.strip_prefix(repository_root)
        .map_err(|_| knowledge_path_error())?
        .to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(knowledge_path_error)
}

fn existing_knowledge_error(message: impl Into<String>) -> MkoError {
    MkoError::new("existing_knowledge_invalid", message)
}

fn knowledge_path_error() -> MkoError {
    MkoError::new(
        "knowledge_path_invalid",
        "knowledge path must remain in a real knowledge directory",
    )
}

fn knowledge_publication_error() -> MkoError {
    MkoError::new(
        "knowledge_publication_invalid",
        "public knowledge entry does not match the committed knowledge publication",
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingKnowledge {
    pub knowledge_id: String,
    pub asset_id: String,
    pub title: String,
    pub knowledge_path: String,
    pub content_revision: String,
    pub rendered_markdown: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeNote {
    pub knowledge_id: String,
    pub asset_id: String,
    pub title: String,
    pub knowledge_path: String,
    pub content_revision: String,
    pub review_status: ReviewState,
    pub concepts: Vec<Concept>,
}

pub fn list_knowledge(repository_root: &Path) -> Result<Vec<KnowledgeNote>, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    project_knowledge_notes(
        &repository_root,
        read_knowledge_documents(&repository_root)?,
    )
}

fn project_knowledge_notes(
    repository_root: &Path,
    documents: Vec<KnowledgeDocument>,
) -> Result<Vec<KnowledgeNote>, MkoError> {
    let mut notes = documents
        .into_iter()
        .map(|document| {
            let knowledge_path = repository_relative(repository_root, &document.path)?;
            Ok(KnowledgeNote {
                knowledge_id: document.record.id,
                asset_id: document.record.asset_id,
                title: document.record.title,
                knowledge_path,
                content_revision: document.record.content_revision,
                review_status: document.record.review.status,
                concepts: document.record.concepts,
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    notes.sort_by(|left, right| {
        (&left.title, &left.knowledge_id).cmp(&(&right.title, &right.knowledge_id))
    });
    Ok(notes)
}

pub fn list_unreviewed_knowledge(
    repository_root: &Path,
) -> Result<Vec<PendingKnowledge>, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let mut pending = read_knowledge_documents(&repository_root)?
        .into_iter()
        .filter(|document| document.record.review.status == ReviewState::Unreviewed)
        .map(|document| {
            let knowledge_path = repository_relative(&repository_root, &document.path)?;
            let rendered_markdown = String::from_utf8(document.snapshot)
                .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
            Ok(PendingKnowledge {
                knowledge_id: document.record.id,
                asset_id: document.record.asset_id,
                title: document.record.title,
                knowledge_path,
                content_revision: document.record.content_revision,
                rendered_markdown,
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    pending.sort_by(|left, right| {
        (&left.title, &left.knowledge_id).cmp(&(&right.title, &right.knowledge_id))
    });
    Ok(pending)
}

pub fn approve_knowledge(
    repository_root: &Path,
    knowledge_id: &str,
    content_revision: &str,
) -> Result<(), MkoError> {
    approve_knowledge_with_clock(
        repository_root,
        knowledge_id,
        content_revision,
        &SystemClock,
    )
}

pub fn approve_knowledge_with_clock(
    repository_root: &Path,
    knowledge_id: &str,
    content_revision: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    approve_knowledge_with_clock_and_observer(
        repository_root,
        knowledge_id,
        content_revision,
        clock,
        &mut NoopKnowledgeMutationObserver,
    )
}

pub fn approve_knowledge_with_clock_and_observer(
    repository_root: &Path,
    knowledge_id: &str,
    content_revision: &str,
    clock: &dyn Clock,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<(), MkoError> {
    let scan_clock = MonotonicElapsedClock::start();
    approve_knowledge_with_clocks_and_observer(
        repository_root,
        knowledge_id,
        content_revision,
        clock,
        &scan_clock,
        observer,
    )
}

#[doc(hidden)]
pub fn approve_knowledge_with_clocks_and_observer(
    repository_root: &Path,
    knowledge_id: &str,
    content_revision: &str,
    clock: &dyn Clock,
    scan_clock: &dyn ElapsedClock,
    observer: &mut dyn KnowledgeMutationObserver,
) -> Result<(), MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let scan_deadline = ScanDeadline::start(scan_clock, DEFAULT_KNOWLEDGE_SCAN_LIMITS);
    let knowledge = existing_knowledge_directory_with_observer(&repository_root, observer)?
        .ok_or_else(|| MkoError::new("knowledge_not_found", "no knowledge note has this ID"))?;
    let mut document =
        find_knowledge_in_directory_with_deadline(&knowledge, knowledge_id, &scan_deadline)?
            .ok_or_else(|| MkoError::new("knowledge_not_found", "no knowledge note has this ID"))?;
    validate_document_revision(&document, content_revision)?;
    document.record.review.status = ReviewState::Reviewed;
    document.record.review.reviewed_at = Some(clock.now_utc());
    document.record.approved_revision = Some(content_revision.to_owned());
    let rendered = render_markdown(&document.record, &document.body)?;
    observer.before_publication()?;
    write_replace_capability_compare_exchange_validated_at_commit(
        &knowledge.directory,
        Path::new(&document.filename),
        &document.snapshot,
        rendered.as_bytes(),
        || Ok(()),
        || {
            validate_expected_knowledge_snapshot(
                &knowledge.directory,
                Path::new(&document.filename),
                &document.snapshot,
                content_revision,
            )
        },
    )
    .map_err(map_knowledge_publication_error)?;
    verify_public_knowledge_publication(
        &knowledge,
        Path::new(&document.filename),
        rendered.as_bytes(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConceptMatch {
    pub asset_id: String,
    pub title: String,
    pub name: String,
    pub kind: ConceptKind,
    pub locator: Option<String>,
    pub knowledge_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeSearchQuery {
    pub term: String,
    pub kind: Option<ConceptKind>,
    pub tag: Option<String>,
}

pub fn search_knowledge(
    repository_root: &Path,
    query: &KnowledgeSearchQuery,
) -> Result<Vec<ConceptMatch>, MkoError> {
    let clock = MonotonicElapsedClock::start();
    search_knowledge_with_scan(
        repository_root,
        query,
        DEFAULT_KNOWLEDGE_SCAN_LIMITS,
        &clock,
        &mut (),
    )
}

#[doc(hidden)]
pub fn search_knowledge_with_scan(
    repository_root: &Path,
    query: &KnowledgeSearchQuery,
    limits: ScanLimits,
    clock: &dyn ElapsedClock,
    observer: &mut dyn KnowledgeScanObserver,
) -> Result<Vec<ConceptMatch>, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let term = query.term.to_lowercase();
    let mut matches = Vec::new();
    for document in read_knowledge_documents_with_scan(&repository_root, limits, clock, observer)? {
        let knowledge_path = repository_relative(&repository_root, &document.path)?;
        for concept in &document.record.concepts {
            if let Some(kind) = &query.kind
                && &concept.kind != kind
            {
                continue;
            }
            if let Some(tag) = &query.tag
                && !concept.tags.iter().any(|value| value == tag)
            {
                continue;
            }
            if !term.is_empty() {
                let haystack = format!(
                    "{} {} {} {}",
                    concept.name,
                    concept.body,
                    concept.tags.join(" "),
                    concept_kind_label(&concept.kind),
                )
                .to_lowercase();
                if !haystack.contains(&term) {
                    continue;
                }
            }
            matches.push(ConceptMatch {
                asset_id: document.record.asset_id.clone(),
                title: document.record.title.clone(),
                name: concept.name.clone(),
                kind: concept.kind.clone(),
                locator: concept.locator.clone(),
                knowledge_path: knowledge_path.clone(),
            });
        }
    }
    Ok(matches)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeValidationIssue {
    pub code: String,
    pub path: String,
    pub message: String,
}

/// Validates a knowledge note's self-contained record shape and review-state
/// consistency. Does not check that `asset_id` refers to an existing Asset
/// Registry record; callers with a materialized asset set (such as
/// `check.rs`) should verify that relation separately, mirroring how
/// `check.rs::validate_source` checks Source-to-Asset relations directly
/// rather than inside `asset_validation.rs`.
pub fn validate_knowledge_record(
    path: &str,
    record: &KnowledgeRecord,
    body: &str,
) -> Vec<KnowledgeValidationIssue> {
    let mut issues = Vec::new();
    let asset_hash = record
        .asset_id
        .strip_prefix("personal-asset-")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        });
    let identity_valid = asset_hash.is_some_and(|hash| {
        record.id == format!("personal-knowledge-{hash}")
            && record.generation.asset_fingerprint == format!("sha256:{hash}")
    });
    if record.record_type != "knowledge"
        || record.schema_version != 1
        || !identity_valid
        || record.generation.processor_version != "knowledge-v1"
        || record.generation.prompt_version != "codex-knowledge-v1"
    {
        issues.push(KnowledgeValidationIssue {
            code: "knowledge_invalid".into(),
            path: path.into(),
            message: "knowledge identity, schema, generation, or fingerprint is not canonical"
                .into(),
        });
    }

    let actual = match calculate_knowledge_revision(record, body) {
        Ok(revision) => revision,
        Err(error) => {
            issues.push(KnowledgeValidationIssue {
                code: error.code().into(),
                path: path.into(),
                message: error.message().into(),
            });
            return issues;
        }
    };
    if record.content_revision != actual {
        issues.push(KnowledgeValidationIssue {
            code: "revision_mismatch".into(),
            path: path.into(),
            message: "stored content_revision does not match recomputed knowledge content".into(),
        });
    }

    let review_valid = match record.review.status {
        ReviewState::Unreviewed => {
            record.review.reviewed_at.is_none()
                && record
                    .approved_revision
                    .as_deref()
                    .is_none_or(is_sha256_revision)
        }
        ReviewState::Reviewed => {
            record.approved_revision.as_deref() == Some(actual.as_str())
                && record.review.reviewed_at.is_some()
        }
    };
    if !review_valid {
        issues.push(KnowledgeValidationIssue {
            code: "review_invalid".into(),
            path: path.into(),
            message: "knowledge review status and approved_revision/reviewed_at are inconsistent"
                .into(),
        });
    }

    let original_inputs = record
        .concepts
        .iter()
        .map(|concept| ConceptInput {
            name: concept.name.clone(),
            kind: concept.kind.clone(),
            body: concept.body.clone(),
            tags: concept.tags.clone(),
            locator: concept.locator.clone(),
        })
        .collect::<Vec<_>>();
    let mut response = KnowledgeResponse {
        synthesis: "validation placeholder".into(),
        concepts: original_inputs.clone(),
    };
    if normalize_and_validate_knowledge(&mut response).is_err()
        || response.concepts != original_inputs
    {
        issues.push(KnowledgeValidationIssue {
            code: "concept_invalid".into(),
            path: path.into(),
            message: "knowledge concepts must satisfy the canonical durable constraints".into(),
        });
    }
    let expected_ids = assign_concept_ids(&response.concepts);
    let ids_valid = record
        .concepts
        .iter()
        .map(|concept| concept.id.as_str())
        .eq(expected_ids.iter().map(|concept| concept.id.as_str()));
    if !ids_valid {
        issues.push(KnowledgeValidationIssue {
            code: "concept_id_invalid".into(),
            path: path.into(),
            message: "knowledge concept IDs must be canonical and unique within the note".into(),
        });
    }

    issues
}

pub fn validate_knowledge_asset_contract(
    path: &str,
    record: &KnowledgeRecord,
    asset: &AssetRecord,
) -> Vec<KnowledgeValidationIssue> {
    let expected_id = asset
        .id
        .replacen("personal-asset-", "personal-knowledge-", 1);
    let expected_path = knowledge_filename(asset.created_at, &asset.title, &expected_id)
        .map(|filename| format!("knowledge/{filename}"));
    if record.id != expected_id
        || record.asset_id != asset.id
        || record.title != asset.title
        || record.generation.asset_fingerprint != asset.fingerprint.value
        || expected_path.as_deref().ok() != Some(path)
    {
        vec![KnowledgeValidationIssue {
            code: "knowledge_invalid".into(),
            path: path.into(),
            message: "knowledge ID, path, title, or Asset fingerprint link is not canonical".into(),
        }]
    } else {
        Vec::new()
    }
}

fn is_sha256_revision(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::{fs, path::Path, sync::mpsc, thread, time::Duration};

    use cap_std::{ambient_authority, fs::Dir};

    use super::read_knowledge_snapshot_with_before_open;

    #[test]
    fn mutation_snapshot_rejects_a_fifo_swap_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record.md");
        let saved = root.path().join("record.saved");
        fs::write(&path, b"snapshot").unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let fifo = path.clone();
        let (sender, receiver) = mpsc::channel();

        let worker = thread::spawn(move || {
            let result = read_knowledge_snapshot_with_before_open(
                &directory,
                Path::new("record.md"),
                || {
                    fs::rename(&path, &saved).unwrap();
                    assert!(
                        std::process::Command::new("mkfifo")
                            .arg(&path)
                            .status()
                            .unwrap()
                            .success()
                    );
                },
            );
            sender.send(result).unwrap();
        });

        let result = match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(fs::OpenOptions::new().write(true).open(&fifo).unwrap());
                let _ = receiver.recv_timeout(Duration::from_secs(1));
                worker.join().unwrap();
                panic!("mutation snapshot blocked while opening a FIFO replacement");
            }
            Err(error) => panic!("mutation snapshot worker disconnected: {error}"),
        };
        worker.join().unwrap();

        assert_eq!(result.unwrap_err().code(), "knowledge_scan_limit");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use cap_std::{ambient_authority, fs::Dir};

    use super::sync_knowledge_directory;

    #[test]
    fn first_create_directory_sync_uses_the_documented_windows_noop_contract() {
        let root = tempfile::tempdir().unwrap();
        let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();

        sync_knowledge_directory(&directory).unwrap();
    }
}
