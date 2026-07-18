use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new, write_replace},
    canonical_source::validate_canonical_source,
    clock::{Clock, SystemClock},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::AssetLock,
    model::{
        AssetStatus, Generation, Relations, Review, ReviewStatus, SemanticResponse, SourceRecord,
        SourceStatus,
    },
    path_policy::{canonical_directory, validate_ascii_slug},
    prepare::{PreparedSourceBundle, load_prepared_source_bundle},
    registry::{mark_asset_processed_with_clock, mark_asset_review_pending_with_clock, read_asset},
    revision::calculate_source_revision,
};

pub const MAX_SEMANTIC_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_SEMANTIC_STRING_BYTES: usize = 64 * 1024;
const MAX_SOURCE_SLUG_BYTES: usize = 96;

#[derive(Clone, Debug)]
pub struct WriteSourceRequest {
    repository_root: PathBuf,
    bundle_path: PathBuf,
    response: Vec<u8>,
    slug: Option<String>,
    replace_pending: bool,
    clear_stale_lock: bool,
}

impl WriteSourceRequest {
    pub fn new(
        repository_root: impl AsRef<Path>,
        bundle_path: impl AsRef<Path>,
        response: Vec<u8>,
    ) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            bundle_path: bundle_path.as_ref().to_path_buf(),
            response,
            slug: None,
            replace_pending: false,
            clear_stale_lock: false,
        }
    }

    pub fn with_slug(mut self, slug: Option<String>) -> Self {
        self.slug = slug;
        self
    }

    pub fn with_replace_pending(mut self, replace_pending: bool) -> Self {
        self.replace_pending = replace_pending;
        self
    }

    pub fn with_clear_stale_lock(mut self, clear_stale_lock: bool) -> Self {
        self.clear_stale_lock = clear_stale_lock;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteSourceResult {
    pub result: String,
    pub source_id: String,
    pub source_path: String,
    pub content_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceStateMismatch {
    pub code: String,
    pub source_id: String,
    pub asset_id: String,
    pub current_state: String,
    pub expected_state: String,
    pub safe_action: String,
}

#[derive(Clone, Debug)]
pub struct RepairSourceStateRequest {
    repository_root: PathBuf,
    asset_id: String,
    clear_stale_lock: bool,
}

impl RepairSourceStateRequest {
    pub fn new(repository_root: impl AsRef<Path>, asset_id: impl Into<String>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            asset_id: asset_id.into(),
            clear_stale_lock: false,
        }
    }

    pub fn with_clear_stale_lock(mut self, clear_stale_lock: bool) -> Self {
        self.clear_stale_lock = clear_stale_lock;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairSourceStateResult {
    pub result: String,
    pub source_id: String,
    pub asset_id: String,
}

pub fn parse_semantic_response(input: &[u8]) -> Result<SemanticResponse, MkoError> {
    if input.len() > MAX_SEMANTIC_RESPONSE_BYTES {
        return Err(schema_error("semantic response exceeds 1 MiB"));
    }
    let mut response: SemanticResponse = serde_json::from_slice(input)
        .map_err(|error| schema_error(format!("invalid semantic response: {error}")))?;
    normalize_and_validate_response(&mut response)?;
    Ok(response)
}

pub fn write_source_draft(request: WriteSourceRequest) -> Result<WriteSourceResult, MkoError> {
    write_source_draft_with_clock(request, &SystemClock)
}

pub fn write_source_draft_with_clock(
    request: WriteSourceRequest,
    clock: &dyn Clock,
) -> Result<WriteSourceResult, MkoError> {
    let bundle = load_prepared_source_bundle(&request.repository_root, &request.bundle_path)?;
    let repository_root = canonical_directory(&request.repository_root, "repository_root_invalid")?;
    if let Some(slug) = request.slug.as_deref() {
        validate_source_slug(slug)?;
    }
    let semantic = parse_semantic_response(&request.response)?;
    let _lock = AssetLock::acquire(
        &repository_root,
        &bundle.asset_id,
        "mko source write-draft",
        clock,
        request.clear_stale_lock,
    )?;
    let asset = read_asset(&repository_root, &bundle.asset_id)?;
    validate_bundle_against_asset(&bundle, &asset)?;
    if !matches!(
        asset.asset_status,
        AssetStatus::Extracted | AssetStatus::ReviewPending | AssetStatus::Processed
    ) {
        return Err(MkoError::new(
            "invalid_state_transition",
            "source draft requires an extracted or review_pending asset",
        ));
    }

    let sources = sources_directory(&repository_root)?;
    let existing = find_source(&sources, &bundle.source_id)?;
    let now = clock.now_utc();
    let body = render_source_body(&semantic);
    let (mut source, relative_path, destination, replacing) = if let Some(existing) = existing {
        validate_existing_source_document(&existing, &asset)
            .map_err(|error| existing_source_error(error.message()))?;
        if existing.record.status == SourceStatus::Approved
            || existing.record.review.status == ReviewStatus::Approved
        {
            return Err(MkoError::new(
                "approved_source_immutable",
                "approved Sources cannot be regenerated or overwritten",
            ));
        }
        if existing.record.status != SourceStatus::ReviewPending
            || existing.record.review.status != ReviewStatus::Pending
        {
            return Err(MkoError::new(
                "source_not_replaceable",
                "only a pending Source can be regenerated",
            ));
        }
        let candidate = source_record(&bundle, &semantic, existing.record.created_at, now, &body)?;
        if candidate.content_revision == existing.record.content_revision
            && calculate_source_revision(&existing.record, &existing.body)?
                == existing.record.content_revision
        {
            transition_asset_after_source(&repository_root, &bundle.asset_id, clock)?;
            return Ok(WriteSourceResult {
                result: "existing".into(),
                source_id: existing.record.id,
                source_path: repository_relative(&repository_root, &existing.path)?,
                content_revision: candidate.content_revision,
            });
        }
        if !request.replace_pending {
            return Err(MkoError::new(
                "replace_pending_required",
                "regenerating a pending Source requires --replace-pending",
            ));
        }
        let relative = repository_relative(&repository_root, &existing.path)?;
        (candidate, relative, existing.path, true)
    } else {
        let source = source_record(&bundle, &semantic, now, now, &body)?;
        let filename = source_filename(
            now,
            request.slug.as_deref(),
            &semantic.title,
            &bundle.source_id,
        )?;
        let destination = sources.join(&filename);
        (source, format!("sources/{filename}"), destination, false)
    };

    source.content_revision = calculate_source_revision(&source, &body)?;
    validate_canonical_source(&relative_path, &source, &body, &asset)?;
    let document = render_markdown(&source, &body)?;
    let result = if replacing {
        ensure_regular_source_destination(&destination)?;
        write_replace(&destination, document.as_bytes())?;
        "replaced"
    } else {
        match write_new(&destination, document.as_bytes(), |path| {
            validate_existing_source(path, &source, &body, &asset)
        })? {
            AtomicWriteResult::Created => "created",
            AtomicWriteResult::Existing => "existing",
        }
    };

    // The Source is already a complete pending System-of-Record document here. If this
    // second, independent write fails, `mko check` can report and repair the mismatch.
    transition_asset_after_source(&repository_root, &bundle.asset_id, clock)?;
    Ok(WriteSourceResult {
        result: result.into(),
        source_id: source.id,
        source_path: relative_path,
        content_revision: source.content_revision,
    })
}

pub fn source_state_mismatch_asset_ids(repository_root: &Path) -> Result<Vec<String>, MkoError> {
    let mut mismatches = source_state_mismatches(repository_root)?
        .into_iter()
        .map(|issue| issue.asset_id)
        .collect::<Vec<_>>();
    mismatches.sort();
    mismatches.dedup();
    Ok(mismatches)
}

pub fn source_state_mismatches(
    repository_root: &Path,
) -> Result<Vec<SourceStateMismatch>, MkoError> {
    let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
    let Some(sources) = existing_sources_directory(&repository_root)? else {
        return Ok(Vec::new());
    };
    let mut mismatches = Vec::new();
    for document in read_source_documents(&sources)? {
        if !matches!(
            document.record.status,
            SourceStatus::ReviewPending | SourceStatus::Approved
        ) {
            continue;
        }
        let Some(asset_id) = document.record.relations.asset_ids.first() else {
            return Err(existing_source_error(
                "canonical Source relation is missing",
            ));
        };
        let asset = read_asset(&repository_root, asset_id)
            .map_err(|error| existing_source_error(error.message()))?;
        validate_existing_source_document(&document, &asset)
            .map_err(|error| existing_source_error(error.message()))?;
        let expected = match document.record.status {
            SourceStatus::ReviewPending => AssetStatus::ReviewPending,
            SourceStatus::Approved => AssetStatus::Processed,
            _ => unreachable!(),
        };
        if asset.asset_status != expected {
            mismatches.push(SourceStateMismatch {
                code: "source_state_mismatch".into(),
                source_id: document.record.id,
                asset_id: asset.id.clone(),
                current_state: asset_status_name(&asset.asset_status).into(),
                expected_state: asset_status_name(&expected).into(),
                safe_action: format!(
                    "mko source repair-state --repo \"{}\" --asset-id {}",
                    repository_root.display(),
                    asset.id
                ),
            });
        }
    }
    mismatches.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    Ok(mismatches)
}

pub fn repair_source_state(
    request: RepairSourceStateRequest,
) -> Result<RepairSourceStateResult, MkoError> {
    repair_source_state_with_clock(request, &SystemClock)
}

pub fn repair_source_state_with_clock(
    request: RepairSourceStateRequest,
    clock: &dyn Clock,
) -> Result<RepairSourceStateResult, MkoError> {
    let repository_root = canonical_directory(&request.repository_root, "repository_root_invalid")?;
    let _lock = AssetLock::acquire(
        &repository_root,
        &request.asset_id,
        "mko source repair-state",
        clock,
        request.clear_stale_lock,
    )?;
    let asset = read_asset(&repository_root, &request.asset_id)?;
    if !matches!(
        asset.asset_status,
        AssetStatus::Extracted | AssetStatus::ReviewPending | AssetStatus::Processed
    ) {
        return Err(MkoError::new(
            "invalid_state_transition",
            "source state repair is limited to extracted, review_pending, or processed assets",
        ));
    }
    let sources = existing_sources_directory(&repository_root)?.ok_or_else(|| {
        MkoError::new(
            "relation_missing",
            "no canonical Source exists for this Asset",
        )
    })?;
    let source_id = asset.id.replacen("asset", "source", 1);
    let document = find_source(&sources, &source_id)?.ok_or_else(|| {
        MkoError::new(
            "relation_missing",
            "no canonical Source exists for this Asset",
        )
    })?;
    validate_existing_source_document(&document, &asset)
        .map_err(|error| existing_source_error(error.message()))?;
    let result = match document.record.status {
        SourceStatus::ReviewPending if document.record.review.status == ReviewStatus::Pending => {
            if asset.asset_status == AssetStatus::ReviewPending {
                "already_consistent"
            } else {
                mark_asset_review_pending_with_clock(&repository_root, &asset.id, clock)?;
                "repaired"
            }
        }
        SourceStatus::Approved if document.record.review.status == ReviewStatus::Approved => {
            if asset.asset_status == AssetStatus::Processed {
                "already_consistent"
            } else {
                mark_asset_processed_with_clock(&repository_root, &asset.id, clock)?;
                "repaired"
            }
        }
        _ => {
            return Err(existing_source_error(
                "state repair requires a valid pending or approved Source",
            ));
        }
    };
    Ok(RepairSourceStateResult {
        result: result.into(),
        source_id,
        asset_id: asset.id,
    })
}

fn transition_asset_after_source(
    repository_root: &Path,
    asset_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let asset = read_asset(repository_root, asset_id)?;
    if asset.asset_status == AssetStatus::ReviewPending {
        return Ok(());
    }
    mark_asset_review_pending_with_clock(repository_root, asset_id, clock)
}

fn validate_bundle_against_asset(
    bundle: &PreparedSourceBundle,
    asset: &crate::model::AssetRecord,
) -> Result<(), MkoError> {
    if bundle.asset_id != asset.id
        || bundle.fingerprint != asset.fingerprint
        || bundle.title_hint != asset.title
        || bundle.logical_path != asset.provider.locator
        || bundle.source_id != asset.id.replacen("asset", "source", 1)
    {
        return Err(MkoError::new(
            "bundle_invalid",
            "prepared Source bundle no longer matches the Asset Registry",
        ));
    }
    Ok(())
}

fn source_record(
    bundle: &PreparedSourceBundle,
    semantic: &SemanticResponse,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    body: &str,
) -> Result<SourceRecord, MkoError> {
    let mut source = SourceRecord {
        id: bundle.source_id.clone(),
        record_type: "source".into(),
        schema_version: 1,
        scope: "personal".into(),
        title: semantic.title.clone(),
        status: SourceStatus::ReviewPending,
        created_at,
        updated_at,
        tags: semantic.tags.clone(),
        domain: semantic.domain.clone(),
        ai_assisted: true,
        relations: Relations {
            asset_ids: vec![bundle.asset_id.clone()],
        },
        generation: Generation {
            extractor_name: bundle.extractor.name.clone(),
            extractor_version: bundle.extractor.version.clone(),
            core_version: bundle.core_version.clone(),
            processor_version: bundle.processor_version.clone(),
            prompt_version: bundle.prompt_version.clone(),
            asset_fingerprint: bundle.fingerprint.value.clone(),
        },
        content_revision: String::new(),
        review: Review {
            status: ReviewStatus::Pending,
            approved_revision: None,
            reviewed_at: None,
        },
        source_metadata: semantic.source_metadata.clone(),
    };
    source.content_revision = calculate_source_revision(&source, body)?;
    Ok(source)
}

fn render_source_body(response: &SemanticResponse) -> String {
    let source_metadata = render_source_metadata(response);
    format!(
        "# {}\n\n## Source Metadata\n\n{}\n\n## One-Sentence Summary\n\n{}\n\n## Problem\n\n{}\n\n## Method\n\n{}\n\n## Contributions\n\n{}\n\n## Reported Evidence\n\n{}\n\n## Stated Limitations\n\n{}\n\n## Domain Perspective\n\n{}\n\n## Implementation Considerations\n\n{}\n\n## Questions and Unknowns\n\n{}\n\n## Related Knowledge\n\n{}\n",
        response.title,
        source_metadata,
        canonical_section_text(&response.one_sentence_summary),
        canonical_section_text(&response.problem),
        canonical_section_text(&response.method),
        canonical_section_text(&response.contributions),
        canonical_section_text(&response.reported_evidence),
        canonical_section_text(&response.stated_limitations),
        canonical_section_text(&response.domain_perspective),
        canonical_section_text(&response.implementation_considerations),
        canonical_section_text(&response.questions_and_unknowns),
        canonical_section_text(&response.related_knowledge),
    )
    .replace("\r\n", "\n")
    .replace('\r', "\n")
    .nfc()
    .collect()
}

fn normalize_and_validate_response(response: &mut SemanticResponse) -> Result<(), MkoError> {
    normalize_string(&mut response.title)?;
    if response.title.trim().is_empty() || response.title.contains('\n') {
        return Err(schema_error(
            "title must be a non-empty single-line Markdown heading",
        ));
    }
    for value in &mut response.source_metadata.authors {
        normalize_string(value)?;
    }
    if let Some(doi) = &mut response.source_metadata.doi {
        normalize_string(doi)?;
    }
    for value in &mut response.tags {
        normalize_string(value)?;
    }
    for value in &mut response.domain {
        normalize_string(value)?;
    }
    for section in [
        &mut response.one_sentence_summary,
        &mut response.problem,
        &mut response.method,
        &mut response.contributions,
        &mut response.reported_evidence,
        &mut response.stated_limitations,
        &mut response.domain_perspective,
        &mut response.implementation_considerations,
        &mut response.questions_and_unknowns,
        &mut response.related_knowledge,
    ] {
        normalize_string(section)?;
    }
    validate_aggregate_semantic_limits(response)?;
    Ok(())
}

fn validate_aggregate_semantic_limits(response: &SemanticResponse) -> Result<(), MkoError> {
    validate_semantic_size(&response.source_metadata.authors.join(", "))?;
    validate_semantic_size(&response.tags.join("\n"))?;
    validate_semantic_size(&response.domain.join("\n"))?;
    validate_semantic_size(&render_source_metadata(response))?;
    for section in [
        &response.one_sentence_summary,
        &response.problem,
        &response.method,
        &response.contributions,
        &response.reported_evidence,
        &response.stated_limitations,
        &response.domain_perspective,
        &response.implementation_considerations,
        &response.questions_and_unknowns,
        &response.related_knowledge,
    ] {
        validate_semantic_size(&canonical_section_text(section))?;
    }
    Ok(())
}

fn render_source_metadata(response: &SemanticResponse) -> String {
    let authors = if response.source_metadata.authors.is_empty() {
        "Not reported".into()
    } else {
        response.source_metadata.authors.join(", ")
    };
    let publication_date = response
        .source_metadata
        .publication_date
        .map(|date| date.to_string())
        .unwrap_or_else(|| "Not reported".into());
    let doi = response
        .source_metadata
        .doi
        .as_deref()
        .unwrap_or("Not reported");
    format!(
        "- Authors: {}\n- Publication Date: {}\n- DOI: {}",
        canonical_section_text(&authors),
        publication_date,
        canonical_section_text(doi)
    )
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

fn normalize_string(value: &mut String) -> Result<(), MkoError> {
    *value = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    validate_semantic_size(value)
}

fn validate_semantic_size(value: &str) -> Result<(), MkoError> {
    if value.len() > MAX_SEMANTIC_STRING_BYTES {
        return Err(schema_error("normalized semantic section exceeds 64 KiB"));
    }
    Ok(())
}

fn source_filename(
    now: DateTime<Utc>,
    requested_slug: Option<&str>,
    title: &str,
    source_id: &str,
) -> Result<String, MkoError> {
    let hash = source_id
        .strip_prefix("personal-source-")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| MkoError::new("source_id_invalid", "invalid content-addressed Source ID"))?;
    let slug = if let Some(slug) = requested_slug {
        validate_source_slug(slug)?;
        slug.to_owned()
    } else {
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
        slug.truncate(MAX_SOURCE_SLUG_BYTES);
        while slug.ends_with('-') {
            slug.pop();
        }
        if slug.is_empty() || validate_source_slug(&slug).is_err() {
            "document".into()
        } else {
            slug
        }
    };
    let date = now.with_timezone(&Seoul).date_naive();
    Ok(format!("{date}-{slug}-{}.md", &hash[..12]))
}

fn validate_source_slug(slug: &str) -> Result<(), MkoError> {
    if slug.len() > MAX_SOURCE_SLUG_BYTES {
        return Err(MkoError::new(
            "invalid_slug",
            "Source slug must be at most 96 ASCII bytes",
        ));
    }
    validate_ascii_slug(slug)
}

fn sources_directory(repository_root: &Path) -> Result<PathBuf, MkoError> {
    let path = repository_root.join("sources");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(source_path_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path).map_err(|_| source_path_error())?;
        }
        Err(_) => return Err(source_path_error()),
    }
    let canonical = fs::canonicalize(&path).map_err(|_| source_path_error())?;
    if !canonical.starts_with(repository_root) {
        return Err(source_path_error());
    }
    reject_source_collisions(&canonical)?;
    Ok(canonical)
}

fn existing_sources_directory(repository_root: &Path) -> Result<Option<PathBuf>, MkoError> {
    let path = repository_root.join("sources");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) | Err(_) => return Err(source_path_error()),
    }
    let canonical = fs::canonicalize(&path).map_err(|_| source_path_error())?;
    if !canonical.starts_with(repository_root) {
        return Err(source_path_error());
    }
    reject_source_collisions(&canonical)?;
    Ok(Some(canonical))
}

