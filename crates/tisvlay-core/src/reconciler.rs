//! Optimistic-value reconciliation gating.
//!
//! [`Reconciler`] is the per-value gate: tracks user intent vs.
//! authoritative observations, drops stale ones during a transition.
//! [`ReconcilerMap`] is the per-key wrapper used by consumers that
//! need a `Reconciler` per `(tv_id, property)`.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;

/// Optimistic-value reconciliation gate.
///
/// Models the "user intent vs. authoritative observation" pattern that
/// shows up in any UI controlling a remote endpoint that takes time to
/// converge:
///
/// - The **optimistic value** (`current()`) is what the user has
///   signaled they want. UI elements display this; new user commands
///   compute deltas against this.
/// - **Authoritative observations** (`observe(value)`) come from
///   outside (a daemon's state-change push, a poll result).
/// - Between a [`dispatch`](Self::dispatch) and the matching
///   [`observe`](Self::observe), intermediate observations are
///   gated out — they represent transient state (e.g., a TV
///   ramping toward a target) that should not bounce the optimistic
///   value backward.
/// - Once an observation matches the last dispatched value the gate
///   opens; subsequent observations apply directly, allowing genuine
///   external changes (e.g., a different client moves the value) to
///   propagate.
///
/// # When to use
///
/// Any time the GUI/tray reads a remote-mirrored value AND issues
/// commands relative to that value. Without gating, the apply layer
/// reads a transient (mid-ramp) observation as the base for the next
/// delta and lands somewhere unexpected. The tray's scroll-wheel
/// dispatch is the canonical case: a fast forward burst followed by
/// a small back scroll otherwise lands at a mid-ramp value minus the
/// back-scroll's delta, far below where the user expects.
///
/// Both the GUI and the tray drive their per-row state through
/// [`ReconcilerMap`] (one `Reconciler` per `(tv_id, property)`); the
/// Slint side stays a thin view that reads the `current()` value.
///
/// # Concurrency
///
/// `Clone` is cheap (shares an `Arc`); pass clones into closures.
/// Internal state is guarded by a `parking_lot::Mutex` — operations are
/// non-blocking in the runtime sense (no `.await`) and the lock has no
/// poisoning, so we never need to think about recovery semantics.
pub struct Reconciler<V> {
    inner: Arc<Mutex<ReconcilerInner<V>>>,
}

struct ReconcilerInner<V> {
    /// Current optimistic value — what `current()` returns.
    optimistic: V,
    /// Most recent value passed to `dispatch()`.
    last_sent: V,
    /// True between a dispatch and a matching observation.
    pending: bool,
}

impl<V> Reconciler<V>
where
    V: Copy + PartialEq + Send + 'static,
{
    /// Construct with an initial optimistic value (matching the
    /// authoritative state at construction time).
    pub fn new(initial: V) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ReconcilerInner {
                optimistic: initial,
                last_sent: initial,
                pending: false,
            })),
        }
    }

    /// Get the current optimistic value (what UI should display, what
    /// new commands should compute deltas against).
    pub fn current(&self) -> V {
        self.inner.lock().optimistic
    }

    /// Record a user-driven dispatch. The optimistic value updates
    /// immediately; a subsequent `observe(value)` matching this value
    /// will close the gate.
    pub fn dispatch(&self, value: V) {
        let mut inner = self.inner.lock();
        inner.optimistic = value;
        inner.last_sent = value;
        inner.pending = true;
    }

    /// Reconcile against an authoritative observation.
    ///
    /// - If pending and the observation matches `last_sent`, the
    ///   authority has converged: clear pending.
    /// - If pending and the observation differs, it's a stale
    ///   intermediate (e.g., TV ramp): drop.
    /// - If not pending, the observation represents an external
    ///   change: update the optimistic value to match.
    pub fn observe(&self, value: V) {
        let mut inner = self.inner.lock();
        if inner.pending {
            if value == inner.last_sent {
                inner.pending = false;
            }
            // else: stale intermediate — drop.
        } else {
            inner.optimistic = value;
            inner.last_sent = value;
        }
    }
}

