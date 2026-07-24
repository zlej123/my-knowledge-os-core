use chrono::{Duration, TimeZone, Utc};
use mko_telegram::pairing::{
    BotIdentityV1, PAIRING_TTL_SECONDS_V1, PairingSessionStateV1, open_pairing_v1,
};
use serde_json::json;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 24, 12, 0, 0).unwrap()
}

fn start() -> mko_telegram::pairing::PairingStartV1 {
    open_pairing_v1(
        "personal_capture",
        BotIdentityV1::new(123_456, "mko_capture_bot").unwrap(),
        "mkoowner",
        "macbook_primary",
        [0xabu8; 16],
        now(),
    )
    .unwrap()
}

fn update(parameter: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "update_id": 9001,
        "message": {
            "message_id": 42,
            "date": 1_784_918_400,
            "text": format!("/start {parameter}"),
            "chat": {
                "id": 123456789,
                "type": "private",
                "first_name": "Owner"
            },
            "from": {
                "id": 123456789,
                "is_bot": false,
                "username": "MkoOwner",
                "first_name": "Owner",
                "language_code": "ko"
            },
            "entities": [{
                "offset": 0,
                "length": 6,
                "type": "bot_command"
            }]
        }
    }))
    .unwrap()
}

#[test]
fn happy_path_returns_private_chat_candidate_and_deep_link() {
    let started = start();
    assert_eq!(started.start_parameter, format!("mko_{}", "ab".repeat(16)));
    assert_eq!(
        started.deep_link,
        format!(
            "https://t.me/mko_capture_bot?start={}",
            started.start_parameter
        )
    );
    assert_eq!(started.session.state, PairingSessionStateV1::Open);
    let mut session = started.session;

    let candidate = session
        .accept_update_v1(
            &update(&started.start_parameter),
            now() + Duration::seconds(1),
        )
        .unwrap();

    assert_eq!(candidate.profile_id, "personal_capture");
    assert_eq!(candidate.bot.username, "mko_capture_bot");
    assert_eq!(candidate.chat_id, "123456789");
    assert_eq!(candidate.sender_id, "123456789");
    assert_eq!(candidate.sender_username.as_deref(), Some("mkoowner"));
    assert_eq!(candidate.update_id, "9001");
    assert_eq!(candidate.message_id, "42");
    assert_eq!(candidate.primary_device_id, "macbook_primary");
    assert_eq!(session.state, PairingSessionStateV1::Consumed);
}

#[test]
fn bot_username_is_normalized_and_must_end_in_bot() {
    assert_eq!(
        BotIdentityV1::new(123_456, "Mko_Capture_Bot")
            .unwrap()
            .username,
        "mko_capture_bot"
    );
    assert!(BotIdentityV1::new(123_456, "mko_capture").is_err());
}

#[test]
fn state_serialization_contains_only_the_nonce_digest() {
    let started = start();
    let serialized = serde_json::to_string(&started.session).unwrap();
    assert!(serialized.contains("start_parameter_digest"));
    assert!(!serialized.contains(&started.start_parameter));
    assert!(!serialized.contains("abababababababababababababababab"));
}

#[test]
fn rejects_nonprivate_sender_mismatch_and_forwarded_fields() {
    let started = start();
    let cases = [
        json!({
            "update_id": 9001,
            "message": {
                "message_id": 42,
                "text": format!("/start {}", started.start_parameter),
                "chat": {"id": 123456789, "type": "group"},
                "from": {"id": 123456789, "is_bot": false}
            }
        }),
        json!({
            "update_id": 9001,
            "message": {
                "message_id": 42,
                "text": format!("/start {}", started.start_parameter),
                "chat": {"id": 123456789, "type": "private"},
                "from": {"id": 987654321, "is_bot": false}
            }
        }),
        json!({
            "update_id": 9001,
            "message": {
                "message_id": 42,
                "text": format!("/start {}", started.start_parameter),
                "chat": {"id": 123456789, "type": "private"},
                "from": {"id": 123456789, "is_bot": false},
                "forward_date": 123
            }
        }),
    ];
    for value in cases {
        let mut session = started.session.clone();
        let error = session
            .accept_update_v1(&serde_json::to_vec(&value).unwrap(), now())
            .unwrap_err();
        assert!(matches!(
            error.code(),
            "telegram_pairing_identity_invalid" | "telegram_pairing_update_invalid"
        ));
        assert_eq!(session.state, PairingSessionStateV1::Open);
    }
}

#[test]
fn expiry_replay_and_nonce_mismatch_fail_closed() {
    let started = start();
    let mut expired = started.session.clone();
    let error = expired
        .accept_update_v1(
            &update(&started.start_parameter),
            now() + Duration::seconds(PAIRING_TTL_SECONDS_V1),
        )
        .unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_expired");
    assert_eq!(expired.state, PairingSessionStateV1::Cancelled);

    let mut wrong_nonce = started.session.clone();
    let error = wrong_nonce
        .accept_update_v1(&update("mko_00000000000000000000000000000000"), now())
        .unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_nonce_mismatch");
    assert_eq!(wrong_nonce.state, PairingSessionStateV1::Open);

    let mut replay = started.session;
    replay
        .accept_update_v1(&update(&started.start_parameter), now())
        .unwrap();
    let error = replay
        .accept_update_v1(&update(&started.start_parameter), now())
        .unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_replayed");
}

#[test]
fn different_owner_username_cancels_the_pairing_nonce() {
    let started = start();
    let mut value: serde_json::Value =
        serde_json::from_slice(&update(&started.start_parameter)).unwrap();
    value["message"]["from"]["username"] = json!("other_owner");
    let mut session = started.session;
    let error = session
        .accept_update_v1(&serde_json::to_vec(&value).unwrap(), now())
        .unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_owner_mismatch");
    assert_eq!(session.state, PairingSessionStateV1::Cancelled);
}

#[test]
fn malformed_and_oversize_updates_are_rejected() {
    let started = start();
    let mut session = started.session.clone();
    let error = session.accept_update_v1(b"{", now()).unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_update_invalid");

    let mut session = started.session;
    let oversized = vec![b'x'; 64 * 1024 + 1];
    let error = session.accept_update_v1(&oversized, now()).unwrap_err();
    assert_eq!(error.code(), "telegram_pairing_update_too_large");
}
