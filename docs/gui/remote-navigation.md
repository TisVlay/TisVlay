# Remote and keyboard navigation (GUI)

How `tisvlay-gui` is driven by keys alone: a TV remote or a gamepad under a 10-foot shell such as Plasma Bigscreen, or a keyboard on a desktop. The user's side is the book's [GUI & Tray](../book/src/guide/gui-tray.md#from-a-remote-or-gamepad) chapter; this is the model behind it and where each part lives. The research into row navigation is [docs/ideas/gui-spatial-navigation.md](../ideas/gui-spatial-navigation.md), and the help system's keyboard side is in [contextual-help.md](contextual-help.md).

## The contract

**The GUI binds no gamepad or CEC input; it reads keys.** Bigscreen's input handler turns gamepads (SDL3) and CEC remotes (libcec) into evdev key presses through the xdg RemoteDesktop portal, so the GUI sees an ordinary Wayland keyboard; a TV that forwards its remote's keys through the daemon's CEC backend delivers them through the kernel's `rc-cec` keymap, the same way but for two keys the hook remaps. A key-level contract works under any session that maps a remote to keys, and with a keyboard.

**Invariant: every function is reachable with arrows, Return and Escape alone.** Tab and Menu are accelerators, since CEC remotes have neither. Space, Alt+arrows and the mouse's back and forward buttons keep working. The vocabulary; the gamepad column was checked on a KDE neon VM with Plasma Bigscreen and a DualShock 4, the CEC columns traced through Bigscreen's input handler source and the kernel keymap, xkb and winit:

| Key | Gamepad | CEC remote, through Bigscreen | CEC remote, through the daemon |
|---|---|---|---|
| Arrows | D-pad, left stick | Arrows | Arrows |
| Return | Cross (A) | Select | Select |
| Escape | Circle (B) | Back/Return, Exit | Exit (remapped from `XF86Close`) |
| Tab, Shift+Tab | R1, L1 | — | — |
| Menu | Square (X), remapped from `XF86MenuKB` | — | Info (remapped from `XF86Info`) |
| Escape | Share, L2, remapped from `XF86Back` | — | — |

- Escape is Back on both: the gamepad's Circle sends Cancel and Escape together, and the CEC Back key sends Escape.
- L1 arrives as Shift+Tab, never as Backtab. Slint turns Shift+Tab into Backtab itself, so a handler that wants "previous" checks both.
- Slint's winit backend drops a key that has neither a Slint name nor text. The gamepad's Square sends KEY_MENU, which xkb maps to `XF86MenuKB` (the keyboard's Menu key is KEY_COMPOSE and arrives named), and Share and L2 send KEY_BACK, `XF86Back`, which winit names `BrowserBack` but Slint has no name for. Through the kernel's CEC keymap, Exit is KEY_EXIT, `XF86Close`, which winit names `Close` and Slint doesn't, and Info is KEY_INFO, `XF86Info`, which winit leaves unidentified. The winit hook remaps all four ([below](#the-winit-hook)). Forward, Games, Cancel and Home are dropped and stay dropped.
- The on-screen keyboard follows text focus from the remote's own input path. Bigscreen sets `KWIN_IM_SHOW_ALWAYS=1`, and its keyboard rises as soon as keyboard focus reaches a text field from the gamepad; a hardware keyboard, or a virtual one injected into the VM, doesn't raise it, since KWin shows the keyboard by the last input device. What the D-pad does with the keyboard up is checked at the controller, not by injection.
- No stream false positive: the input handler's portal session doesn't register with the session's stream detector.

## One focusable base, one ring

`Focusable` (`ui/widgets.slint`) is a Rectangle-rooted component with an inner `FocusScope` behind its `TouchArea`. The scope has `focus-on-click: false`, so the first click isn't spent on taking focus; the click handler focuses explicitly, so a pointer user's next keys continue from the control they clicked. The base owns `accessible-role`, `accessible-enabled`, `accessible-action-default` and Return/Space activation, all calling one `activated` callback. The std Button, Switch and CheckBox are replaced by themed widgets on it, as `ThemedSelect` is; `OptimisticSlider` keeps fluent's Slider inside a wrapper that draws the ring on its `has-focus`; `LineEditField` is a bare `TextInput` in the shared control box (a std `LineEdit` leaves the Tab order only through `enabled: false`, which dims it, and a read-only one stays a stop). The accessible actions double as the test surface: the `accessibility` feature is off in production.

The ring (`FocusRing`) is an accent outline, `Theme.focus-ring`, `Design.ring-width` wide at `Design.ring-offset` outside the control with the control's corner radius; `ring-inside` draws it inside a row that fills a clipped column. It shows only while `Nav.focus-visible` is set: the shell's capture handler sets it on a navigation key (Tab, the arrows, Return, Space) and a pointer press on a control clears it, so a mouse user never sees rings trail their clicks. The ring stays on a focused control that turns disabled. A landing made without a key (the wizard closing, a history step) places focus but doesn't show the ring; the next navigation key does.

What a focusable tells the shell: `column` (0 the sidebar, 1 a list, 2 the detail or a single-column pane), `tab-stop` (whether Tab visits it), `nav-row` (the detail row it sits in), `reveal` (whether a pane scrolls to keep it in view), and the `focus-in` callback, which runs however focus arrived.

## Regions

Three columns, sidebar, list and detail, and inside the detail, rows. Up and Down step inside the region focus is in; Left and Right go to the focused control first, then walk the row, then jump columns.

- **The sidebar** is a rail: the four items and Reload, stepped by the shell itself (`rail-step` in `ui/main.slint`), stopping at both ends. The pane follows focus along it, and Return or Right enter the pane: its list's selected row, or its first control. A click stays on the item.
- **A list** (the TVs, the profile list's two sections in order, the wizard's discovered TVs, each with its "+") steps its own stops. The shell asks through `Nav.request-list-step`; the focused row or "+" answers by focusing its neighbor and marks the request served, and the shell's 1 ms fallback steps the Tab order when none does, which is how Down past the last discovered TV continues into the manual-entry fields. Focusing a row selects it, however focus got there (a key, a click, a landing the shell placed), so a list never shows an empty detail. Return or Right enter the detail's first stop: a TV's Power button, a profile's Duplicate button (a `forward-focus` target can't sit inside a conditional, and the header's Rename swaps with the rename field), a discovered TV's Next in the footer.
- **Roving Tab stops.** Only the active sidebar item and the selected row (the first, with nothing selected) are Tab stops (`Focusable.tab-stop`, Slint's `focus-on-tab-navigation`), ARIA's roving tabindex. A desktop Tab goes active item, Reload, "+", selected row, detail, without switching or selecting anything on the way. The detail's stops all stay Tab stops.
- **The detail steps by rows.** A row component (`FormRow`, `LabeledRow`, the profile field rows, the toolbars, the About header's two rows, the wizard's footer) takes an id from `Nav.next-row-id()` as it's created and hands it to its stops through `nav-row`; every stop reports its column and row through `Nav.stop-focused` as it takes focus, and a focusable, the select and the text field's ring scope report their `accessible-id` too (the slider, the spin box and an editing text input report none): `Focusable`, the select and the field synchronously from `focus-gained`, the slider and the spin box from the deferred `changed has-focus`, which the walk flushes between hops. Up and Down land on the previous or next row's first stop, its control; Right walks out to the row's trailing icons (the "i", reset, restore, gate) and Left walks back. Every icon stays a stop: nothing hides behind a contextual key. The walks run in Rust (`keys.rs`): `row_step` synthesizes Tabs until the row changes (a Backtab then walks on to that row's first stop), flushing the deferred change handlers between hops, and undoes a walk that changed the column instead, so the detail stops at its ends; `stop_step` takes one Tab and undoes it when the row changed, returning false so the shell jumps columns. A stop in no row (0: a dialog's button, a pane that isn't a form) steps as a row of its own.
- **Value controls, in couch mode.** A focused slider or spin box is idle until Return enters it (`Nav.value-idle`): idle, Left and Right walk its row, and the shell takes them in its capture phase before the control would; entered, they adjust the value, and Return or Escape leaves; a soft fill says the slider is entered, the caret that the spin box is. On a desktop Left and Right go to the control first, as everywhere. A held arrow on a slider sends the last value, not a stream: volume and backlight go through the GUI's `Coalescer`. A spin box commits on the way out only what was typed into it: a row walk passes through it too, and must not write the value it shows. Change handlers run newest first, so when focus moves straight from one value control to the next the losing control's handler runs last: `stop-focused` clears `Nav.value-idle` for every new stop, a value control sets it after reporting, and a control clears it on focus loss only while it's still the last to have reported.
- **Column jumps.** Left from a detail goes to its selected list row, Left from a list or a single-column pane to the active sidebar item, Right from the sidebar into the pane. The requests go through `Nav.request-column`: the pane that exists answers by observing `column-tick`, and every answerer marks the request served (`column-served`), because a list created by the same change that made the request (the wizard closing) checks that marker in its `init` instead, and an unmarked old request would pull focus into the next TV list created. A TV list with nothing to focus (no TVs, the "+" disabled with the daemon down) sends the request back to the sidebar, and the wizard answers at every step: its discovery rows or address field, else the footer's enabled button.

## Keys, in the shell

`nav-scope` (`ui/main.slint`) is the window's `forward-focus` and the home of key handling, not a Tab stop (`focus-on-tab-navigation: false`: a stop there is invisible and inert, and Shift+Tab from the first control would land on it instead of wrapping). It keeps `focus-on-click`, which is how a click on empty background dismisses a pinned help bubble.

Its capture phase sees a key before the focused control does: it counts the key (`Nav.key-seen`), reveals the ring on a navigation key, restores the last column on an arrow while the scope itself has focus (a fresh window, a click on empty background) rather than walking from stale state, takes Up and Down for `step-focus` unless a select's list is open or Alt or Ctrl is held (so a text field's caret moves don't keep them), takes Left and Right for `horizontal-step` while a value control is idle, and takes Menu for `Help.menu-toggle(Nav.focus-row)`, the release as well as the press, since Slint's `ContextMenu` opens on either. Its bubble phase gets the keys no child took: Alt+Left and Alt+Right for history, a bare Left or Right for `horizontal-step` (along the row, then across the columns), Tab and Shift+Tab for couch mode's pane cycle unless `Nav.synthesizing` marks a Tab Rust made for an arrow, and Escape for the chain below.

The bubble phase is what makes the surfaces that own a key win without a modal flag: a dialog cycling its buttons, an open select, an editing field, an entered slider or spin box, a pinned "i" and the rename take Tab or Escape before `nav-scope` sees them.

## Escape

One level out per press, in `escape()`, after the surfaces that take it themselves (a select's list, a dialog, an editing field, an entered slider or spin box, a pinned "i", the rename):

1. A visible help bubble closes, however it opened.
2. The pairing wizard mirrors its footer: Back if enabled, else Cancel if enabled, else nothing (on the done step).
3. A TV config edit cancels (`EditPane.request-cancel()`), asking first when it has changes (`EditPane.dirty`). On a desktop, Escape in an editing config field cancels the edit the same way. Both this level and the one above apply only while the TVs pane shows: the wizard and an edit left open there are out of sight from another pane, and Escape walks the pane shown.
4. A detail goes to its selected list row, from any of its stops (Left is the way back to a row's control).
5. A list goes to the active sidebar item.
6. On the sidebar: history back if the page was reached through a click-through (a profile pencil, a "used by" link; `Nav.click-through-back`, kept by Rust), else couch mode quits the process (`Nav.close()`, `slint::quit_event_loop`), as Bigscreen's apps end on Back. A desktop does nothing.

Going up the hierarchy before back in history is how Bigscreen's apps, Android TV and Kodi behave. One press of Circle shouldn't throw away a config typed on an on-screen keyboard, hence the discard confirm.

## Landings and anchors

Every transition names where focus lands:

| Transition | Focus lands on |
|---|---|
| Dialog closes | Its opener; the selected list row or active sidebar item if the opener is gone (Remove TV) |
| Edit saved or cancelled | The Edit button |
| Rename ended by key | The Rename button that replaces the field |
| Field reset | That row's control |
| Wizard exits | The new TV's row, or the TVs "+" |
| Return or Right on a sidebar item | The pane's selected row, or its first stop |
| Return or Right on a list row | The detail's first stop |
| A click-through (pencil, "used by") | The new pane's detail |
| History back | The stop that triggered the forward move, else that page's column; a page left through the sidebar keeps no stop, since the item focused by then is the destination's and would navigate forward again |
| A detail gone under focus (its profile deleted, its TV removed, a TV before it removed) | Its list, through `has-selection` and selected-uuid change handlers |

Slint can't hold an element reference, so a landing is an **anchor**: every stop's focus-in bumps `Nav.focus-seq`, and a focusable keeps the count as its anchor, with its `accessible-id`. A dialog records anchor, id and column at open and asks for them back on close through `Nav.request-anchor(seq, id, column)`. The focusable holding the count (a living instance, a dialog's opener) or the id (one re-created with its pane, which is what history back needs: the count named an instance the pane switch destroyed) focuses itself and marks the request served, and the shell's 1 ms fallback requests the column when none does. A focusable also takes its anchor on activation, which may open a dialog with no focus change; `focus-gained` being synchronous, a control created and focused in one pass reports too, and its `init` runs the `focus-in` callback the deferred handler would miss. Every stop records its anchor as it takes focus; a request is answered only by a stop that can take focus (enabled and focusable, `Nav.anchor-matches` deciding the match), and a dialog is disabled while closed: a programmatic `focus()` on an item that refuses walks on to the next item that accepts, hidden ones included. The TV detail's ids carry the TV's uuid (the "i" through `FormRow.id-prefix`), since a detail exists for every TV, hidden when not selected, and an anchor found by id must name the one shown. History entries (`nav.rs`'s `NavEntry`) store the anchor, the id, the column and whether the page was a click-through; a return runs a tick after the pane switch (a zero `slint::Timer`) so the new stops can answer.

The models refreshed on every reconcile (the About components, a TV's vendor config and info rows, a profile's "used by" links) are diffed in place, as the TV and profile lists are, so a state burst doesn't destroy the focused Copy, Config or "used by" button.

## Lost focus recovers

Slint clears focus when the focused item is hidden or destroyed, and with nothing focused it runs no key handler at all, only its built-in Tab: a remote's keys would go dead after a pane switch or a refresh that destroyed the focused control. `has-focus` is set only on the focused scope, never on an ancestor, so the shell can't see "nothing has focus" directly. Instead `nav-scope`'s capture handler counts every key it sees, and the key router (the winit hook in production, the test helper in tests) notes the count before a navigation key and checks it a tick after the dispatch (`keys::key_seen`, `keys::recover_if_unhandled`): a key that didn't move it arrived with no focus, and `restore-focus` puts focus back on the last column, where the pane picks the stop.

## Scrolling keeps a margin

Slint's own scroll-into-view brings the focused item alone to the viewport's edge, which cut off a row's label and its group's title as focus walked down a detail. Every stop reports its window rectangle as it takes focus (`Nav.reveal`), and the pane's `HelpScrollView` nudges its viewport so the rectangle keeps `Design.scroll-margin` above and below, enough for a neighbor row or a group's title band. A dialog's buttons float over the pane and opt out (`Focusable.reveal: false`); lists keep Slint's reveal, a row being the whole stop. The alternative, parking the focused item at a fixed fraction of the viewport as Android TV's Leanback and most console menus do, moves more.

## Help from the keyboard

Each "i" is a `Focusable` stop of its row, the last one: Return or Space toggles its pin. Menu toggles the focused row's help without moving focus; a profile field row, which has no "i" of its own, relays the request to its section toolbar's (`section-row`). Help pinned from the keyboard follows the row it was pinned from (`pinned-for-row`, the field row for a relayed Menu): it closes when focus leaves that row, on Escape, or when another pins. The check runs a millisecond after the row changes, because a row walk's probe hop past the row's end is undone before then. An icon-only button shows its tip when focus lingers on it, through the hover path, in both modes. The bubble, the triggers and the dismissal rules are in [contextual-help.md](contextual-help.md).

## Dialogs

`ConfirmDialog` focuses Cancel, the safe choice; `NameInputDialog` focuses its field, editing at once, so couch mode's keyboard rises. Both are `visible: open` and disabled while closed (their scope, buttons and field), cancel on Escape, cycle Tab and Shift+Tab/Backtab among their own enabled controls with explicit `focus()` calls (skipping a disabled confirm), and land per the table above on close; Return in the name field confirms. The two components cover every dialog in the GUI, since Slint's traversal can't be scoped to a subtree. Fields created inside an `if` (the rename, the config edit) focus in their `init`. A rename and the name dialogs open editing; the TV config edit lands on its name field's ring and, in couch mode, waits for Return.

## Couch mode

The layout for a TV across the room (`couch.rs`): a pure function of the preference, the `TISVLAY_COUCH_MODE=auto|on|off` override and the session's signals, where Auto turns it on when `PLASMA_PLATFORM` lists `mediacenter`, the key Bigscreen's own apps pick their TV layouts by. The preference (mode and scale factor) is `gui.toml` in the user's config directory, the GUI's first state of its own, and the file the GUI's D-Bus `config_path` and the About pane's Config button point at. An environment variable rather than a flag, because the GUI is D-Bus-activated with no arguments and a second launch hands off to the running window. The Settings pane shows the scale as a percentage (`ThemedSpinBox` is an integer control) and the file stores a factor. The file is hand-editable: a scale outside the control's bounds (the `Couch` global's, defined once) is clamped and one that isn't a number is the default, and a file that doesn't parse when the window opens is set aside as `gui.toml.edited`, by the instance that keeps the window (a second launch hands off without reading it); a hand edit made while the window runs is overwritten by the next save, as the book says. Saves go through one writer task, in order, the latest value last (the scale spin box commits a value per step of a held arrow, and concurrent atomic writes would unlink each other's temp file), and exit waits for it briefly.

What it changes:

- The window opens maximized, and the UI scales by the preference's factor on top of the compositor's scale: the winit hook multiplies each `ScaleFactorChanged`, and puts the factor on at the window's first `Resized`, since Slint creates the winit window when the event loop starts, after `show()`, and sets the compositor's raw scale past the hook then; a compositor that sends no scale event of its own (X11) gets its scale that way (a session that mapped the window with no `Resized` at all, which none seen does, would leave it at 1× until a Settings change), and the Settings path reads the live compositor scale from the window. `SLINT_SCALE_FACTOR` isn't used: it replaces the compositor's scale, and setting it in-process would mean `set_var` after the runtime's threads start. The window's minimum size is relaxed, since a scaled maximized window can be smaller in logical pixels.
- Focus starts on the active sidebar item at launch, ring showing, so a remote user sees where they are before pressing anything, and on re-activation when nothing has focus (an open dialog or an editing field keeps it).
- Tab and Shift+Tab cycle the panes TVs, Profiles, Settings, About, as console menus do with their shoulder buttons.
- Text fields stop at a ring first and start editing on Return (which brings up the on-screen keyboard); Return or Escape stops editing and keeps focus on the field. Sliders and spin boxes are entered the same way.
- Copy buttons leave the focus order: a clipboard is little use on a couch session.
- Escape on the sidebar quits.

A uniform relative scale covers the literal px values a token-only large mode would miss, and still honors the session's own display scale; a layout with its own couch tokens isn't planned. The default scale comes from Bigscreen's body font over the GUI's, not from a real TV; the preference lets it be tuned from use.

## The winit hook

`on_winit_window_event` in `lib.rs`, on the `unstable-winit-030` surface ([slint-unstable-winit-030.md](../concerns/slint-unstable-winit-030.md)), runs before Slint sees an event:

- `MouseInput` Back and Forward drive the history.
- `ScaleFactorChanged` is multiplied by couch mode's factor and the original prevented; `Resized` puts the factor on once the window exists, idempotently.
- `KeyboardInput` for a key the backend would drop goes through `keys::remap_dropped_key` (`BrowserBack` and `Close` to Escape; the unidentified Xkb keysyms `0x1008ff65`, `XF86MenuKB`, and `0x10081166`, `XF86Info`, to Menu); the hook dispatches the Slint key instead and prevents the original.
- `KeyboardInput` for a navigation key wraps the dispatch in the focus-loss watch (`watch_focus_after`).

The hook can't do the arrow routing itself: running before Slint, it can't tell a key a slider used from one nobody used, which is why the arrows are routed in `nav-scope` and walked through `Nav`'s callbacks.

## Where the code is

- `crates/tisvlay-gui/ui/globals/nav.slint`: the `Nav` global, the whole shell-to-widget surface (focus visibility, the column and its requests, the key count, list steps, anchors, rows, the idle value flag, reveal rectangles, the click-through flag and `close`).
- `crates/tisvlay-gui/ui/widgets.slint`: `Focusable`, `FocusRing`, `ListRow` (the base under the TV, profile and discovered rows: the list column, the inside ring, `clicked` on Return, Space and Right, the list's focus tick and `step`), `LineEditField`, `ThemedSpinBox`, `OptimisticSlider`, `ContextualHelpButton`, `ConfirmDialog`, `NameInputDialog`. A stop that both reports its place and reveals its rectangle calls `Nav.stop-focused-at`.
- `crates/tisvlay-gui/ui/main.slint`: `nav-scope`, the rail (its order defined once in `pane-index` and `pane-at`; the four items are named only in `focus-pane-item` and `focused-pane-index`), the column requests and their fallbacks, `escape()`, `cycle-pane`, `restore-focus`.
- `crates/tisvlay-gui/ui/components/*.slint`: the rows and their ids (the profile editor's three row kinds share `ProfileFieldRow`: the entry, the row id and the Menu relay), the lists' step handlers, the panes' landing handlers.
- `crates/tisvlay-gui-app/src/keys.rs`: the key router (synthesized Tabs, the row walks, the remap, the focus-loss check); `nav.rs`: the history and its anchors; `couch.rs`: couch mode; `lib.rs`: the hook, the scale, launch focus.
- `crates/tisvlay-gui-app/tests/it/navigation.rs`: the suite, one test per rule above; how keys are driven and focus observed is in [docs/testing.md](../testing.md#driving-keys). Not covered end to end: two Escapes walking two click-throughs, since the history is wired with a daemon client; the click-through flag and the anchors are unit-tested in `nav.rs`, and the chain's sidebar level against the flag.

## Lessons

What the VM and the tests taught, kept here so the next change doesn't re-learn it. The Slint-level ones are in [slint-gotchas.md](../concerns/slint-gotchas.md).

- **Tab couldn't reach a text field below the fold.** Slint's traversal skips a stop under a clipping ancestor whose clip rect is empty while scrolled out of view, and `LineEdit` clips its `TextInput`, so `LineEditField` keeps a reach scope outside the clip that takes the Tab and forwards focus programmatically. The focusable base keeps its scope outside any clip.
- **Return-to-select felt wrong; selection follows focus.** First built as Return-to-select after Bigscreen's launcher lists, then changed before the Escape work: a pane switch and a profile selection are cheap and local, which is when ARIA's tabs pattern prescribes automatic activation, and master-detail lists preview the focused row on desktops and TVs alike. The linear Tab order would then have switched panes on every Tab through the rail, so the rail and the lists became regions with one roving stop.
- **Stepping every widget of a row felt clunky; the detail navigates by rows.** The explicit, geometric and structural families were compared in the ideas doc; the tagged Tab order was chosen because the Tab order already matches each row's reading order and Slint offers no other "next".
- **An anchor by count landed on the wrong stop.** Back from a profile landed on Power, not the pencil: the count named an instance the pane switch had destroyed. Anchors now carry the `accessible-id` too, and the request is made a tick after the pane switch.
- **A dialog closed onto the list row instead of its opener.** The opener was created and focused in one pass (a profile detail focusing Duplicate from `init`) and never saw a `changed has-focus`; anchors are taken on activation as well, and the report moved to the synchronous `focus-gained`.
- **Review found the states a remote couldn't leave.** The wizard's later steps answered no column request, so a Re-pair confirm or Retry stranded focus where only Tab could reach; history back after a sidebar switch recorded the destination item as the anchor and navigated forward again; with the daemon down and no TVs, Right from the TVs item handed focus to the disabled "+", and Slint's programmatic focus walked on into a hidden dialog's scope, where every key was dropped as invisible and the recovery sent focus straight back; Escape on another pane cancelled the hidden wizard or edit; a column request served by another pane pulled focus into the next TV list created. Each has a regression test.
- **Change handlers run newest first.** A control losing focus runs its handler after the one gaining it, which left two adjacent value controls live in couch mode.
- **The couch scale compounded on every resize, and took focus with it.** The window the winit hook is handed is Slint's, whose `scale_factor()` is the scale already applied, so a `Resized` arm that read the compositor's scale from it multiplied the factor in again each time (1.5 became 2.25 on the VM) until a Settings change read the real one. Every re-scale re-laid out the pane under focus, and Slint drops a focus item its clip-aware visibility test rejects at the next key, which showed as keys going dead between row walks, recovered to the detail's first stop with the pane scrolled to the top. The compositor's scale comes from `with_winit_window` now, and the walks hold.
- **Deleting under focus left focus dead.** The detail that goes away hands focus to its list through a `has-selection` change handler, so the next key isn't spent on recovery.
- **Couch mode's pane cycle stole Tab from dialogs** while it lived in the capture phase; it moved to the bubble phase, where a dialog cycles its own buttons first. The same placement made a modal flag for Escape unnecessary.
- **The Menu key opened a text field's context menu on its release** after the press was captured; both are captured.
- **A focus ring was clipped by a `Flickable`** (the "used by" links): the box overhangs by the ring's room.
- **The couch-mode scale was judged from Bigscreen's font ratio, not on a TV.** The preference exists so it can be tuned from use.
