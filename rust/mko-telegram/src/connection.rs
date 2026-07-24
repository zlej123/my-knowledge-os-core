//! Atomic OS-keyring Telegram connection storage.
//!
//! A connection stores its token and private-chat binding in one credential so
//! connect/disconnect cannot leave a usable half-state. There is no file or
//! environment-variable fallback.

use chrono::{DateTime, Utc};
use mko_core::error::MkoError;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    binding::TelegramBindingV1,
    pairing::BotIdentityV1,
    secret::{TelegramBotToken, token_text},
};

const CONNECTION_SERVICE_V1: &str = "my-knowledge-os.telegram";
const MAX_CONNECTION_BYTES_V1: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConnectionRef {
    pub service: String,
    pub account: String,
}

impl TelegramConnectionRef {
    pub fn for_profile(profile_id: &str) -> Result<Self, MkoError> {
        if !is_profile_id(profile_id) {
            return Err(MkoError::new(
                "telegram_connection_ref_invalid",
                "Telegram connection profile must be a bounded identifier",
            ));
        }
        Ok(Self {
            service: CONNECTION_SERVICE_V1.to_owned(),
            account: format!("{profile_id}.connection-v1"),
        })
    }
}

/// An in-memory, non-serializable connected Telegram credential and binding.
///
/// It intentionally implements neither `Debug`, `Clone`, nor `Serialize` so
/// callers cannot accidentally expose a bot token through ordinary logging or
/// JSON output.
pub struct TelegramConnectionV1 {
    token: TelegramBotToken,
    binding: TelegramBindingV1,
}

impl TelegramConnectionV1 {
    pub fn new(token: TelegramBotToken, binding: TelegramBindingV1) -> Result<Self, MkoError> {
        binding.validate()?;
        Ok(Self { token, binding })
    }

    pub fn token(&self) -> &TelegramBotToken {
        &self.token
    }

    pub fn binding(&self) -> &TelegramBindingV1 {
        &self.binding
    }
}

pub trait TelegramConnectionStore {
    fn contains(&self, reference: &TelegramConnectionRef) -> Result<bool, MkoError>;
    fn get(&self, reference: &TelegramConnectionRef) -> Result<TelegramConnectionV1, MkoError>;
    fn put(
        &self,
        reference: &TelegramConnectionRef,
        connection: TelegramConnectionV1,
    ) -> Result<(), MkoError>;
    fn delete(&self, reference: &TelegramConnectionRef) -> Result<(), MkoError>;
}

/// Production storage backed only by macOS Keychain or Windows Credential
/// Manager. Unsupported platforms fail closed before any plaintext fallback.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsTelegramConnectionStore;

