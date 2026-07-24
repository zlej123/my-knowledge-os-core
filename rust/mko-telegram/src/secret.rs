//! Opaque in-memory Telegram bot token.
//!
//! Durable storage belongs exclusively to the atomic connection store.

use mko_core::error::MkoError;
use zeroize::Zeroizing;

const MIN_BOT_TOKEN_BYTES_V1: usize = 16;
const MAX_BOT_TOKEN_BYTES_V1: usize = 256;

/// An in-memory bot token whose owned buffer is zeroized on drop.
///
/// It intentionally implements neither `Debug`, `Display`, `Clone`, nor
/// `Serialize`. Only this crate's transport implementations can borrow the
/// secret contents.
pub struct TelegramBotToken(Zeroizing<String>);

impl TelegramBotToken {
    pub fn new(value: String) -> Result<Self, MkoError> {
        if !(MIN_BOT_TOKEN_BYTES_V1..=MAX_BOT_TOKEN_BYTES_V1).contains(&value.len())
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(MkoError::new(
                "telegram_token_invalid",
                "Telegram bot token violates the bounded secret contract",
            ));
        }
        Ok(Self(Zeroizing::new(value)))
    }

    pub(crate) fn expose_secret(&self) -> &str {
        &self.0
    }
}

pub(crate) fn token_text(token: &TelegramBotToken) -> &str {
    token.expose_secret()
}
