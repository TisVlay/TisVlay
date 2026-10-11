# Sit the rpc layer on zenoh-ext's typed queryables

Every rpc is declared once in `tisvlay-wire`'s `endpoint` module, and the `Caller` and the servers' `Call` enums do the serde work at each end. zenoh has an open proposal, [eclipse-zenoh/zenoh#2510](https://github.com/eclipse-zenoh/zenoh/issues/2510) (opened 2026-03), for typed wrappers in zenoh-ext (`TypedPublisher<T>`, `TypedSubscriber<T>`, `TypedQueryable<Req, Resp>`) built on its own serializer. It's an application-layer extension with no protocol change, motivated by exactly the boilerplate our layer removes.

## If it lands

- **What could move:** the per-call encode and decode inside `Caller` and `serve`, onto the upstream wrappers. The endpoint table, the request enums and `Reply<R>` stay ours: they encode our key space and dispatch, which no generic wrapper knows about.
- **What to check first:** the upstream wrappers use zenoh's `ZSerializer` by default, not JSON. Our payloads are JSON on purpose, since `tisvlay watch` and raw gets stay readable, so adoption needs a JSON codec hook or stays with serde_json.
- **What must survive:** the matching-gated querier (the querier-matching law in `docs/architecture.md`) and the client-mode-server rule. A wrapper that declares its own queriers must still let us await matching before the first get.

## Trigger conditions

- #2510 merges into a zenoh release the workspace adopts.
