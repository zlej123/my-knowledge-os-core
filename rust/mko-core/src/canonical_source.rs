use chrono_tz::Asia::Seoul;

use crate::{
    error::MkoError,
    model::{AssetRecord, ReviewStatus, SourceRecord, SourceStatus},
    pdf::{EXTRACTOR_NAME, EXTRACTOR_VERSION},
    prepare::{PROCESSOR_VERSION, PROMPT_VERSION},
    revision::calculate_source_revision,
    version::KNOWLEDGE_CONTRACT_VERSION,
};

const BODY_SECTIONS: [&str; 11] = [
    "Source Metadata",
    "One-Sentence Summary",
    "Problem",
    "Method",
    "Contributions",
    "Reported Evidence",
    "Stated Limitations",
    "Domain Perspective",
    "Implementation Considerations",
    "Questions and Unknowns",
    "Related Knowledge",
];

pub fn validate_canonical_source(
    repository_path: &str,
    source: &SourceRecord,
    body: &str,
    asset: &AssetRecord,
) -> Result<String, MkoError> {
    let expected_source_id = asset.id.replacen("asset", "source", 1);
    if source.id != expected_source_id
        || source.record_type != "source"
        || source.schema_version != 1
        || source.scope != "personal"
        || !source.ai_assisted
        || source.relations.asset_ids.as_slice() != [asset.id.as_str()]
        || source.generation.extractor_name != EXTRACTOR_NAME
        || source.generation.extractor_version != EXTRACTOR_VERSION
        || source.generation.core_version != KNOWLEDGE_CONTRACT_VERSION
        || source.generation.processor_version != PROCESSOR_VERSION
        || source.generation.prompt_version != PROMPT_VERSION
        || source.generation.asset_fingerprint != asset.fingerprint.value
        || !canonical_source_path(repository_path, source)
        || !canonical_body_shape(source, body)
    {
        return Err(MkoError::new(
            "source_invalid",
            "Source is not the canonical v0.1 document for its Asset",
        ));
    }

    let actual_revision = calculate_source_revision(source, body)?;
    if source.content_revision != actual_revision {
        return Err(MkoError::new(
            "revision_mismatch",
            "stored content_revision does not match the canonical Source content",
        ));
    }
    validate_review(source, &actual_revision)?;
    Ok(actual_revision)
}

fn validate_review(source: &SourceRecord, actual_revision: &str) -> Result<(), MkoError> {
    let valid = match source.status {
        SourceStatus::ReviewPending => {
            source.review.status == ReviewStatus::Pending
                && source.review.approved_revision.is_none()
                && source.review.reviewed_at.is_none()
        }
        SourceStatus::Approved => {
            source.review.status == ReviewStatus::Approved
                && source.review.approved_revision.as_deref() == Some(actual_revision)
                && source.review.reviewed_at.is_some()
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
                source.review.approved_revision.as_deref() == Some(actual_revision)
                    && source.review.reviewed_at.is_some()
            }
            ReviewStatus::Rejected => {
                source.review.approved_revision.is_none() && source.review.reviewed_at.is_some()
            }
        },
    };
    if valid {
        Ok(())
    } else {
        Err(MkoError::new(
            if source.review.status == ReviewStatus::Approved {
                "approval_stale"
            } else {
                "review_invalid"
            },
            "Source status and review metadata are inconsistent",
        ))
    }
}

fn canonical_source_path(path: &str, source: &SourceRecord) -> bool {
    let Some(hash) = source.id.strip_prefix("personal-source-").filter(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }) else {
        return false;
    };
    let Some(filename) = path.strip_prefix("sources/") else {
        return false;
    };
    if filename.contains('/') || filename.contains('\\') {
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

fn canonical_body_shape(source: &SourceRecord, body: &str) -> bool {
    if body.contains('\r') || !body.ends_with('\n') {
        return false;
    }
    let mut headings = vec![format!("# {}", source.title)];
    headings.extend(BODY_SECTIONS.map(|section| format!("## {section}")));
    let actual = body
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect::<Vec<_>>();
    if actual != headings.iter().map(String::as_str).collect::<Vec<_>>() {
        return false;
    }
    if !body.starts_with(&format!("# {}\n\n", source.title)) {
        return false;
    }
    BODY_SECTIONS
        .iter()
        .all(|section| body.contains(&format!("\n\n## {section}\n\n")))
}
