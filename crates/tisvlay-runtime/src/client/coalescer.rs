//! Per-key coalescing dispatch. See [`Coalescer`].

/// Per-key coalescing dispatch.
///
/// Rapid `fire(key, value)` calls are folded by the user-supplied
/// `merge` function before `apply(key, merged_value)` runs. Bounds
/// in-flight work to one worker task per key, regardless of input
/// rate.
///
/// Common merge choices:
/// - **Last-wins** for absolute values (slider position):
///   `|_, new| new`
/// - **Sum** for relative deltas (scroll-wheel ticks):
///   `|old, new| old + new` (where `V: Add`)
///
/// The worker for a given key is spawned lazily on the first
/// `fire(key, _)` and lives forever. Each burst draws one value via
/// `recv()`, drains anything else queued via `try_recv()` folding
/// through `merge`, then runs `apply(key, merged)`. New values
/// arriving while `apply` is running queue and form the next burst.
///
/// Use cases in this workspace:
/// - GUI slider drags: bound daemon writes to one in-flight call per
///   `(tv_id, property)` regardless of drag rate (last-wins).
/// - Tray scroll wheel: fold rapid scroll ticks into one daemon call
///   per `(tv_id, target)` burst (sum).
///
/// `Clone` is cheap (clones internal `Arc`s); pass clones into
/// closures that need to fire from multiple call sites.
pub struct Coalescer<K, V> {
    inner: std::sync::Arc<CoalescerInner<K, V>>,
}

type ApplyFn<K, V> =
    Box<dyn Fn(K, V) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

struct CoalescerInner<K, V> {
    senders:
        parking_lot::Mutex<std::collections::HashMap<K, tokio::sync::mpsc::UnboundedSender<V>>>,
    merge: Box<dyn Fn(V, V) -> V + Send + Sync>,
    apply: ApplyFn<K, V>,
}

impl<K, V> Clone for Coalescer<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> Coalescer<K, V>
where
    K: std::hash::Hash + Eq + Clone + Send + 'static,
    V: Send + 'static,
{
    /// Construct a coalescer with the given merge and apply functions.
    ///
    /// `merge(a, b) -> c` is called as `c = merge(latest, next)` to
    /// fold values queued during a single burst.
    ///
    /// `apply(key, merged)` runs the actual work. The returned future
    /// is `await`ed by the per-key worker; the next burst can't start
    /// for that key until it completes.
    pub fn new<Merge, Apply, Fut>(merge: Merge, apply: Apply) -> Self
    where
        Merge: Fn(V, V) -> V + Send + Sync + 'static,
        Apply: Fn(K, V) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self {
            inner: std::sync::Arc::new(CoalescerInner {
                senders: parking_lot::Mutex::new(std::collections::HashMap::new()),
                merge: Box::new(merge),
                apply: Box::new(move |k, v| Box::pin(apply(k, v))),
            }),
        }
    }

    /// Push `value` for `key`. Spawns a worker for this key if none
    /// exists yet. Non-blocking: returns immediately after enqueueing.
    pub fn fire(&self, key: K, value: V) {
        let mut map = self.inner.senders.lock();
        let tx = if let Some(tx) = map.get(&key) {
            tx.clone()
        } else {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<V>();
            let key_inner = key.clone();
            let inner = self.inner.clone();
            tokio::spawn(async move {
                while let Some(mut latest) = rx.recv().await {
                    while let Ok(v) = rx.try_recv() {
                        latest = (inner.merge)(latest, v);
                    }
                    (inner.apply)(key_inner.clone(), latest).await;
                }
            });
            map.insert(key, tx.clone());
            tx
        };
        drop(map);
        let _ = tx.send(value);
    }
}

impl<K, V> std::fmt::Debug for Coalescer<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coalescer")
            .field("active_keys", &self.inner.senders.lock().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::Coalescer;
    use parking_lot::Mutex;
    use tokio::sync::{mpsc, oneshot};
    use tokio::time::timeout;

    #[tokio::test]
    async fn values_fired_during_apply_merge_into_one_follow_up() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = oneshot::channel();
        let release = Arc::new(Mutex::new(Some(release_rx)));
        let (applied_tx, mut applied_rx) = mpsc::unbounded_channel();
        let coalescer = Coalescer::new(
            |latest: i32, next| latest + next,
            move |key: &str, value| {
                let key = key.to_string();
                let release = Arc::clone(&release);
                let started_tx = started_tx.clone();
                let applied_tx = applied_tx.clone();
                async move {
                    let wait = release.lock().take();
                    if let Some(wait) = wait {
                        started_tx
                            .send((key.clone(), value))
                            .expect("test is waiting for the first apply");
                        wait.await.expect("test releases the first apply");
                    }
                    applied_tx
                        .send((key, value))
                        .expect("test is waiting for each applied value");
                }
            },
        );

        coalescer.fire("tv-1", 1);
        assert_eq!(
            timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("first apply starts")
                .expect("first apply sends its start signal"),
            ("tv-1".to_string(), 1)
        );

        coalescer.fire("tv-1", 2);
        coalescer.fire("tv-1", 3);
        release_tx.send(()).expect("first apply is waiting");

        let first = timeout(Duration::from_secs(1), applied_rx.recv())
            .await
            .expect("first apply completes")
            .expect("first apply sends its value");
        let follow_up = timeout(Duration::from_secs(1), applied_rx.recv())
            .await
            .expect("merged follow-up completes")
            .expect("follow-up sends its value");
        assert_eq!(first, ("tv-1".to_string(), 1));
        assert_eq!(follow_up, ("tv-1".to_string(), 5));
    }
}
