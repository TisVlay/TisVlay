//! Integration tests for discovery + pairing user stories against the mock backend.
//!
//! Covers the end-to-end flows a real user goes through when pairing their
//! first TV, driven entirely from test code via [`MockHandle`]:
//!
//! - Empty network — discovery returns no TVs
//! - Happy path — discover → pair → receive credentials
//! - Reject — user declines pairing prompt
//! - Timeout — user never responds
//! - Multiple TVs — discover several, pair one
//! - Dynamic publish — TV appears mid-session
//! - Mixed-mode multi-TV — one accepts, one rejects, in the same test
//! - Config shorthand vs handle precedence
//!
//! [`MockHandle`]: tisvlay_mock::MockHandle

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_backends::create_test_registry;
use tisvlay_core::{Credentials, DiscoveredTv, PairingError, PairingErrorCode};

// ---------- Fixtures ----------

fn discovered(id: &str, name: &str) -> DiscoveredTv {
    DiscoveredTv::new(id, name, format!("mock://{id}"), "mock")
}

/// Minimal vendor config matching a given address. Used to start a
/// pairing session once a TV has been discovered.
fn vendor_config(address: &str) -> toml::Value {
    toml::Value::try_from(toml::toml! {
        address = address
    })
    .unwrap()
}

/// Vendor config that uses the `pairing` shorthand — no handle call needed.
fn vendor_config_with_shorthand(address: &str, shorthand: &str) -> toml::Value {
    let toml_str = format!(
        r#"
address = "{address}"
pairing = "{shorthand}"
"#
    );
    toml::from_str(&toml_str).unwrap()
}

/// Drive a pairing session to completion, bounded by a test-safe timeout.
///
/// Returns `Ok(Credentials)` on success or `Err(PairingError)` on failure,
/// mirroring `PairingSession::wait_for_completion` but with a hard timeout
/// so a buggy state machine can't hang the test suite.
async fn drain_pairing(
    session: &mut tisvlay_core::PairingSession,
) -> Result<Credentials, PairingError> {
    tokio::time::timeout(Duration::from_secs(1), session.wait_for_completion())
        .await
        .expect("pairing did not complete within 1s")
}

// ---------- Discovery ----------

#[tokio::test]
async fn discover_empty_network_returns_empty_list() {
    let (registry, _mock) = create_test_registry();
    let tvs = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert!(tvs.is_empty());
}

#[tokio::test]
async fn discover_returns_published_tvs_in_order() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.publish_tv(discovered("office", "Office C3"));
    mock.publish_tv(discovered("guest_room", "Guest Room"));

    let tvs = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(tvs.len(), 3);
    assert_eq!(tvs[0].id, "living_room");
    assert_eq!(tvs[1].id, "office");
    assert_eq!(tvs[2].id, "guest_room");
}

#[tokio::test]
async fn discover_picks_up_newly_published_tvs() {
    let (registry, mock) = create_test_registry();

    let first = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert!(first.is_empty());

    mock.publish_tv(discovered("living_room", "Living Room G4"));

    let second = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, "living_room");
}

#[tokio::test]
async fn clear_discovered_removes_all_tvs() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.publish_tv(discovered("office", "Office C3"));
    assert_eq!(mock.discovered_count(), 2);

    mock.clear_discovered();
    assert_eq!(mock.discovered_count(), 0);

    let tvs = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert!(tvs.is_empty());
}

// ---------- Pairing (via handle) ----------

#[tokio::test]
async fn pair_happy_path_via_handle() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.simulate_user_accept("mock://living_room");

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config("mock://living_room"))
        .unwrap();

    let credentials = drain_pairing(&mut session)
        .await
        .expect("pairing should have completed");
    assert_eq!(credentials.data, "mock-key");
}

#[tokio::test]
async fn pair_user_rejection_surfaces_as_failed() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.simulate_user_reject("mock://living_room");

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config("mock://living_room"))
        .unwrap();

    let error = drain_pairing(&mut session)
        .await
        .expect_err("pairing should have failed");
    assert_eq!(error.code, PairingErrorCode::Rejected);
}

#[tokio::test]
async fn pair_timeout_surfaces_as_failed() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.simulate_pairing_timeout("mock://living_room");

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config("mock://living_room"))
        .unwrap();

    let error = drain_pairing(&mut session)
        .await
        .expect_err("pairing should have failed");
    assert_eq!(error.code, PairingErrorCode::Timeout);
}

// ---------- Pairing (via config shorthand) ----------

#[tokio::test]
async fn pair_shorthand_accept_auto_completes() {
    let (registry, _mock) = create_test_registry();

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config_with_shorthand(
            "mock://living_room",
            "accept",
        ))
        .unwrap();

    let credentials = drain_pairing(&mut session)
        .await
        .expect("shorthand accept should have completed");
    assert_eq!(credentials.data, "mock-key");
}

#[tokio::test]
async fn pair_shorthand_reject_auto_fails() {
    let (registry, _mock) = create_test_registry();

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config_with_shorthand(
            "mock://living_room",
            "reject",
        ))
        .unwrap();

    drain_pairing(&mut session)
        .await
        .expect_err("shorthand reject should have failed");
}

#[tokio::test]
async fn pair_handle_overrides_shorthand() {
    // If the config says "accept" but the handle says "reject" *before*
    // start_pairing() consumes the outcome, the handle wins — the
    // documented precedence rule.
    let (registry, mock) = create_test_registry();
    mock.simulate_user_reject("mock://living_room");

    let connector = registry.get_connector("mock").unwrap();
    let mut session = connector
        .start_pairing(&vendor_config_with_shorthand(
            "mock://living_room",
            "accept",
        ))
        .unwrap();

    drain_pairing(&mut session)
        .await
        .expect_err("handle's reject should have won over shorthand's accept");
}

// ---------- End-to-end user story: mixed-mode multi-TV ----------

#[tokio::test]
async fn discover_then_pair_multiple_tvs_with_mixed_outcomes() {
    let (registry, mock) = create_test_registry();

    mock.publish_tv(discovered("living_room", "Living Room G4"));
    mock.publish_tv(discovered("office", "Office C3"));

    // Seed different outcomes per TV before the test drives them.
    mock.simulate_user_accept("mock://living_room");
    mock.simulate_user_reject("mock://office");

    let connector = registry.get_connector("mock").unwrap();

    let tvs = registry
        .discover_tvs("mock", Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(tvs.len(), 2);

    let mut results = Vec::new();
    for tv in tvs {
        let mut session = connector
            .start_pairing(&vendor_config(&tv.address))
            .unwrap();
        let outcome = drain_pairing(&mut session).await;
        results.push((tv.id, outcome));
    }

    // Living room accepted; office rejected.
    assert_eq!(results[0].0, "living_room");
    assert!(
        results[0].1.is_ok(),
        "living_room should have paired: {:?}",
        results[0].1
    );

    assert_eq!(results[1].0, "office");
    assert!(
        results[1].1.is_err(),
        "office should have been rejected: {:?}",
        results[1].1
    );
}
