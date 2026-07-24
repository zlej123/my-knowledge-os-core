//! Validated owner-local Telegram connection bindings.
//!
//! Persistence belongs exclusively to the atomic connection store so a token
//! and binding can never be committed independently.

use chrono::{DateTime, Utc};
use mko_core::error::MkoError;
use serde::Serialize;

use crate::pairing::BotIdentityV1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramBindingV1 {
    pub profile_id: String,
    pub bot: BotIdentityV1,
    pub chat_id: String,
    pub sender_id: String,
    pub primary_device_id: String,
    pub connected_at: DateTime<Utc>,
}

impl TelegramBindingV1 {
    pub fn new(
        profile_id: impl Into<String>,
        bot: BotIdentityV1,
        chat_id: impl Into<String>,
        sender_id: impl Into<String>,
        primary_device_id: impl Into<String>,
        connected_at: DateTime<Utc>,
    ) -> Result<Self, MkoError> {
        let binding = Self {
            profile_id: profile_id.into(),
            bot,
            chat_id: chat_id.into(),
            sender_id: sender_id.into(),
            primary_device_id: primary_device_id.into(),
            connected_at,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), MkoError> {
        let normalized_bot = BotIdentityV1::new(self.bot.bot_id, self.bot.username.clone()).ok();
        if !is_profile_id(&self.profile_id)
            || !is_positive_decimal_id(&self.chat_id)
            || !is_positive_decimal_id(&self.sender_id)
            || self.chat_id != self.sender_id
            || !is_device_id(&self.primary_device_id)
            || normalized_bot.as_ref() != Some(&self.bot)
        {
            return Err(MkoError::new(
                "telegram_binding_invalid",
                "Telegram binding violates the bounded private-chat contract",
            ));
        }
        Ok(())
    }
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

fn is_positive_decimal_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::{BotIdentityV1, TelegramBindingV1};

    #[test]
    fn binding_requires_one_valid_private_chat_owner() {
        assert!(
            TelegramBindingV1::new(
                "personal",
                BotIdentityV1::new(1, "mko_capture_bot").unwrap(),
                "2",
                "2",
                "primary",
                chrono::Utc::now(),
            )
            .is_ok()
        );
        assert!(
            TelegramBindingV1::new(
                "personal",
                BotIdentityV1::new(1, "mko_capture_bot").unwrap(),
                "2",
                "3",
                "primary",
                chrono::Utc::now(),
            )
            .is_err()
        );
    }
}