fn reject_source_collisions(sources: &Path) -> Result<(), MkoError> {
    let mut names = HashSet::new();
    for entry in fs::read_dir(sources).map_err(|_| source_path_error())? {
        let entry = entry.map_err(|_| source_path_error())?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(source_path_error)?
            .nfc()
            .collect::<String>()
            .to_lowercase();
        if !names.insert(name) {
            return Err(MkoError::new(
                "path_collision",
                "sources contains a case or Unicode-normalization collision",
            ));
        }
    }
    Ok(())
}

struct SourceDocument {
    record: SourceRecord,
    body: String,
    path: PathBuf,
}

fn validate_existing_source_document(
    document: &SourceDocument,
    asset: &crate::model::AssetRecord,
) -> Result<(), MkoError> {
    let filename = document
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| existing_source_error("existing Source filename is not UTF-8"))?;
    validate_canonical_source(
        &format!("sources/{filename}"),
        &document.record,
        &document.body,
        asset,
    )
    .map(|_| ())
    .map_err(|error| existing_source_error(error.message()))
}

fn find_source(sources: &Path, source_id: &str) -> Result<Option<SourceDocument>, MkoError> {
    let mut matches = read_source_documents(sources)?
        .into_iter()
        .filter(|document| document.record.id == source_id);
    let found = matches.next();
    if matches.next().is_some() {
        return Err(MkoError::new(
            "source_conflict",
            "multiple canonical Source documents use the same ID",
        ));
    }
    Ok(found)
}

