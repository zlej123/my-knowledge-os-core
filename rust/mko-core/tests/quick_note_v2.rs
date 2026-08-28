use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    quick_note_v2::{
        QuickNotePublicationOutcomeV2, list_quick_notes_v2, prepare_quick_note_v2,
        publish_quick_note_v2, search_quick_notes_v2,
    },
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
fn exact_confirmation_publishes_normalized_user_text_once() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let created_at = "2026-07-31T01:02:03Z".parse().unwrap();
    let prepared = prepare_quick_note_v2("line one\r\nline two\n", created_at).unwrap();
    assert!(prepared.input_changed);
    assert_eq!(prepared.note.text, "line one\nline two");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/v2/quick-note.schema.json")).unwrap();
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&serde_json::to_value(&prepared.note).unwrap())
    );

    let error =
        publish_quick_note_v2(root.path(), &prepared, "y", &FixedClock(created_at)).unwrap_err();
    assert_eq!(error.code(), "quick_note_confirmation_mismatch");
    assert!(
        root.path()
            .join("notes")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );

    let first = publish_quick_note_v2(
        root.path(),
        &prepared,
        &prepared.confirmation_phrase,
        &FixedClock(created_at),
    )
    .unwrap();
    let second = publish_quick_note_v2(
        root.path(),
        &prepared,
        &prepared.confirmation_phrase,
        &FixedClock(created_at),
    )
    .unwrap();
    assert_eq!(first.outcome, QuickNotePublicationOutcomeV2::Created);
    assert_eq!(second.outcome, QuickNotePublicationOutcomeV2::Existing);

    let notes = list_quick_notes_v2(root.path()).unwrap();
    assert_eq!(notes, vec![prepared.note.clone()]);
    assert_eq!(
        search_quick_notes_v2(root.path(), "LINE TWO").unwrap(),
        notes
    );
}

#[test]
fn search_requires_all_whitespace_tokens_and_normalizes_nfd_queries() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let created_at = "2026-07-31T01:02:03Z".parse().unwrap();
    let prepared =
        prepare_quick_note_v2("학습률을 크게 개선하는 방법을 정리했다.", created_at).unwrap();
    publish_quick_note_v2(
        root.path(),
        &prepared,
        &prepared.confirmation_phrase,
        &FixedClock(created_at),
    )
    .unwrap();

    // Both tokens are present but not adjacent: token-AND, not a single
    // substring test, must find this.
    assert_eq!(
        search_quick_notes_v2(root.path(), "학습률 개선").unwrap(),
        vec![prepared.note.clone()]
    );
    assert!(
        search_quick_notes_v2(root.path(), "학습률 존재하지않는단어")
            .unwrap()
            .is_empty()
    );

    // Stored text is NFC-normalized at publish time; a macOS-style NFD query
    // must still match the canonically identical stored text (D10).
    let nfd_query: String = "학습률 개선".nfd().collect();
    assert_ne!(nfd_query, "학습률 개선".nfc().collect::<String>());
    assert_eq!(
        search_quick_notes_v2(root.path(), &nfd_query).unwrap(),
        vec![prepared.note]
    );
}

#[test]
fn empty_control_and_bidi_text_never_reaches_publication() {
    let created_at = "2026-07-31T01:02:03Z".parse().unwrap();
    for text in ["", "\n", "hello\u{0007}", "left\u{202e}right"] {
        assert_eq!(
            prepare_quick_note_v2(text, created_at).unwrap_err().code(),
            "quick_note_text_invalid"
        );
    }
}
