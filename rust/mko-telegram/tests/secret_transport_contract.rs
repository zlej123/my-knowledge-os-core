use mko_core::error::MkoError;
use mko_telegram::{pairing::BotIdentityV1, secret::TelegramBotToken, transport::TelegramBotApi};

struct FailingApi;

impl TelegramBotApi for FailingApi {
    fn get_me(&self, _token: &TelegramBotToken) -> Result<BotIdentityV1, MkoError> {
        Err(MkoError::new(
            "telegram_get_me_failed",
            "Telegram bot identity verification failed",
        ))
    }
}

#[test]
fn token_is_bounded_and_errors_never_echo_it() {
    let secret = "123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef";
    let token = TelegramBotToken::new(secret.to_owned()).unwrap();
    let error = FailingApi.get_me(&token).unwrap_err();
    assert_eq!(error.code(), "telegram_get_me_failed");
    assert!(!error.to_string().contains(secret));

    assert!(TelegramBotToken::new("short".to_owned()).is_err());
    assert!(TelegramBotToken::new(format!("{}\n", "x".repeat(20))).is_err());
    assert!(TelegramBotToken::new("x".repeat(257)).is_err());
}
