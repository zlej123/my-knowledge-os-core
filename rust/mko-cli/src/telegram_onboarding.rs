use mko_core::error::MkoError;
use mko_telegram::{
    binding::TelegramBindingV1,
    connection::{TelegramConnectionRef, TelegramConnectionStore, TelegramConnectionV1},
    secret::TelegramBotToken,
    transport::TelegramBotApi,
};

const CONNECTION_TEST_MESSAGE_V1: &str = "My Knowledge OS 채널 소유 확인 테스트입니다. 설정 저장이 끝나기 전까지 연결은 완료되지 않습니다. 이 채팅에는 지식 승인이나 투자 판단 권한이 없습니다.";

/// Deliver the ownership test before committing one atomic connection
/// credential. A delivery failure therefore cannot leave durable state.
pub(crate) fn persist_verified_connection<A, S>(
    api: &A,
    store: &S,
    reference: &TelegramConnectionRef,
    token: TelegramBotToken,
    binding: TelegramBindingV1,
) -> Result<(), MkoError>
where
    A: TelegramBotApi,
    S: TelegramConnectionStore,
{
    api.send_message(&token, &binding.chat_id, CONNECTION_TEST_MESSAGE_V1)?;
    let connection = TelegramConnectionV1::new(token, binding)?;
    store.put(reference, connection)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use chrono::{TimeZone, Utc};
    use mko_telegram::{
        connection::TelegramConnectionV1, pairing::BotIdentityV1, transport::TelegramRawUpdateV1,
    };

    use super::*;

    struct FakeApi {
        fail_send: bool,
        send_count: Cell<usize>,
    }

    impl TelegramBotApi for FakeApi {
        fn get_me(&self, _token: &TelegramBotToken) -> Result<BotIdentityV1, MkoError> {
            unreachable!("the persistence seam does not perform bot discovery")
        }

        fn get_updates(
            &self,
            _token: &TelegramBotToken,
            _offset: Option<u64>,
        ) -> Result<Vec<TelegramRawUpdateV1>, MkoError> {
            unreachable!("the persistence seam does not poll updates")
        }

        fn send_message(
            &self,
            _token: &TelegramBotToken,
            _chat_id: &str,
            _text: &str,
        ) -> Result<(), MkoError> {
            self.send_count.set(self.send_count.get() + 1);
            if self.fail_send {
                Err(MkoError::new(
                    "telegram_send_failed",
                    "Telegram test delivery failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakeStore {
        fail_put: bool,
        binding: RefCell<Option<TelegramBindingV1>>,
    }

    impl TelegramConnectionStore for FakeStore {
        fn contains(&self, _reference: &TelegramConnectionRef) -> Result<bool, MkoError> {
            Ok(self.binding.borrow().is_some())
        }

        fn get(
            &self,
            _reference: &TelegramConnectionRef,
        ) -> Result<TelegramConnectionV1, MkoError> {
            Err(MkoError::new(
                "telegram_connection_missing",
                "not needed by this test seam",
            ))
        }

        fn put(
            &self,
            _reference: &TelegramConnectionRef,
            connection: TelegramConnectionV1,
        ) -> Result<(), MkoError> {
            if self.fail_put {
                return Err(MkoError::new(
                    "telegram_connection_store_failed",
                    "test store rejected the connection",
                ));
            }
            self.binding.replace(Some(connection.binding().clone()));
            Ok(())
        }

        fn delete(&self, _reference: &TelegramConnectionRef) -> Result<(), MkoError> {
            self.binding.replace(None);
            Ok(())
        }
    }

    fn token() -> TelegramBotToken {
        TelegramBotToken::new("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef".to_owned()).unwrap()
    }

    fn binding() -> TelegramBindingV1 {
        TelegramBindingV1::new(
            "personal",
            BotIdentityV1::new(123, "mko_capture_bot").unwrap(),
            "456",
            "456",
            "macos-primary",
            Utc.with_ymd_and_hms(2026, 7, 24, 12, 0, 0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn delivery_failure_never_writes_a_connection() {
        let api = FakeApi {
            fail_send: true,
            send_count: Cell::new(0),
        };
        let store = FakeStore::default();
        let reference = TelegramConnectionRef::for_profile("personal").unwrap();

        let error =
            persist_verified_connection(&api, &store, &reference, token(), binding()).unwrap_err();

        assert_eq!(error.code(), "telegram_send_failed");
        assert_eq!(api.send_count.get(), 1);
        assert!(!store.contains(&reference).unwrap());
    }

    #[test]
    fn store_failure_occurs_only_after_delivery_and_leaves_no_half_state() {
        let api = FakeApi {
            fail_send: false,
            send_count: Cell::new(0),
        };
        let store = FakeStore {
            fail_put: true,
            ..FakeStore::default()
        };
        let reference = TelegramConnectionRef::for_profile("personal").unwrap();

        let error =
            persist_verified_connection(&api, &store, &reference, token(), binding()).unwrap_err();

        assert_eq!(error.code(), "telegram_connection_store_failed");
        assert_eq!(api.send_count.get(), 1);
        assert!(!store.contains(&reference).unwrap());
    }

    #[test]
    fn successful_delivery_commits_one_validated_binding() {
        let api = FakeApi {
            fail_send: false,
            send_count: Cell::new(0),
        };
        let store = FakeStore::default();
        let reference = TelegramConnectionRef::for_profile("personal").unwrap();

        persist_verified_connection(&api, &store, &reference, token(), binding()).unwrap();

        assert_eq!(api.send_count.get(), 1);
        assert!(store.contains(&reference).unwrap());
        assert_eq!(
            store.binding.borrow().as_ref().unwrap().profile_id,
            "personal"
        );
    }
}
