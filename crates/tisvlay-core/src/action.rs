//! Shared `SessionAction` dispatch.
//!
//! Both the daemon (`TvManager`, direct backend calls) and the user
//! session service (via `DaemonClient`, D-Bus RPC) translate a resolved
//! [`SessionAction`] into one of four primitive operations. This trait
//! holds that mapping in a single place so the two dispatch sites can't
//! drift on which action calls which primitive.

use async_trait::async_trait;

use crate::config::SessionAction;

/// A target that can perform the primitive TV power/screen operations a
/// [`SessionAction`] resolves to.
///
/// Implementors provide the four primitives; [`apply_action`] is the
/// single, shared action→primitive mapping.
///
/// [`apply_action`]: ActionExecutor::apply_action
#[async_trait]
pub trait ActionExecutor {
    /// Error type of the underlying primitives (the daemon's
    /// `TvManagerError`, the client's [`crate::Error`], etc.).
    type Error;

    async fn screen_on(&self, tv_id: &str) -> Result<(), Self::Error>;
    async fn screen_off(&self, tv_id: &str) -> Result<(), Self::Error>;
    async fn power_on(&self, tv_id: &str) -> Result<(), Self::Error>;
    async fn power_off(&self, tv_id: &str) -> Result<(), Self::Error>;

    /// Dispatch a resolved [`SessionAction`] to its primitive.
    ///
    /// [`SessionAction::None`] is a no-op. This is the one place the
    /// action→primitive mapping lives; adding a `SessionAction` variant
    /// forces an update here (and nowhere else).
    async fn apply_action(&self, action: SessionAction, tv_id: &str) -> Result<(), Self::Error> {
        match action {
            SessionAction::ScreenOff => self.screen_off(tv_id).await,
            SessionAction::ScreenOn => self.screen_on(tv_id).await,
            SessionAction::PowerOff => self.power_off(tv_id).await,
            SessionAction::PowerOn => self.power_on(tv_id).await,
            SessionAction::None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ActionExecutor for Recorder {
        type Error = ();
        async fn screen_on(&self, tv_id: &str) -> Result<(), ()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("screen_on:{tv_id}"));
            Ok(())
        }
        async fn screen_off(&self, tv_id: &str) -> Result<(), ()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("screen_off:{tv_id}"));
            Ok(())
        }
        async fn power_on(&self, tv_id: &str) -> Result<(), ()> {
            self.calls.lock().unwrap().push(format!("power_on:{tv_id}"));
            Ok(())
        }
        async fn power_off(&self, tv_id: &str) -> Result<(), ()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("power_off:{tv_id}"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn apply_action_maps_each_variant_to_its_primitive() {
        let r = Recorder::default();
        r.apply_action(SessionAction::ScreenOff, "tv")
            .await
            .unwrap();
        r.apply_action(SessionAction::ScreenOn, "tv").await.unwrap();
        r.apply_action(SessionAction::PowerOff, "tv").await.unwrap();
        r.apply_action(SessionAction::PowerOn, "tv").await.unwrap();
        // None must not call any primitive.
        r.apply_action(SessionAction::None, "tv").await.unwrap();

        assert_eq!(
            *r.calls.lock().unwrap(),
            vec![
                "screen_off:tv",
                "screen_on:tv",
                "power_off:tv",
                "power_on:tv",
            ]
        );
    }
}