fn read_source_documents(sources: &Path) -> Result<Vec<SourceDocument>, MkoError> {
    let mut documents = Vec::new();
    for entry in fs::read_dir(sources)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?
    {
        let entry = entry.map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(source_path_error());
        }
        let input = fs::read_to_string(&path)
            .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
        let parsed = parse_markdown::<SourceRecord>(&input)
            .map_err(|error| existing_source_error(error.message()))?;
        documents.push(SourceDocument {
            record: parsed.metadata,
            body: parsed.body,
            path,
        });
    }
    Ok(documents)
}

fn validate_existing_source(
    path: &Path,
    expected: &SourceRecord,
    expected_body: &str,
    asset: &crate::model::AssetRecord,
) -> Result<(), MkoError> {
    let input = fs::read_to_string(path)
        .map_err(|error| MkoError::new("source_unreadable", error.to_string()))?;
    let existing = parse_markdown::<SourceRecord>(&input)
        .map_err(|error| existing_source_error(error.message()))?;
    let document = SourceDocument {
        record: existing.metadata,
        body: existing.body,
        path: path.to_path_buf(),
    };
    validate_existing_source_document(&document, asset)?;
    if document.record.id != expected.id
        || document.record.relations != expected.relations
        || document.record.content_revision != calculate_source_revision(expected, expected_body)?
    {
        return Err(MkoError::new(
            "source_conflict",
            "deterministic Source path contains different content",
        ));
    }
    Ok(())
}

