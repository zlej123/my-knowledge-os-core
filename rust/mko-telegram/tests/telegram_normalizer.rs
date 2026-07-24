use mko_core::capture_v1::{CaptureInputV1, SubjectScopeV1};
use mko_telegram::capture::{
    TelegramCaptureConfigV1, canonicalize_youtube_url_v1, normalize_telegram_update_v1,
};
use serde_json::json;

fn config() -> TelegramCaptureConfigV1 {
    TelegramCaptureConfigV1::new("personal", ["-100123"], ["12345"]).unwrap()
}

fn document_update(caption: &str) -> serde_json::Value {
    json!({
        "update_id": 9001,
        "message": {
            "message_id": 2002,
            "date": 1_784_838_400,
            "chat": { "id": -100123, "type": "private" },
            "from": { "id": 12345, "is_bot": false },
            "caption": caption,
            "document": {
                "file_id": "telegram_file_id",
                "file_unique_id": "unique_pdf_id",
                "file_name": "../../do-not-trust-me.pdf",
                "mime_type": "application/pdf",
                "file_size": 1234
            }
        }
    })
}

#[test]
fn normalizes_an_allowlisted_pdf_and_ignores_sender_filename() {
    let envelope = normalize_telegram_update_v1(
        &config(),
        &serde_json::to_vec(&document_update("/finance Important report")).unwrap(),
    )
    .unwrap();

    assert_eq!(envelope.selected_scope, Some(SubjectScopeV1::Finance));
    assert_eq!(
        envelope.received_at.to_rfc3339(),
        "2026-07-23T20:26:40+00:00"
    );
    assert!(envelope.capture_id.starts_with("cap_tg_"));
    assert_eq!(
        envelope.input,
        CaptureInputV1::TelegramPdf {
            file_id: "telegram_file_id".to_owned(),
            file_unique_id: "unique_pdf_id".to_owned(),
            declared_size_bytes: 1234,
            mime_type: "application/pdf".to_owned(),
        }
    );
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains("do-not-trust-me")
    );
}

#[test]
fn rejects_unauthorized_chat_or_sender() {
    let mut chat = document_update("/general");
    chat["message"]["chat"]["id"] = json!(-100124);
    assert_eq!(
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&chat).unwrap())
            .unwrap_err()
            .code(),
        "telegram_identity_unauthorized"
    );

    let mut sender = document_update("/general");
    sender["message"]["from"]["id"] = json!(12346);
    assert_eq!(
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&sender).unwrap())
            .unwrap_err()
            .code(),
        "telegram_identity_unauthorized"
    );
}

#[test]
fn rejects_malformed_and_oversized_pdf_documents() {
    let mut malformed = document_update("/general");
    malformed["message"]["document"]["mime_type"] = json!("application/octet-stream");
    assert_eq!(
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&malformed).unwrap())
            .unwrap_err()
            .code(),
        "telegram_pdf_invalid"
    );

    let mut oversized = document_update("/general");
    oversized["message"]["document"]["file_size"] = json!(52_428_801_u64);
    assert_eq!(
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&oversized).unwrap())
            .unwrap_err()
            .code(),
        "telegram_pdf_invalid"
    );
}

#[test]
fn normalizes_youtube_url_with_an_explicit_general_command() {
    let update = json!({
        "update_id": 9002,
        "message": {
            "message_id": 2003,
            "date": 1_784_838_401,
            "chat": { "id": -100123 },
            "from": { "id": 12345 },
            "text": "/general https://youtu.be/dQw4w9WgXcQ?si=tracking"
        }
    });
    let envelope =
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&update).unwrap()).unwrap();
    assert_eq!(envelope.selected_scope, Some(SubjectScopeV1::General));
    assert_eq!(
        envelope.input,
        CaptureInputV1::Youtube {
            video_id: "dQw4w9WgXcQ".to_owned(),
            canonical_url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
        }
    );
}

#[test]
fn rejects_mixed_and_unsupported_url_inputs() {
    assert_eq!(
        normalize_telegram_update_v1(
            &config(),
            &serde_json::to_vec(&document_update("/general https://youtu.be/dQw4w9WgXcQ")).unwrap(),
        )
        .unwrap_err()
        .code(),
        "telegram_input_mixed"
    );

    let unsupported = json!({
        "update_id": 9003,
        "message": {
            "message_id": 2004,
            "date": 1_784_838_402,
            "chat": { "id": -100123 },
            "from": { "id": 12345 },
            "text": "https://example.com/video"
        }
    });
    assert_eq!(
        normalize_telegram_update_v1(&config(), &serde_json::to_vec(&unsupported).unwrap())
            .unwrap_err()
            .code(),
        "telegram_youtube_invalid"
    );
}

#[test]
fn capture_is_deterministic_for_the_same_update() {
    let input = serde_json::to_vec(&document_update("/finance")).unwrap();
    let first = normalize_telegram_update_v1(&config(), &input).unwrap();
    let second = normalize_telegram_update_v1(&config(), &input).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.capture_id, second.capture_id);
}

#[test]
fn canonicalizer_rejects_extra_hosts_paths_ports_and_userinfo() {
    for url in [
        "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtube.com:443/watch?v=dQw4w9WgXcQ",
        "https://user:pass@youtu.be/dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ/other",
    ] {
        assert!(
            canonicalize_youtube_url_v1(url).is_err(),
            "must reject {url}"
        );
    }
}
