//! `Manager1.DeleteSystemProfile` is rejected with `AccessDenied`
//! when a TV's `Tv1.SystemProfile` references the profile. The error
//! message names the offending TV UUID so the GUI delete-confirm
//! dialog can carry the rejection reason.

#![cfg(feature = "mock")]

use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};

#[tokio::test]
async fn referenced_profile_cannot_be_deleted() {
    let daemon = TestDaemon::with_one_mock_tv("ref_blockdel_tv")
        .await
        .expect("test daemon");
    let tv_uuid = test_tv_uuid("ref_blockdel_tv").to_string();

    let manager = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path("/org/tisvlay/Manager")
        .unwrap()
        .interface("org.tisvlay.Manager1")
        .unwrap()
        .build()
        .await
        .unwrap();

    let new_path: zbus::zvariant::OwnedObjectPath = manager
        .call("CreateSystemProfile", &("guest",))
        .await
        .expect("create");
    let path_str = new_path.as_str().to_string();
    let profile_uuid = path_str
        .rsplit('/')
        .next()
        .unwrap()
        .replace('_', "-")
        .to_string();

    // Assign the TV to the new profile via Tv1.SystemProfile.
    let tv_props = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path(format!("/org/tisvlay/Tv/{}", tv_uuid.replace('-', "_")))
        .unwrap()
        .build()
        .await
        .unwrap();
    tv_props
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from(profile_uuid.as_str()),
        )
        .await
        .expect("set Tv1.SystemProfile");

    // Delete should fail with AccessDenied; the body names the TV UUID.
    let err: zbus::Error = manager
        .call::<_, _, ()>("DeleteSystemProfile", &(profile_uuid.as_str(),))
        .await
        .expect_err("referenced profile must not delete");

    let (name, body) = match &err {
        zbus::Error::MethodError(name, body, _) => (name.as_str().to_string(), body.clone()),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.AccessDenied",
        "referenced delete should map to AccessDenied; got {err:?}",
    );
    let body = body.unwrap_or_default();
    assert!(
        body.contains(&tv_uuid),
        "error body should name TV UUID {tv_uuid}; got {body:?}",
    );
}
