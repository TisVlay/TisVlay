## The pairing wizard can keep the TV-prompt text under "Paired successfully"

**Status**: open
**Severity**: low — cosmetic; the pairing itself succeeds
**Found by**: the promo video's setup capture (`docs/promo/shots/setup.toml`), where the finished wizard read "Paired successfully" over "Please accept the pairing prompt on your LG TV"

### Problem

While a pairing waits on the TV, the backend's prompt text ("Please accept the pairing prompt on your LG TV", from `crates/tisvlay-lg/src/connector.rs`) reaches the wizard through the progress stream: `make_progress_callback` in `crates/tisvlay-gui-app/src/wizard.rs` writes each sample's phase and message to `PairingWizard`.

When the pair rpc returns, `publish_pair_outcome` sets the phase to complete and writes its own success line ("Client key stored.") only `if wizard.get_pair_message().is_empty()`. If the last progress message is still the prompt text when the reply lands, the guard keeps it, and the finished step shows the heading for success over an instruction that no longer applies.

Whether it shows depends on order: the reply against the progress stream's last sample. The same capture, unchanged, has ended both ways: "Client key stored." in one run, the stale prompt in others.

### Not yet known

- Whether a terminal progress sample is meant to clear the message, and is being lost or arriving late, or whether no sample ever clears it and the clean runs were the reply winning the race the other way.
- Whether it shows with a real TV. The capture pairs through the promo driver's connector, which sends the same two states the LG connector does (the prompt, then complete), but answers faster than a TV.

### Fix plan

Decide which side owns the finished step's text. The simplest rule: on success `publish_pair_outcome` always writes its line, since a prompt can't still be pending once the pairing completed. A unit test beside `edit_pane.rs`'s existing assertions on `get_pair_message` would pin it: a progress message set, then a successful outcome, reads as the success line.