fn ensure_regular_source_destination(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "source_not_found"
        } else {
            "source_unreadable"
        };
        MkoError::new(code, error.to_string())
    })?;
    if !metadata.file_type().is_file() {
        return Err(source_path_error());
    }
    Ok(())
}

fn repository_relative(repository_root: &Path, path: &Path) -> Result<String, MkoError> {
    path.strip_prefix(repository_root)
        .map_err(|_| source_path_error())?
        .to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(source_path_error)
}

fn schema_error(message: impl Into<String>) -> MkoError {
    MkoError::new("schema_invalid", message)
}

fn existing_source_error(message: impl Into<String>) -> MkoError {
    MkoError::new("existing_source_invalid", message)
}

fn asset_status_name(status: &AssetStatus) -> &'static str {
    match status {
        AssetStatus::Registered => "registered",
        AssetStatus::Extracted => "extracted",
        AssetStatus::ReviewPending => "review_pending",
        AssetStatus::Processed => "processed",
        AssetStatus::Changed => "changed",
        AssetStatus::Missing => "missing",
        AssetStatus::Superseded => "superseded",
        AssetStatus::Failed => "failed",
    }
}

fn source_path_error() -> MkoError {
    MkoError::new(
        "source_path_invalid",
        "Source path must remain in a real sources directory",
    )
}
