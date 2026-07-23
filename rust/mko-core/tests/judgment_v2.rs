use chrono::{TimeZone, Utc};
use mko_core::judgment_v2::{
    JudgmentAnnotationV2, JudgmentAuthorshipV2, JudgmentPublicationOutcomeV2, prepare_judgment_v2,
    publish_judgment_v2,
};
use mko_core::{clock::Clock, scaffold_v2::scaffold_personal_kb_v2};
use serde_json::Value;

const KNOWLEDGE_ID: &str =
    "personal-knowledge-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REVISION: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Clone, Copy)]
struct FixedClock(chrono::DateTime<Utc>);

impl FixedClock {
    fn new(value: chrono::DateTime<Utc>) -> Self {
        Self(value)
    }
}

impl Clock for FixedClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        self.0
    }
}

#[test]
fn prepares_exact_echo_confirmed_conversation_judgment() {
    let prepared = prepare_judgment_v2(
        KNOWLEDGE_ID,
        REVISION,
        "내 판단은 이렇다.\r\n추가 검증이 필요하다.",
        JudgmentAuthorshipV2::UserConfirmedViaConversation,
        Utc.with_ymd_and_hms(2026, 7, 22, 1, 2, 3).unwrap(),
    )
    .unwrap();

    assert_eq!(
        prepared.annotation.text,
        "내 판단은 이렇다.\n추가 검증이 필요하다."
    );
    assert_eq!(
        prepared.annotation.authorship,
        JudgmentAuthorshipV2::UserConfirmedViaConversation
    );
    assert!(
        String::from_utf8(prepared.markdown)
            .unwrap()
            .contains(&prepared.annotation.text)
    );
}

#[test]
fn schema_and_rust_round_trip_the_annotation() {
    let prepared = prepare_judgment_v2(
        KNOWLEDGE_ID,
        REVISION,
        "검증된 내 판단",
        JudgmentAuthorshipV2::UserConfirmedViaTty,
        Utc.with_ymd_and_hms(2026, 7, 22, 1, 2, 3).unwrap(),
    )
    .unwrap();
    let value = serde_json::to_value(&prepared.annotation).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/v2/judgment.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));
    let decoded: JudgmentAnnotationV2 = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, prepared.annotation);
}

#[test]
fn rejects_empty_oversized_control_and_bidi_text() {
    for text in ["", "\u{1b}[31mred", "abc\u{202e}txt"] {
        assert_eq!(
            prepare_judgment_v2(
                KNOWLEDGE_ID,
                REVISION,
                text,
                JudgmentAuthorshipV2::UserConfirmedViaConversation,
                Utc::now(),
            )
            .unwrap_err()
            .code(),
            "judgment_text_invalid"
        );
    }
    let oversized = "가".repeat(11_000);
    assert_eq!(
        prepare_judgment_v2(
            KNOWLEDGE_ID,
            REVISION,
            &oversized,
            JudgmentAuthorshipV2::UserConfirmedViaConversation,
            Utc::now(),
        )
        .unwrap_err()
        .code(),
        "judgment_text_invalid"
    );
}

#[test]
fn publishes_only_against_an_existing_exact_knowledge_revision() {
    let temp = tempfile::tempdir().unwrap();
    scaffold_personal_kb_v2(temp.path()).unwrap();
    let record = temp.path().join("knowledge").join(KNOWLEDGE_ID);
    std::fs::create_dir(&record).unwrap();
    std::fs::create_dir(record.join("revisions")).unwrap();
    let revision_bytes = b"canonical knowledge";
    let revision = mko_core::revision_v2::sha256_digest(revision_bytes);
    std::fs::write(
        record
            .join("revisions")
            .join(format!("{}.md", revision.replace(':', "-"))),
        revision_bytes,
    )
    .unwrap();
    let created_at = Utc.with_ymd_and_hms(2026, 7, 22, 1, 2, 3).unwrap();
    let prepared = prepare_judgment_v2(
        KNOWLEDGE_ID,
        &revision,
        "내 판단",
        JudgmentAuthorshipV2::UserConfirmedViaConversation,
        created_at,
    )
    .unwrap();
    let clock = FixedClock::new(created_at);

    let created = publish_judgment_v2(temp.path(), &prepared, &clock).unwrap();
    let existing = publish_judgment_v2(temp.path(), &prepared, &clock).unwrap();
    assert_eq!(created.outcome, JudgmentPublicationOutcomeV2::Created);
    assert_eq!(existing.outcome, JudgmentPublicationOutcomeV2::Existing);
    assert_eq!(created.path, existing.path);

    let missing = prepare_judgment_v2(
        KNOWLEDGE_ID,
        REVISION,
        "없는 revision",
        JudgmentAuthorshipV2::UserConfirmedViaConversation,
        created_at,
    )
    .unwrap();
    assert_eq!(
        publish_judgment_v2(temp.path(), &missing, &clock)
            .unwrap_err()
            .code(),
        "judgment_revision_invalid"
    );
}
