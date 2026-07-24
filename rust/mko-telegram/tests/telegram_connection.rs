use std::{cell::RefCell, collections::BTreeMap};

use chrono::{TimeZone, Utc};
use mko_core::error::MkoError;
use mko_telegram::{
    binding::TelegramBindingV1,
    connection::{TelegramConnectionRef, TelegramConnectionStore, TelegramConnectionV1},
    pairing::BotIdentityV1,
    secret::TelegramBotToken,
};

#[derive(Default)]
struct MemoryConnectionStore(RefCell<BTreeMap<(String, String), TelegramBindingV1>>);

impl TelegramConnectionStore for MemoryConnectionStore {
    fn contains(&self, reference: &TelegramConnectionRef) -> Result<bool, MkoError> {
        Ok(self
            .0
            .borrow()
            .contains_key(&(reference.service.clone(), reference.account.clone())))
    }

    fn get(&self, reference: &TelegramConnectionRef) -> Result<TelegramConnectionV1, MkoError> {
        self.0
            .borrow()
            .get(&(reference.service.clone(), reference.account.clone()))
            .cloned()
            .ok_or_else(|| {
                MkoError::new(
                    "telegram_connection_missing",
                    "Telegram is not connected for this profile",
                )
            })
            .and_then(|binding| {
                TelegramConnectionV1::new(
                    TelegramBotToken::new("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef".to_owned())?,
                    binding,
                )
            })
    }

    fn put(
        &self,
        reference: &TelegramConnectionRef,
        connection: TelegramConnectionV1,
    ) -> Result<(), MkoError> {
        self.0.borrow_mut().insert(
            (reference.service.clone(), reference.account.clone()),
            connection.binding().clone(),
        );
        Ok(())
    }

    fn delete(&self, reference: &TelegramConnectionRef) -> Result<(), MkoError> {
        self.0
            .borrow_mut()
            .remove(&(reference.service.clone(), reference.account.clone()));
        Ok(())
    }
}

fn connection() -> TelegramConnectionV1 {
    TelegramConnectionV1::new(
        TelegramBotToken::new("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef".to_owned()).unwrap(),
        TelegramBindingV1::new(
            "personal_capture",
            BotIdentityV1::new(123_456, "mko_capture_bot").unwrap(),
            "123456789",
            "123456789",
            "macbook_primary",
            Utc.with_ymd_and_hms(2026, 7, 24, 12, 0, 0).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn connection_ref_is_versioned_and_connection_store_is_atomic() {
    let store = MemoryConnectionStore::default();
    let reference = TelegramConnectionRef::for_profile("personal_capture").unwrap();
    assert_eq!(reference.service, "my-knowledge-os.telegram");
    assert_eq!(reference.account, "personal_capture.connection-v1");
    assert!(!store.contains(&reference).unwrap());

    store.put(&reference, connection()).unwrap();
    assert!(store.contains(&reference).unwrap());
    let fetched = store.get(&reference).unwrap();
    assert_eq!(fetched.binding().profile_id, "personal_capture");
    assert_eq!(fetched.binding().chat_id, "123456789");
    assert!(store.contains(&reference).unwrap());

    store.delete(&reference).unwrap();
    store.delete(&reference).unwrap();
    let error = match store.get(&reference) {
        Ok(_) => panic!("deleted connection must be absent"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "telegram_connection_missing");
}

#[test]
fn connection_ref_rejects_invalid_profile() {
    assert!(TelegramConnectionRef::for_profile("../personal").is_err());
    assert!(TelegramConnectionRef::for_profile(&"a".repeat(65)).is_err());
}
