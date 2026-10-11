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