impl<V> Clone for Reconciler<V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<V: std::fmt::Debug> std::fmt::Debug for Reconciler<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("Reconciler")
            .field("optimistic", &inner.optimistic)
            .field("last_sent", &inner.last_sent)
            .field("pending", &inner.pending)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ReconcilerMap
// ---------------------------------------------------------------------------

/// Per-key wrapper around [`Reconciler<V>`] — a `HashMap<K,
/// Reconciler<V>>` with the small set of methods every consumer
/// actually uses (observe / dispatch / current). Reconcilers are
/// created lazily on first use per key.
///
/// Used by both the tray (per `tv_id`, one map per writable property
/// like volume / backlight) and the GUI (same shape, different
/// callbacks). Single-implementation gating across both consumers.
///
/// `Clone` is cheap (shares an `Arc`).
pub struct ReconcilerMap<K, V> {
    inner: Arc<Mutex<HashMap<K, Reconciler<V>>>>,
}

impl<K, V> Default for ReconcilerMap<K, V> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<K, V> Clone for ReconcilerMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> ReconcilerMap<K, V>
where
    K: Hash + Eq + Clone,
    V: Copy + PartialEq + Send + 'static,
{
    /// An empty map; reconcilers are created per key on first use.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed an authoritative observation; lazily creates the
    /// per-key reconciler on first use. Returns the
    /// post-observation `current()` so callers can write the gated
    /// value into their own caches (`CachedTvState`, `TvViewModel`).
    pub fn observe(&self, key: &K, value: V) -> V {
        let mut map = self.inner.lock();
        let r = map
            .entry(key.clone())
            .or_insert_with(|| Reconciler::new(value));
        r.observe(value);
        r.current()
    }

    /// Read the current optimistic value as a base for a new delta.
    /// Returns `fallback` if no reconciler exists yet for this key
    /// (no observation has fired, e.g. immediately after pairing).
    pub fn current(&self, key: &K, fallback: V) -> V {
        self.inner
            .lock()
            .get(key)
            .map(|r| r.current())
            .unwrap_or(fallback)
    }

    /// Record a user-driven dispatch for the gate. Lazily creates the
    /// reconciler if none exists yet.
    pub fn dispatch(&self, key: &K, target: V) {
        let mut map = self.inner.lock();
        let r = map
            .entry(key.clone())
            .or_insert_with(|| Reconciler::new(target));
        r.dispatch(target);
    }
}

