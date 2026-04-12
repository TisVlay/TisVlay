//! Observability regression tests using `tracing-mock`.
//!
//! Assert that specific production events are emitted with the right
//! fields. Catches refactors that silently remove instrumentation we
//! depend on for debugging and ops.
//!
//! tracing-mock's subscriber is strict: every span enter/exit and
//! event must be accounted for in order. So tests include the full
//! span lifecycle (new → enter → event → exit → drop) which makes
//! them also a regression test for the `#[instrument]` attributes.

use tisvlay_test_support::tracing_mock::{expect, subscriber};
use tracing::subscriber::set_default;

#[test]
fn publish_tv_emits_instrumented_debug_event() {
    // publish_tv has #[instrument(fields(tv_id = %tv.id, address = %tv.address))].
    // tv_id lives on the span, not on the inner debug! event.
    let (sub, handle) =
        subscriber::mock()
            .new_span(expect::span().named("publish_tv").with_fields(
                expect::field("tv_id").with_value(&tracing::field::display("lr_trace")),
            ))
            .enter(expect::span().named("publish_tv"))
            .event(expect::event().at_level(tracing::Level::DEBUG))
            .exit(expect::span().named("publish_tv"))
            .drop_span(expect::span().named("publish_tv"))
            .run_with_handle();

    let _guard = set_default(sub);

    let (_connector, _discovery, mock) = tisvlay_mock::new_test_parts();
    mock.publish_tv(tisvlay_core::DiscoveredTv::new(
        "lr_trace",
        "Living Room",
        "mock://lr_trace",
        "mock",
    ));

    handle.assert_finished();
}

#[test]
fn simulate_user_accept_emits_instrumented_debug_event() {
    // simulate_user_accept has #[instrument(fields(tv_id = %tv_id))].
    // The inner set_pairing_outcome emits debug!(?outcome, ...) — the
    // outcome field is on the event (Debug format).
    let (sub, handle) =
        subscriber::mock()
            .new_span(expect::span().named("simulate_user_accept").with_fields(
                expect::field("tv_id").with_value(&tracing::field::display("mock://lr")),
            ))
            .enter(expect::span().named("simulate_user_accept"))
            .event(
                expect::event()
                    .at_level(tracing::Level::DEBUG)
                    .with_fields(expect::field("outcome")),
            )
            .exit(expect::span().named("simulate_user_accept"))
            .drop_span(expect::span().named("simulate_user_accept"))
            .run_with_handle();

    let _guard = set_default(sub);

    let (_connector, _discovery, mock) = tisvlay_mock::new_test_parts();
    mock.simulate_user_accept("mock://lr");

    handle.assert_finished();
}
