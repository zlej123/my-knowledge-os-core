use chrono::{DateTime, NaiveDate, Utc};
use mko_core::model::{
    Generation, Relations, Review, ReviewStatus, SourceMetadata, SourceRecord, SourceStatus,
};

#[allow(dead_code)]
pub fn fixture_source() -> SourceRecord {
    let timestamp = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    SourceRecord {
        id: "personal-source-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .into(),
        record_type: "source".into(),
        schema_version: 1,
        scope: "personal".into(),
        title: "Fixture Source".into(),
        status: SourceStatus::ReviewPending,
        created_at: timestamp,
        updated_at: timestamp,
        tags: vec!["fixture".into()],
        domain: vec!["testing".into()],
        ai_assisted: true,
        relations: Relations {
            asset_ids: vec![
                "personal-asset-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
            ],
        },
        generation: Generation {
            extractor_name: "fixture-extractor".into(),
            extractor_version: "1.0.0".into(),
            core_version: "0.1.0".into(),
            processor_version: "1.0.0".into(),
            prompt_version: "1.0.0".into(),
            asset_fingerprint:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        },
        content_revision: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .into(),
        review: Review {
            status: ReviewStatus::Pending,
            approved_revision: None,
            reviewed_at: None,
        },
        source_metadata: SourceMetadata {
            authors: vec!["Fixture Author".into()],
            publication_date: Some(NaiveDate::from_ymd_opt(2026, 7, 18).unwrap()),
            doi: Some("10.1000/original".into()),
        },
    }
}