impl<K: std::fmt::Debug, V> std::fmt::Debug for ReconcilerMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconcilerMap")
            .field("active_keys", &self.inner.lock().len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod reconciler_tests {
    use super::Reconciler;

    #[test]
    fn initial_value_is_returned_by_current() {
        let r = Reconciler::new(42u8);
        assert_eq!(r.current(), 42);
    }

    #[test]
    fn dispatch_updates_optimistic_immediately() {
        let r = Reconciler::new(0u8);
        r.dispatch(50);
        assert_eq!(r.current(), 50);
    }

    #[test]
    fn pending_observations_that_dont_match_are_dropped() {
        let r = Reconciler::new(0u8);
        r.dispatch(50);
        // Simulated TV ramp: 10, 20, 30, 40 — none match 50.
        r.observe(10);
        assert_eq!(
            r.current(),
            50,
            "intermediate ramp value must not regress optimistic"
        );
        r.observe(20);
        r.observe(30);
        r.observe(40);
        assert_eq!(r.current(), 50);
    }

    #[test]
    fn observation_matching_last_sent_clears_pending() {
        let r = Reconciler::new(0u8);
        r.dispatch(50);
        r.observe(50);
        assert_eq!(r.current(), 50);
        // After convergence, an external change applies (would be
        // dropped before convergence).
        r.observe(80);
        assert_eq!(r.current(), 80);
    }

    #[test]
    fn external_change_while_idle_updates_optimistic() {
        let r = Reconciler::new(20u8);
        // No dispatch — pending=false. External observation applies.
        r.observe(75);
        assert_eq!(r.current(), 75);
    }

    #[test]
    fn back_scroll_during_ramp_uses_intent_as_base() {
        // Models the tray's scroll-asymmetry bug: a fast forward burst
        // followed by a small back scroll while the TV is still
        // ramping. The reconciler keeps `current()` pinned to the
        // user's intent throughout, so the apply layer's
        // `current() + delta` lands where the user expects.
        let r = Reconciler::new(0u8);
        r.dispatch(50);
        // TV ramps 0→50 via 10, 20, 30, 40. Each observation arrives
        // BEFORE the back-scroll dispatch.
        r.observe(10);
        r.observe(20);
        r.observe(30);
        // User scrolls back -2 mid-ramp. Apply reads `current()`.
        let base = r.current();
        assert_eq!(base, 50, "user-intent base, NOT the bouncing observation");
        let new_target = base - 2;
        r.dispatch(new_target);
        assert_eq!(r.current(), 48);
        // TV now ramps from wherever it was (~30) toward 48. None of
        // those intermediates are 48, so they're all dropped.
        r.observe(35);
        r.observe(40);
        r.observe(45);
        assert_eq!(r.current(), 48);
        r.observe(48);
        assert_eq!(r.current(), 48);
    }

    #[test]
    fn re_dispatch_before_convergence_replaces_target() {
        let r = Reconciler::new(0u8);
        r.dispatch(50);
        r.observe(20); // mid-ramp, dropped
        // User changes mind before TV reaches 50.
        r.dispatch(70);
        // Old ramp value (50) arrives — dropped because we now want 70.
        r.observe(50);
        assert_eq!(r.current(), 70);
        r.observe(70);
        assert_eq!(r.current(), 70);
    }
}

#[cfg(test)]
mod reconciler_map_tests {
    use super::ReconcilerMap;

    #[test]
    fn current_returns_fallback_when_key_absent() {
        let m: ReconcilerMap<String, u8> = ReconcilerMap::new();
        assert_eq!(m.current(&"absent".to_string(), 99), 99);
    }

    #[test]
    fn observe_lazily_creates_reconciler_seeded_with_observation() {
        let m: ReconcilerMap<String, u8> = ReconcilerMap::new();
        let key = "tv1".to_string();
        // First observation seeds the reconciler with that value
        // (pending=false → idle apply path).
        let after = m.observe(&key, 30);
        assert_eq!(after, 30);
        // Subsequent reads via `current` return the observed value.
        assert_eq!(m.current(&key, 0), 30);
    }

    #[test]
    fn dispatch_lazily_creates_reconciler_with_pending_target() {
        let m: ReconcilerMap<String, u8> = ReconcilerMap::new();
        let key = "tv1".to_string();
        m.dispatch(&key, 50);
        assert_eq!(m.current(&key, 0), 50);
        // Stale observation during pending is dropped.
        let after = m.observe(&key, 20);
        assert_eq!(after, 50, "intent stays at 50; ramp value 20 dropped");
        // Convergent observation clears pending.
        let after = m.observe(&key, 50);
        assert_eq!(after, 50);
        // Now external changes apply directly.
        let after = m.observe(&key, 80);
        assert_eq!(after, 80);
    }

    #[test]
    fn keys_are_independent() {
        let m: ReconcilerMap<String, u8> = ReconcilerMap::new();
        let a = "tv_a".to_string();
        let b = "tv_b".to_string();
        m.dispatch(&a, 50);
        m.dispatch(&b, 80);
        // Observation on b doesn't affect a's pending state.
        let _ = m.observe(&b, 80);
        assert_eq!(m.current(&a, 0), 50);
        let after_stale = m.observe(&a, 30);
        assert_eq!(after_stale, 50, "a still pending at 50, 30 is stale");
    }
}
