# Deterministic network simulation for the LG reconnect logic

The LG client's reconnect and backoff behavior depends on timing and on the order network events arrive in: the TV closes its WebSocket on standby, wakes slowly after Wake-on-LAN, and can refuse the first few connections. Today's tests run it against a local test server in real time, which covers the happy paths but can't easily script "the TV drops mid-request, then refuses twice, then accepts".

## How

[turmoil](https://crates.io/crates/turmoil) runs tokio code against a simulated network with a deterministic clock. Partitions, drops and delays are scripted and replay exactly, so a reconnect bug found once reproduces every time.

- **Scope is the vendor layer only:** the LG client and connection over a simulated socket. zenoh doesn't run under turmoil, so the daemon and session stacks stay on their real-socket tests.
- **The transport would need a seam** so the client can dial a turmoil socket in tests, much as the CEC backend's `CecLink` seam lets its tests use a fake link.

## Trade-offs to budget for

- **The seam itself:** a socket abstraction in `tisvlay-lg` exists only for tests until a second user appears.
- **Simulated isn't real:** firmware quirks (wss vs ws, the two-phase pairing prompt) still need the real-TV field batch.

## Trigger conditions

- A reconnect or backoff bug reported from the field that the real-time tests can't reproduce.
- Work on the connection layer that changes its retry behavior.
