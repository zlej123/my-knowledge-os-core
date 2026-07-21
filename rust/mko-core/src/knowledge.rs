use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new, write_replace},
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    path_policy::canonical_directory,
    registry::read_asset,
};

pub const MAX_KNOWLEDGE_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_KNOWLEDGE_STRING_BYTES: usize = 64 * 1024;
const MAX_KNOWLEDGE_SLUG_BYTES: usize = 96;

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
            asset_id: asset_id.into(),
            response,
            replace: false,
        }
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
    let repository_root = canonical_directory(&request.repository_root, "repository_root_invalid")?;
    let mut response = parse_knowledge_response(&request.response)?;
    normalize_and_validate_knowledge(&mut response)?;
    let asset = read_asset(&repository_root, &request.asset_id)
        .map_err(|error| MkoError::new("asset_not_found", error.message()))?;

    let hash = asset.id.strip_prefix("personal-asset-").ok_or_else(|| {
        MkoError::new(
            "asset_id_invalid",
            "asset ID must be a content-addressed asset ID",
        )
    })?;
    let expected_id = format!("personal-knowledge-{hash}");

    let existing = find_knowledge(&repository_root, &expected_id)?;
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
        write_replace(&existing.path, document.as_bytes())?;
        return Ok(WriteKnowledgeResult {
            result: "replaced".into(),
            knowledge_id: record.id,
            knowledge_path: repository_relative(&repository_root, &existing.path)?,
            content_revision: record.content_revision,
        });
    }

    let knowledge_dir = knowledge_directory(&repository_root)?;
    let filename = knowledge_filename(clock.now_utc(), &asset.title, &expected_id)?;
    let destination = knowledge_dir.join(&filename);
    let document = render_markdown(&record, &body)?;
    match write_new(&destination, document.as_bytes(), |path| {
        validate_existing_knowledge(path, &record)
    })? {
        AtomicWriteResult::Created | AtomicWriteResult::Existing => Ok(WriteKnowledgeResult {
            result: "created".into(),
            knowledge_id: record.id,
            knowledge_path: format!("knowledge/{filename}"),
            content_revision: record.content_revision,
        }),
    }
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

fn knowledge_directory(repository_root: &Path) -> Result<PathBuf, MkoError> {
    let path = repository_root.join("knowledge");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(knowledge_path_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path).map_err(|_| knowledge_path_error())?;
        }
        Err(_) => return Err(knowledge_path_error()),
    }
    let canonical = fs::canonicalize(&path).map_err(|_| knowledge_path_error())?;
    if !canonical.starts_with(repository_root) {
        return Err(knowledge_path_error());
    }
    reject_knowledge_collisions(&canonical)?;
    Ok(canonical)
}

fn reject_knowledge_collisions(knowledge: &Path) -> Result<(), MkoError> {
    let mut names = HashSet::new();
    for entry in fs::read_dir(knowledge).map_err(|_| knowledge_path_error())? {
        let entry = entry.map_err(|_| knowledge_path_error())?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(knowledge_path_error)?
            .nfc()
            .collect::<String>()
            .to_lowercase();
        if !names.insert(name) {
            return Err(MkoError::new(
                "path_collision",
                "knowledge contains a case or Unicode-normalization collision",
            ));
        }
    }
    Ok(())
}

fn find_knowledge(
    repository_root: &Path,
    knowledge_id: &str,
) -> Result<Option<KnowledgeDocument>, MkoError> {
    let mut matches = read_knowledge_documents(repository_root)?
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
    let path = repository_root.join("knowledge");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(_) | Err(_) => return Err(knowledge_path_error()),
    }
    let mut documents = Vec::new();
    for entry in fs::read_dir(&path)
        .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?
    {
        let entry =
            entry.map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        let entry_path = entry.path();
        if entry_path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let metadata = fs::symlink_metadata(&entry_path)
            .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(knowledge_path_error());
        }
        let input = fs::read_to_string(&entry_path)
            .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
        let parsed = parse_markdown::<KnowledgeRecord>(&input)
            .map_err(|error| existing_knowledge_error(error.message()))?;
        documents.push(KnowledgeDocument {
            record: parsed.metadata,
            body: parsed.body,
            path: entry_path,
        });
    }
    Ok(documents)
}

fn validate_existing_knowledge(path: &Path, expected: &KnowledgeRecord) -> Result<(), MkoError> {
    let input = fs::read_to_string(path)
        .map_err(|error| MkoError::new("knowledge_unreadable", error.to_string()))?;
    let existing = parse_markdown::<KnowledgeRecord>(&input)
        .map_err(|error| existing_knowledge_error(error.message()))?;
    if existing.metadata.id != expected.id
        || existing.metadata.content_revision != expected.content_revision
    {
        return Err(MkoError::new(
            "knowledge_conflict",
            "deterministic knowledge path contains different content",
        ));
    }
    Ok(())
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingKnowledge {
    pub knowledge_id: String,
    pub asset_id: String,
    pub title: String,
    pub knowledge_path: String,
    pub content_revision: String,
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
            Ok(PendingKnowledge {
                knowledge_id: document.record.id,
                asset_id: document.record.asset_id,
                title: document.record.title,
                knowledge_path,
                content_revision: document.record.content_revision,
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
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let mut document = find_knowledge(&repository_root, knowledge_id)?
        .ok_or_else(|| MkoError::new("knowledge_not_found", "no knowledge note has this ID"))?;
    if document.record.content_revision != content_revision {
        return Err(MkoError::new(
            "knowledge_revision_mismatch",
            "knowledge note content_revision has changed since selection",
        ));
    }
    document.record.review.status = ReviewState::Reviewed;
    document.record.review.reviewed_at = Some(clock.now_utc());
    document.record.approved_revision = Some(content_revision.to_owned());
    let rendered = render_markdown(&document.record, &document.body)?;
    write_replace(&document.path, rendered.as_bytes())
}
