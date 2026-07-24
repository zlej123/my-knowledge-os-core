//! Redacted Telegram Bot API contract.
//!
//! Concrete HTTP implementations belong in this crate so the token's contents
//! never have to be exposed to callers.

use std::time::Duration;

use mko_core::error::MkoError;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    pairing::{BotIdentityV1, MAX_TELEGRAM_PAIRING_UPDATE_BYTES_V1},
    secret::{TelegramBotToken, token_text},
};

pub trait TelegramBotApi {
    fn get_me(&self, token: &TelegramBotToken) -> Result<BotIdentityV1, MkoError>;

    fn get_updates(
        &self,
        _token: &TelegramBotToken,
        _offset: Option<u64>,
    ) -> Result<Vec<TelegramRawUpdateV1>, MkoError> {
        Err(MkoError::new(
            "telegram_updates_unsupported",
            "This Telegram Bot API adapter does not support update polling",
        ))
    }

    fn send_message(
        &self,
        _token: &TelegramBotToken,
        _chat_id: &str,
        _text: &str,
    ) -> Result<(), MkoError> {
        Err(MkoError::new(
            "telegram_send_unsupported",
            "This Telegram Bot API adapter does not support message delivery",
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelegramRawUpdateV1 {
    pub update_id: u64,
    pub json: Vec<u8>,
}

#[derive(Clone)]
pub struct HttpTelegramBotApi {
    agent: ureq::Agent,
}

impl Default for HttpTelegramBotApi {
    fn default() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .proxy(None)
            .build()
            .into();
        Self { agent }
    }
}

impl TelegramBotApi for HttpTelegramBotApi {
    fn get_me(&self, token: &TelegramBotToken) -> Result<BotIdentityV1, MkoError> {
        let url = Zeroizing::new(format!(
            "https://api.telegram.org/bot{}/getMe",
            token_text(token)
        ));
        let mut response = self
            .agent
            .get(url.as_str())
            .call()
            .map_err(|_| telegram_api_error())?;
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|_| telegram_api_error())?;
        parse_get_me_response(body.as_bytes())
    }

    fn get_updates(
        &self,
        token: &TelegramBotToken,
        offset: Option<u64>,
    ) -> Result<Vec<TelegramRawUpdateV1>, MkoError> {
        let url = bot_api_url(token, "getUpdates");
        let mut request = self
            .agent
            .get(url.as_str())
            .query("limit", "10")
            .query("timeout", "2")
            .query("allowed_updates", "[\"message\"]");
        let offset_text = offset.map(|value| value.to_string());
        if let Some(value) = offset_text.as_deref() {
            request = request.query("offset", value);
        }
        let mut response = request.call().map_err(|_| telegram_updates_error())?;
        let body = response
            .body_mut()
            .with_config()
            .limit(256 * 1024)
            .read_to_string()
            .map_err(|_| telegram_updates_error())?;
        parse_get_updates_response(body.as_bytes())
    }

    fn send_message(
        &self,
        token: &TelegramBotToken,
        chat_id: &str,
        text: &str,
    ) -> Result<(), MkoError> {
        if !is_positive_decimal(chat_id) || text.is_empty() || text.len() > 4096 {
            return Err(MkoError::new(
                "telegram_send_invalid",
                "Telegram delivery requires a valid chat and bounded message",
            ));
        }
        let url = bot_api_url(token, "sendMessage");
        let mut response = self
            .agent
            .post(url.as_str())
            .send_form([("chat_id", chat_id), ("text", text)])
            .map_err(|_| telegram_send_error())?;
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .map_err(|_| telegram_send_error())?;
        parse_ok_response(body.as_bytes()).map_err(|_| telegram_send_error())
    }
}

#[derive(Debug, Deserialize)]
struct TelegramEnvelope<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct GetMeResult {
    id: u64,
    is_bot: bool,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateEnvelope {
    ok: bool,
    result: Option<Vec<serde_json::Value>>,
}

fn parse_get_me_response(input: &[u8]) -> Result<BotIdentityV1, MkoError> {
    let envelope: TelegramEnvelope<GetMeResult> =
        serde_json::from_slice(input).map_err(|_| telegram_api_error())?;
    let result = envelope
        .result
        .filter(|result| envelope.ok && result.is_bot)
        .ok_or_else(telegram_api_error)?;
    BotIdentityV1::new(result.id, result.username.ok_or_else(telegram_api_error)?)
        .map_err(|_| telegram_api_error())
}

fn parse_get_updates_response(input: &[u8]) -> Result<Vec<TelegramRawUpdateV1>, MkoError> {
    let envelope: UpdateEnvelope =
        serde_json::from_slice(input).map_err(|_| telegram_updates_error())?;
    if !envelope.ok {
        return Err(telegram_updates_error());
    }
    envelope
        .result
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            let update_id = value
                .get("update_id")
                .and_then(serde_json::Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or_else(telegram_updates_error)?;
            let json = serde_json::to_vec(&value).map_err(|_| telegram_updates_error())?;
            if json.len() > MAX_TELEGRAM_PAIRING_UPDATE_BYTES_V1 {
                return Err(telegram_updates_error());
            }
            Ok(TelegramRawUpdateV1 { update_id, json })
        })
        .collect()
}

fn parse_ok_response(input: &[u8]) -> Result<(), MkoError> {
    let envelope: TelegramEnvelope<serde_json::Value> =
        serde_json::from_slice(input).map_err(|_| telegram_send_error())?;
    if envelope.ok && envelope.result.is_some() {
        Ok(())
    } else {
        Err(telegram_send_error())
    }
}

fn bot_api_url(token: &TelegramBotToken, method: &str) -> Zeroizing<String> {
    Zeroizing::new(format!(
        "https://api.telegram.org/bot{}/{method}",
        token_text(token)
    ))
}

fn telegram_api_error() -> MkoError {
    MkoError::new(
        "telegram_get_me_failed",
        "Telegram could not verify the bot token and bot identity",
    )
}

fn telegram_updates_error() -> MkoError {
    MkoError::new(
        "telegram_updates_failed",
        "Telegram updates could not be read for bot pairing",
    )
}

fn telegram_send_error() -> MkoError {
    MkoError::new(
        "telegram_send_failed",
        "Telegram could not deliver the connection test message",
    )
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

#[cfg(test)]
mod tests {
    use super::{HttpTelegramBotApi, parse_get_me_response, parse_get_updates_response};

    #[test]
    fn production_agent_ignores_environment_proxy_configuration() {
        let api = HttpTelegramBotApi::default();
        assert!(api.agent.config().proxy().is_none());
    }

    #[test]
    fn parses_realistic_get_me_response_without_requiring_all_telegram_fields() {
        let identity = parse_get_me_response(
            br#"{
                "ok": true,
                "result": {
                    "id": 123456789,
                    "is_bot": true,
                    "first_name": "MKO",
                    "username": "MkoCaptureBot",
                    "can_join_groups": true,
                    "supports_inline_queries": false
                }
            }"#,
        )
        .expect("getMe should parse");

        assert_eq!(identity.bot_id, 123456789);
        assert_eq!(identity.username, "mkocapturebot");
    }

    #[test]
    fn rejects_non_bot_and_redacts_api_description() {
        let error = parse_get_me_response(
            br#"{
                "ok": false,
                "description": "Not Found: secret token fragment",
                "result": null
            }"#,
        )
        .expect_err("non-ok response must fail");

        assert_eq!(error.code(), "telegram_get_me_failed");
        assert!(!error.message().contains("secret token fragment"));
    }

    #[test]
    fn parses_bounded_update_batch_and_preserves_pairing_json() {
        let updates = parse_get_updates_response(
            br#"{
                "ok": true,
                "result": [
                    {
                        "update_id": 17,
                        "message": {
                            "message_id": 4,
                            "date": 1720000000,
                            "chat": {"id": 42, "type": "private"},
                            "from": {"id": 42, "is_bot": false, "first_name": "Owner"},
                            "text": "/start mko_00112233445566778899aabbccddeeff"
                        }
                    }
                ]
            }"#,
        )
        .expect("updates should parse");

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].update_id, 17);
        assert!(String::from_utf8_lossy(&updates[0].json).contains("\"message_id\":4"));
    }
}
