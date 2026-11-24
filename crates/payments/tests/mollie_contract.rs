//! Nightly contract test against Mollie test mode (`make mollie-contract`).
//! Without `MOLLIE_TEST_API_KEY_FILE` it does nothing, so normal test runs stay offline.

#![forbid(unsafe_code)]

use payments::{ApiKey, MollieClient, MollieOptions};

#[path = "support/contract.rs"]
mod contract;

#[tokio::test]
async fn mollie_test_mode() {
    let Some(path) = std::env::var_os("MOLLIE_TEST_API_KEY_FILE") else {
        eprintln!("mollie contract test skipped: MOLLIE_TEST_API_KEY_FILE is not set");
        return;
    };
    let key = ApiKey::from_file(path).expect("readable Mollie test API key");
    assert!(
        key.is_test(),
        "the contract test only runs with a test_ key"
    );
    let client = MollieClient::new(key, None, MollieOptions::default()).expect("Mollie client");
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    contract::run(&client, &run_id).await;
}
