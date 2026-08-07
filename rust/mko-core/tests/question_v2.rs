use chrono::{Duration, Utc};
use mko_core::{
    question_v2::{
        MAX_QUESTION_CHARS, QuestionRecordV2, append_question_v2, questions_for_asset_v2,
    },
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

fn asset(fill: char) -> String {
    format!("personal-asset-{}", String::from(fill).repeat(64))
}

// The log is read as a history — "what was I trying to understand" — so it
// keeps order, and it is append-only so a later revision of the note cannot
// disturb what was asked before it.
#[test]
fn questions_accumulate_in_the_order_they_were_asked() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let asset_id = asset('a');
    let base = Utc::now();

    for (offset, text, kept) in [
        (0, "ADC 샘플링 레이트가 왜 이 값인지", false),
        (60, "클럭 도메인 분리 이유", true),
    ] {
        append_question_v2(
            &repository,
            &QuestionRecordV2::new(&asset_id, text, base + Duration::seconds(offset), kept),
        )
        .unwrap();
    }

    let questions = questions_for_asset_v2(&repository, &asset_id).unwrap();

    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].text, "ADC 샘플링 레이트가 왜 이 값인지");
    assert!(!questions[0].became_unit);
    assert_eq!(questions[1].text, "클럭 도메인 분리 이유");
    assert!(questions[1].became_unit);
    assert!(
        questions_for_asset_v2(&repository, &asset('b'))
            .unwrap()
            .is_empty(),
        "another Asset's questions are not this Asset's"
    );
}

// Asking the same thing twice is a fact about the session, not a duplicate to
// collapse: it says the first answer did not land.
#[test]
fn the_same_question_asked_twice_is_kept_twice() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let asset_id = asset('a');
    let base = Utc::now();

    append_question_v2(
        &repository,
        &QuestionRecordV2::new(&asset_id, "클럭 도메인 분리 이유", base, false),
    )
    .unwrap();
    append_question_v2(
        &repository,
        &QuestionRecordV2::new(
            &asset_id,
            "클럭 도메인 분리 이유",
            base + Duration::seconds(600),
            true,
        ),
    )
    .unwrap();

    assert_eq!(
        questions_for_asset_v2(&repository, &asset_id)
            .unwrap()
            .len(),
        2
    );
}

// A knowledge base scaffolded before questions existed must start logging
// without a migration, exactly as attempts do.
#[test]
fn a_knowledge_base_without_the_directory_starts_logging_anyway() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    std::fs::remove_dir_all(repository.join("assets/questions")).ok();

    append_question_v2(
        &repository,
        &QuestionRecordV2::new(&asset('a'), "물어본 것", Utc::now(), false),
    )
    .unwrap();

    assert_eq!(
        questions_for_asset_v2(&repository, &asset('a'))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_question_that_could_not_be_read_back_is_refused() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let at = Utc::now();

    for (asset_id, text) in [
        (asset('a'), " ".repeat(4)),
        (asset('a'), "가".repeat(MAX_QUESTION_CHARS + 1)),
        ("not-an-asset".to_owned(), "물어본 것".to_owned()),
    ] {
        assert_eq!(
            append_question_v2(
                &repository,
                &QuestionRecordV2::new(&asset_id, &text, at, false)
            )
            .unwrap_err()
            .code(),
            "question_invalid"
        );
    }
}
