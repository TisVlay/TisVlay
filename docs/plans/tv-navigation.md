# TV remote and gamepad navigation

## Goal

Make the settings GUI usable from the couch: a TV remote or a gamepad, on a TV across the room. Plasma Bigscreen is the target session, and the one QA surfaced the gap on. Today the remote can reach almost nothing. Most of the GUI's actions are pointer-only click areas, focus is invisible where it does land, and the window is sized for a desk.

The result should also be a better keyboard-only GUI on any desktop, since the remote arrives as keys.

## What the remote delivers

Researched on a KDE neon VM (Plasma Bigscreen 6.8.2, a DualShock 4 over USB) and in the upstream source.

- **Bigscreen turns controllers into keys.** `plasma-bigscreen-inputhandler` reads gamepads through SDL3 and CEC remotes through libcec, and replays them as evdev key presses through the xdg RemoteDesktop portal. The GUI sees an ordinary Wayland keyboard, so TisVlay needs no gamepad or CEC input code.
- **Slint keeps only the keys it can name.** Its winit backend drops any key without a Slint name (`i-slint-backend-winit` `to_slint_key`), so Back, Forward, Games, Cancel and Home never arrive, even though `Key.Back` exists: it maps from Qt only. What does arrive is the app's whole vocabulary:

| Key | Gamepad | CEC remote |
|---|---|---|
| Arrows | D-pad, left stick | Arrows |
| Return | Cross (A) | Select |
| Escape | Circle (B) | Back/Return, Exit |
| Tab / Backtab | R1 / L1 | — |
| Menu | Square (X) | — |

- **Escape is Back.** The gamepad's Circle sends Cancel and Escape together, and the CEC Back key sends Escape, so one key means Back on both.
- **Text entry already works.** Bigscreen's on-screen keyboard appears for a focused Slint text field and types into it, so the plan has no text-entry step.
- **No stream false positive.** The input handler's portal session doesn't register with the session's stream detector: `stream/state` stays empty while the controller drives the UI.

## What the GUI lacks

From an audit of the UI at the branch point:

- **Unreachable actions.** About 12 shared widgets (IconBtn, DangerBtn, UriBtn, CopyBtn, LinkText, interactive Chip, InlineError's buttons, SpinStepper) and about 8 one-off elements (sidebar items, Reload, TV and profile list rows, both "+" buttons, DiscoveryRow, the About brand area) are `TouchArea`-only. A remote can't reach navigation, TV or profile selection, Pair, Edit/Save/Cancel, Rename/Delete/Duplicate/Reset, the per-field restore-input and stream-gate toggles, Copy, Remove TV, or any About action. Settings is the only fully reachable pane.
- **Invisible stops.** The window-filling `nav-scope` FocusScope (`main.slint`) is a Tab stop and the initial focus. Every contextual-help "i" is an inert stop with no ring: about 10 on a TV detail.
- **No shared focus ring.** Std widgets draw fluent's focus border. ThemedSelect and ThemedSpinBox use an accent border that also means "open".
- **Dialogs aren't modal to the keyboard.** ConfirmDialog and NameInputDialog don't take focus or handle Escape, and Tab walks into the page behind. The pairing wizard, TV config edit mode and profile rename don't focus their first field, and focusing a field as it materializes trips Slint's property-recursion guard (`profiles_pane.slint`, the rename field).
- **No Back.** Escape works in three places: an open select, a pinned help bubble, the rename field.
- **Hover-only content.** Every icon-button tip, the About component health tips, the DiscoveryRow permissions remedy and the Reload tip.
- **Fixed desk sizing.** A 720×480 minimum and preferred size, compile-time `Design` tokens, about 267 literal px values, no scale handling, and a Settings pane with no ScrollView.

Slint gives us more than expected in two places: Tab/Backtab traversal is built in (item-tree order, skipping hidden items), and a newly focused item is scrolled into view through every enclosing scroll area. It has no arrow-key navigation, and no public "focus next" API in `.slint` or Rust.

## Decisions

1. **The input vocabulary is the five keys above, and nothing else.** The GUI binds no gamepad or CEC input, so it works the same under any session that maps a remote to keys, and with a plain keyboard. *Why:* Bigscreen already owns the device side, and Slint can't see the other keys anyway.
2. **One focusable base for every custom control.** A shared component owns the FocusScope (covering the whole widget, so scroll-into-view reveals all of it), a focus ring drawn from a new `Design` focus token, and Return/Space activation that calls the same action the widget's `accessible-action-default` already runs. Every TouchArea-only widget moves onto it, and ThemedSelect and ThemedSpinBox draw the same ring, distinct from their "open" look. *Why:* one place for the look and the keys, and the accessibility actions already name each widget's activation.
3. **Arrows step through the Tab order; no spatial navigation yet.** Up/Down always move to the previous/next stop. They're taken from text fields (single-line, so they have no use for them) and from a closed ThemedSelect, which stops opening on Down; Return and Space still open it. Left/Right stay with the focused widget first (a slider adjusts, a text field moves its cursor), and step to the previous/next stop only when nothing used them. Movement synthesizes Tab/Backtab, because Slint offers no other way to say "next". Whether that happens in the winit event hook before Slint sees the key, or in a `nav-scope` handler with a deferred dispatch, is settled by the first step's spike. *Why:* the Tab order already matches the reading order of every pane, so linear stepping covers the GUI at a fraction of the cost of row/column rules. True 2D navigation goes to an idea doc, to revisit once the linear version has been used from the couch.
4. **Menu shows help for the focused control.** The "i" buttons leave the Tab order. Menu on a focused control toggles its row's contextual help, or the control's own tip where it has one. That covers every hover-only tip as well. *Why:* about 10 inert stops on a TV detail are a lot of presses on a remote, and Menu is otherwise unused.
5. **Escape walks back one level, owned by `nav-scope`.** In order: an open select or pinned help closes (they handle it already). Then a dialog cancels, and an edit (TV config, profile rename) cancels, as profile rename already does. Then the pairing wizard steps back, or cancels from its first step. Otherwise it's history back through the existing `nav-back`, and at the start of history focus moves to the active sidebar item. *Why:* one owner and one order, and the widgets that already handle Escape keep it, since a handler below `nav-scope` sees the key first.
6. **Dialogs hold focus.** ConfirmDialog and NameInputDialog focus their first control on open, cancel on Escape, and cycle Tab/Backtab among their own controls with explicit `focus()` calls. Edit modes and the pairing wizard focus their first field on entry. Every focus-on-open is deferred a frame, around the recursion guard. *Why:* the two dialog components cover all six dialog call sites, and Slint's traversal can't be scoped to a subtree.
7. **List rows select on Return, not on focus.** TV, profile and discovered-TV rows become focusable, and Return selects. *Why:* it matches button activation, and stepping through the list doesn't swap the detail pane on every press. Selection that follows focus goes in the idea doc beside 2D navigation.
8. **A TV mode, entered from Bigscreen's signal or by hand.** `PLASMA_PLATFORM=mediacenter` (set by the Bigscreen session, and the key its Qt apps choose their TV layouts by) turns it on, and a GUI flag forces it on or off. TV mode opens the window maximized and scales the whole UI by a factor on top of the compositor's. *Why:* a uniform scale covers the literal px values that a token-only large mode would miss. Whether the scale is applied through `SLINT_SCALE_FACTOR` before backend init (which freezes the scale against later compositor changes) or a `ScaleFactorChanged` dispatch after the window opens is the sizing step's spike. A TV-tuned layout with its own tokens isn't planned.

## Steps

Each step is a commit on `feat/bigscreen`. GUI tests drive keys through `slint::Window::dispatch_event` and observe focus through side effects (Return fires the focused control's action), since the testing backend has no public key helper or focus accessor. Hardware checks run on the neon VM with the DualShock 4 under Bigscreen.

1. **Key-test harness, and clean up the Tab order.** A test helper sends key presses and releases to the testing window, together with its first use: `nav-scope` and the "i" buttons leave the Tab order. Spike, before building on it: confirm Tab traversal and `is_visible` work without an event loop (the window may need `set_size`), and that a synthesized Tab can be dispatched from both candidate places in decision 3. *Verified by:* a test that Tabs from a fresh window and lands on the first real control. On the VM, the first R1 press shows focus on a control.
2. **The focusable base and the focus ring.** The component, its `Design` token, and the shared widgets moved onto it: IconBtn, DangerBtn, UriBtn, CopyBtn, LinkText, interactive Chip, InlineError's buttons. ThemedSelect and ThemedSpinBox adopt the ring. SpinStepper stays pointer-only, since the spin box's field takes the arrows. *Verified by:* tests that Tab to each widget kind and fire it with Return. On the VM, the ring reads clearly at distance.
3. **The one-off controls and list rows.** Sidebar items, Reload, both "+" buttons, the About brand area, and TV, profile and discovered-TV rows with Return-to-select. *Verified by:* tests that pick a TV and a profile, and start pairing, by keys alone. On the VM, every pane is walkable with R1/L1.
4. **Arrow keys.** Decision 3's rules, including taking Up/Down from text fields and ThemedSelect's open-on-Down. *Verified by:* tests for stepping, for a slider keeping Left/Right, and for a text field keeping Left/Right while losing Up/Down. On the VM, the D-pad walks every pane.
5. **Dialogs and edit modes hold focus.** Decision 6. *Verified by:* tests that a dialog opens focused, cycles Tab inside itself, and cancels on Escape. A rename focuses its field on entry without tripping the recursion guard.
6. **Escape walks back.** Decision 5's chain in `nav-scope`. *Verified by:* a test for each level of the chain. On the VM, Circle backs out of a dialog, an edit, the wizard and history in turn.
7. **Menu shows help.** Decision 4. *Verified by:* tests that Menu on a focused row opens its help and Menu again closes it, and that an icon button's tip shows. On the VM, Square opens and closes help.
8. **TV mode.** Decision 8, plus a ScrollView for the Settings pane, which clips first at a large scale. *Verified by:* a unit test of mode detection. On the VM, a Bigscreen launch opens maximized and scaled, and a desktop launch doesn't change. On a real TV under Bigscreen, text reads from the couch.
9. **Docs, and retire the plan.** A book section on driving the GUI with a remote, the focus and key model in `docs/architecture.md`, and an idea doc for 2D navigation and focus-follows selection. The plan file is deleted.

## Deviations

None yet.
