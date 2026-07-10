//! Object-safe wrapper over `tisvlay_core::TvDiscovery` (whose native-async
//! `discover` makes the trait itself not object-safe), so the bridge can
//! hold a vendor → discovery map of trait objects.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tisvlay_core::{DiscoveredTv, TvDiscovery};

pub trait DynDiscovery: Send + Sync {
    fn discover(
        &self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = tisvlay_core::Result<Vec<DiscoveredTv>>> + Send + '_>>;
}

impl<T: TvDiscovery> DynDiscovery for T {
    fn discover(
        &self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = tisvlay_core::Result<Vec<DiscoveredTv>>> + Send + '_>> {
        Box::pin(TvDiscovery::discover(self, timeout))
    }
}
