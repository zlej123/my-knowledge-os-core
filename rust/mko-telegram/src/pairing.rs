//! Pure, bounded Telegram private-chat pairing.
//!
//! This module neither obtains a token nor contacts Telegram. The caller owns
//! secure-store lookup and `getMe`; this state machine only turns an already
//! verified bot identity and caller-provided random bytes into a one-time
//! deep-link pairing challenge.

use chrono::{DateTime, Duration, Utc};
use mko_core::error::MkoError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PAIRING_NONCE_BYTES_V1: usize = 16;
pub const PAIRING_TTL_SECONDS_V1: i64 = 5 * 60;
pub const MAX_TELEGRAM_PAIRING_UPDATE_BYTES_V1: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotIdentityV1 {
    pub bot_id: u64,
    pub username: String,
}

impl BotIdentityV1 {
    pub fn new(bot_id: u64, username: impl Into<String>) -> Result<Self, MkoError> {
        let identity = Self {
            bot_id,
            username: username.into().to_ascii_lowercase(),
        };
        if identity.bot_id == 0 || !is_bot_username(&identity.username) {
            return Err(MkoError::new(
                "telegram_bot_identity_invalid",
                "Telegram bot identity must have a positive ID and lowercase username",
            ));
        }
        Ok(identity)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingSessionStateV1 {
    Open,
    Consumed,
    Cancelled,
}

/// Persistable non-secret pairing state. It intentionally contains only the
/// SHA-256 digest of the start parameter, never the parameter itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingSessionV1 {
    pub profile_id: String,
    pub bot: BotIdentityV1,
    pub expected_owner_username: String,
    pub primary_device_id: String,
    pub start_parameter_digest: String,
    pub expires_at: DateTime<Utc>,
    pub state: PairingSessionStateV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingStartV1 {
    /// This is returned once for display in the deep link/QR. It is not a
    /// credential and must not be persisted with `PairingSessionV1`.
    pub start_parameter: String,
    pub deep_link: String,
    pub session: PairingSessionV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCandidateV1 {
    pub profile_id: String,
    pub bot: BotIdentityV1,
    pub chat_id: String,
    pub sender_id: String,
    pub sender_username: Option<String>,
    pub update_id: String,
    pub message_id: String,
    pub primary_device_id: String,
}

/// Opens a five-minute, one-time pairing session from exactly 16 caller
/// supplied random bytes. Randomness generation is deliberately outside this
/// pure module so tests and host policy control it.
pub fn open_pairing_v1(
    profile_id: impl Into<String>,
    bot: BotIdentityV1,
    expected_owner_username: impl Into<String>,
    primary_device_id: impl Into<String>,
    nonce_bytes: [u8; PAIRING_NONCE_BYTES_V1],
    now: DateTime<Utc>,
) -> Result<PairingStartV1, MkoError> {
    let profile_id = profile_id.into();
    let expected_owner_username = expected_owner_username
        .into()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    let primary_device_id = primary_device_id.into();
    if !is_profile_id(&profile_id)
        || !is_telegram_username(&expected_owner_username)
        || !is_device_id(&primary_device_id)
    {
        return Err(MkoError::new(
            "telegram_pairing_config_invalid",
            "pairing profile, owner username, and primary device IDs must be bounded identifiers",
        ));
    }
    let start_parameter = format!("mko_{}", hex::encode(nonce_bytes));
    if !is_start_parameter(&start_parameter) {
        return Err(MkoError::new(
            "telegram_pairing_nonce_invalid",
            "pairing start parameter violates Telegram v1 bounds",
        ));
    }
    let session = PairingSessionV1 {
        profile_id,
        bot: bot.clone(),
        expected_owner_username,
        primary_device_id,
        start_parameter_digest: digest_start_parameter(&start_parameter),
        expires_at: now + Duration::seconds(PAIRING_TTL_SECONDS_V1),
        state: PairingSessionStateV1::Open,
    };
    Ok(PairingStartV1 {
        deep_link: format!("https://t.me/{}?start={start_parameter}", bot.username),
        start_parameter,
        session,
    })
}

impl PairingSessionV1 {
    pub fn cancel(&mut self) {
        if self.state == PairingSessionStateV1::Open {
            self.state = PairingSessionStateV1::Cancelled;
        }
    }

    /// Accepts exactly one private, owner-originated `/start <parameter>`
    /// message. Any parsing, expiry, nonce, replay, or identity failure leaves
    /// no candidate and fails closed.
    pub fn accept_update_v1(
        &mut self,
        input: &[u8],
        now: DateTime<Utc>,
    ) -> Result<PairingCandidateV1, MkoError> {
        match self.state {
            PairingSessionStateV1::Consumed => {
                return Err(MkoError::new(
                    "telegram_pairing_replayed",
                    "Telegram pairing session has already been consumed",
                ));
            }
            PairingSessionStateV1::Cancelled => {
                return Err(MkoError::new(
                    "telegram_pairing_cancelled",
                    "Telegram pairing session is not open",
                ));
            }
            PairingSessionStateV1::Open => {}
        }
        if now >= self.expires_at {
            self.state = PairingSessionStateV1::Cancelled;
            return Err(MkoError::new(
                "telegram_pairing_expired",
                "Telegram pairing session has expired",
            ));
        }
        if input.len() > MAX_TELEGRAM_PAIRING_UPDATE_BYTES_V1 {
            return Err(MkoError::new(
                "telegram_pairing_update_too_large",
                "Telegram pairing Update exceeds the bounded v1 input size",
            ));
        }
        let update: PairingUpdate = serde_json::from_slice(input)
            .map_err(|error| MkoError::new("telegram_pairing_update_invalid", error.to_string()))?;
        let message = update.message;
        let update_id = positive_id(update.update_id, "update_id")?;
        let message_id = positive_id(message.message_id, "message_id")?;
        let chat_id = positive_id(message.chat.id, "chat_id")?;
        let sender_id = positive_id(message.from.id, "sender_id")?;
        if message.chat.kind != "private"
            || message.from.is_bot
            || chat_id != sender_id
            || message.forward_origin.is_some()
            || message.forward_date.is_some()
            || message.sender_chat.is_some()
        {
            return Err(MkoError::new(
                "telegram_pairing_identity_invalid",
                "pairing requires one private chat owned by a non-bot sender",
            ));
        }
        let parameter = message
            .text
            .strip_prefix("/start ")
            .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
            .ok_or_else(|| {
                MkoError::new(
                    "telegram_pairing_command_invalid",
                    "pairing requires exactly one Telegram start command",
                )
            })?;
        if message.text != format!("/start {parameter}") || !is_start_parameter(parameter) {
            return Err(MkoError::new(
                "telegram_pairing_command_invalid",
                "pairing requires exactly one Telegram start command",
            ));
        }
        if digest_start_parameter(parameter) != self.start_parameter_digest {
            return Err(MkoError::new(
                "telegram_pairing_nonce_mismatch",
                "Telegram pairing start parameter does not match this session",
            ));
        }
        let sender_username = match message
            .from
            .username
            .map(|username| username.to_ascii_lowercase())
            .map(|username| {
                if is_telegram_username(&username) {
                    Ok(username)
                } else {
                    Err(MkoError::new(
                        "telegram_pairing_identity_invalid",
                        "Telegram pairing sender username is invalid",
                    ))
                }
            })
            .transpose()
        {
            Ok(username) => username,
            Err(error) => {
                self.state = PairingSessionStateV1::Cancelled;
                return Err(error);
            }
        };
        if sender_username.as_deref() != Some(self.expected_owner_username.as_str()) {
            self.state = PairingSessionStateV1::Cancelled;
            return Err(MkoError::new(
                "telegram_pairing_owner_mismatch",
                "Telegram pairing was opened by a different owner account",
            ));
        }
        self.state = PairingSessionStateV1::Consumed;
        Ok(PairingCandidateV1 {
            profile_id: self.profile_id.clone(),
            bot: self.bot.clone(),
            chat_id,
            sender_id,
            sender_username,
            update_id,
            message_id,
            primary_device_id: self.primary_device_id.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct PairingUpdate {
    update_id: i64,
    message: PairingMessage,
}

#[derive(Debug, Deserialize)]
struct PairingMessage {
    message_id: i64,
    text: String,
    chat: PairingChat,
    from: PairingUser,
    #[serde(default)]
    forward_origin: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    forward_date: Option<i64>,
    #[serde(default)]
    sender_chat: Option<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct PairingChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct PairingUser {
    id: i64,
    is_bot: bool,
    #[serde(default)]
    username: Option<String>,
}

fn digest_start_parameter(parameter: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(parameter.as_bytes()))
    )
}

fn positive_id(value: i64, name: &str) -> Result<String, MkoError> {
    if value <= 0 {
        return Err(MkoError::new(
            "telegram_pairing_update_invalid",
            format!("Telegram {name} must be positive"),
        ));
    }
    Ok(value.to_string())
}

fn is_bot_username(value: &str) -> bool {
    (5..=32).contains(&value.len())
        && value.ends_with("bot")
        && value.starts_with(|character: char| character.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_telegram_username(value: &str) -> bool {
    (5..=32).contains(&value.len())
        && value.starts_with(|character: char| character.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_profile_id(value: &str) -> bool {
    value.len() <= 64
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn is_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn is_start_parameter(value: &str) -> bool {
    value.len() <= 64
        && value.starts_with("mko_")
        && value["mko_".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value["mko_".len()..].len() == PAIRING_NONCE_BYTES_V1 * 2
}
