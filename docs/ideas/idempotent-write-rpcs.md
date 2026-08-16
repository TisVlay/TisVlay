# Idempotent write rpcs (client-minted ids)

The shared `caller::Caller` re-issues a rpc exactly once when its reply channel closes empty *after* querier matching was confirmed (a queryable dropped mid-call, or a reply lost in flight). That re-issue is enabled only for the **read** family (`info`, `vendors`, `pairing-schema`, `get-config`, profile defaults/effective/baselines, tray `notification_settings`) — reads are naturally idempotent.

The **write** rpcs stay single-shot: `pair`, `add-tv`, and `discover-start` mint server-side state (random device UUIDs, a scan session) *before* the deferred ack, so a blind re-issue can duplicate that state. This leaves a real gap — a write whose reply is lost after matching surfaces as a failure the client can't safely retry, even though the daemon may have applied it.

**Path forward.** Generate the identifiers client-side so the write becomes idempotent under re-issue:

- `add-tv`: the daemon already honors a caller-supplied `AddTv.id`, so the client can mint the UUID and re-issue is a no-op on the second arrival.
- `pair`: same shape — mint the device UUID client-side.
- `discover-start`: carry a client-minted session id so a re-issued start rejoins the existing scan rather than spawning a second.

With ids owned by the caller, the `caller::Caller` re-issue can extend to the write family too, closing the lost-ack window for the operations that most need it.

**Trade-offs.** Small surface, but it moves UUID authority from daemon to client for these verbs — the daemon must treat a repeat id as the same operation (upsert / rejoin), not an error. Worth doing when a user reports a pairing or add-tv that "failed" but actually took, or when the write-rpc lost-ack window is observed in the field.

Filed from the GUI-state-consistency work (the matching-gated `caller::Caller`); git history has the full rationale.