impl TelegramConnectionStore for OsTelegramConnectionStore {
    fn contains(&self, reference: &TelegramConnectionRef) -> Result<bool, MkoError> {
        #[cfg(any(target_os = "macos", windows))]
        {
            match credential_entry(reference)?.get_password() {
                Ok(stored) => {
                    let _stored = Zeroizing::new(stored);
                    Ok(true)
                }
                Err(keyring::v1::Error::NoEntry) => Ok(false),
                Err(_) => Err(connection_store_error()),
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = reference;
            Err(connection_store_unsupported())
        }
    }

    fn get(&self, reference: &TelegramConnectionRef) -> Result<TelegramConnectionV1, MkoError> {
        #[cfg(any(target_os = "macos", windows))]
        {
            let stored = Zeroizing::new(credential_entry(reference)?.get_password().map_err(
                |error| match error {
                    keyring::v1::Error::NoEntry => MkoError::new(
                        "telegram_connection_missing",
                        "Telegram is not connected for this profile",
                    ),
                    _ => connection_store_error(),
                },
            )?);
            parse_connection(stored.as_str())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = reference;
            Err(connection_store_unsupported())
        }
    }

    fn put(
        &self,
        reference: &TelegramConnectionRef,
        connection: TelegramConnectionV1,
    ) -> Result<(), MkoError> {
        let serialized = serialize_connection(&connection)?;
        #[cfg(any(target_os = "macos", windows))]
        {
            credential_entry(reference)?
                .set_password(serialized.as_str())
                .map_err(|_| connection_store_error())
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = (reference, serialized);
            Err(connection_store_unsupported())
        }
    }

    fn delete(&self, reference: &TelegramConnectionRef) -> Result<(), MkoError> {
        #[cfg(any(target_os = "macos", windows))]
        {
            match credential_entry(reference)?.delete_credential() {
                Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
                Err(_) => Err(connection_store_error()),
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = reference;
            Err(connection_store_unsupported())
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StoredConnectionV1<'a> {
    token: &'a str,
    binding: &'a TelegramBindingV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadConnectionV1 {
    token: String,
    binding: ReadBindingV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadBindingV1 {
    profile_id: String,
    bot: ReadBotIdentityV1,
    chat_id: String,
    sender_id: String,
    primary_device_id: String,
    connected_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadBotIdentityV1 {
    bot_id: u64,
    username: String,
}

fn serialize_connection(connection: &TelegramConnectionV1) -> Result<Zeroizing<String>, MkoError> {
    connection.binding.validate()?;
    let serialized = serde_json::to_string(&StoredConnectionV1 {
        token: token_text(&connection.token),
        binding: &connection.binding,
    })
    .map_err(|_| connection_store_error())?;
    if serialized.len() > MAX_CONNECTION_BYTES_V1 {
        return Err(MkoError::new(
            "telegram_connection_invalid",
            "Telegram connection exceeds the bounded storage contract",
        ));
    }
    Ok(Zeroizing::new(serialized))
}

fn parse_connection(input: &str) -> Result<TelegramConnectionV1, MkoError> {
    if input.len() > MAX_CONNECTION_BYTES_V1 {
        return Err(connection_invalid_error());
    }
    let stored: ReadConnectionV1 =
        serde_json::from_str(input).map_err(|_| connection_invalid_error())?;
    let token = TelegramBotToken::new(stored.token).map_err(|_| connection_invalid_error())?;
    let binding = TelegramBindingV1::new(
        stored.binding.profile_id,
        BotIdentityV1::new(stored.binding.bot.bot_id, stored.binding.bot.username)
            .map_err(|_| connection_invalid_error())?,
        stored.binding.chat_id,
        stored.binding.sender_id,
        stored.binding.primary_device_id,
        stored.binding.connected_at,
    )
    .map_err(|_| connection_invalid_error())?;
    TelegramConnectionV1::new(token, binding).map_err(|_| connection_invalid_error())
}

#[cfg(any(target_os = "macos", windows))]
fn credential_entry(reference: &TelegramConnectionRef) -> Result<keyring::v1::Entry, MkoError> {
    keyring::v1::Entry::new(&reference.service, &reference.account)
        .map_err(|_| connection_store_error())
}

fn connection_invalid_error() -> MkoError {
    MkoError::new(
        "telegram_connection_invalid",
        "Telegram connection data is invalid",
    )
}

fn connection_store_error() -> MkoError {
    MkoError::new(
        "telegram_connection_store_failed",
        "The operating system credential store could not complete the Telegram connection operation",
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
fn connection_store_unsupported() -> MkoError {
    MkoError::new(
        "telegram_connection_store_unsupported",
        "Telegram connection storage requires macOS Keychain or Windows Credential Manager",
    )
}

fn is_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::{binding::TelegramBindingV1, pairing::BotIdentityV1, secret::TelegramBotToken};

    use super::{TelegramConnectionV1, parse_connection, serialize_connection};

    fn connection() -> TelegramConnectionV1 {
        TelegramConnectionV1::new(
            TelegramBotToken::new("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef".to_owned()).unwrap(),
            TelegramBindingV1::new(
                "personal",
                BotIdentityV1::new(1, "mko_capture_bot").unwrap(),
                "2",
                "2",
                "primary",
                chrono::Utc.with_ymd_and_hms(2026, 7, 24, 0, 0, 0).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn parser_rejects_token_without_echoing_it() {
        let token = "123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef";
        let error = match parse_connection(&format!("{{\"token\":\"{token}\",\"binding\":{{}}}}")) {
            Ok(_) => panic!("malformed connection must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "telegram_connection_invalid");
        assert!(!error.to_string().contains(token));
    }

    #[test]
    fn stored_connection_round_trips_without_exposing_a_public_serializer() {
        let serialized = serialize_connection(&connection()).unwrap();
        let parsed = parse_connection(serialized.as_str()).unwrap();
        assert_eq!(parsed.binding().profile_id, "personal");
        assert_eq!(parsed.binding().chat_id, "2");
    }
}
