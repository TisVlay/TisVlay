## Slint compiler/runtime gotchas hit building the GUI

**Priority**: low (all worked around)
**Found by**: contextual-help overlay implementation

Running list of Slint footguns this project has hit and worked around. Each is a candidate upstream bug report; recorded here so the next person adding a Slint component doesn't re-derive the workaround.

### `Timer.restart()` ICEs the compiler inside an inlined component

When a `Timer` lives in a reusable component that gets inlined, `lower_restart_timer` can't find it post-inline and the compiler panics. Drive `running` directly (`running: true` / `false` in the handler) instead of calling `.restart()`. A minimal component with a timer compiles on 1.18.1 and 1.19 alike, so the shape that crashed isn't pinned down; the workaround costs nothing and stays.

### `changed <geometry>` reading `absolute-position` panics the layout cache

A `changed anchor` handler — or any handler that reads `absolute-position` *during* layout — trips a reentrant layout-cache panic. Defer the read to a post-layout event: bump a cheap non-geometry tick property from `changed viewport-y` / `changed width` and read `absolute-position` only in that tick's handler. The contextual-help overlay's anchor tracking uses this pattern ([gui/contextual-help.md](../gui/contextual-help.md)). A probe reading `absolute-position` from a `changed width` handler didn't panic on 1.19; the deferred read is the robust form regardless.

### Built-in `Tooltip` / `PopupWindow` are unfit for anchored persistent bubbles

- **`Tooltip`** hard-binds its popup to the pointer (jitter), uses `StyledText` so it can't word-wrap, and clamps position but not size (edge crop). Fine for short cursor-following hints only.
- **`PopupWindow`** captures focus on show (hover-steal → flicker), can't read the window size (so no center/flip placement), and its focus-grab with no working dismiss can **lock up the UI** for a click-persistent bubble. Its winit backend renders popups into the parent surface and clamps to the window anyway, so it offers no advantage over an in-window overlay for a small bubble.

Both drove the custom in-window overlay — see [gui/contextual-help.md](../gui/contextual-help.md).

### `TouchArea` can't catch clicks while passing hover through

A Slint `TouchArea` is binary: enabled it eats both clicks and hover; disabled it eats neither. There is no "block clicks but let hover through," so a full-window modal scrim and a live cursor are mutually exclusive — the help overlay forgoes a scrim (intercepting only the bubble rect) and dismisses when focus leaves the "i"'s row instead, which (because the window's `forward-focus` points at a full-area nav `FocusScope` with `focus-on-click`) still yields a click-anywhere dismiss without any scrim. The same binary nature means a destroyed or disabled `TouchArea` never emits a final `has-hover = false`, so a trigger that goes non-interactive must clear its own hover state explicitly.

The rest were hit making the GUI navigable from a remote ([gui/remote-navigation.md](../gui/remote-navigation.md)), on Slint 1.18.1.

### The winit backend drops keys it can't name

A key with neither a Slint name nor text never reaches a handler. A gamepad's Square under Plasma Bigscreen arrives as the `XF86MenuKB` keysym (winit: `Key::Unidentified(NativeKey::Xkb(0x1008ff65))`) and its Share and L2 as `XF86Back` (winit: `NamedKey::BrowserBack`, which Slint has no name for). The `on_winit_window_event` hook sees them first: `keys::remap_dropped_key` maps them to Menu and Escape, the hook dispatches that key through `Window::dispatch_event` and prevents the original.

### No focus, no key handlers

When the focused item is hidden or destroyed, Slint clears focus, and with nothing focused it runs no key handler at all, not even a window-filling `FocusScope`'s capture phase; only the built-in Tab still works. `has-focus` is set only on the focused scope, never on an ancestor, so the state can't be observed from the shell either. The GUI counts the keys its capture handler sees and compares the count around each dispatch from the winit hook; a key that didn't move it had no focus, and focus is restored to its last column.

### `changed has-focus` is deferred; `focus-gained` is synchronous

A `changed` handler runs after the pass that changed the property; `FocusScope.focus-gained(reason)` runs synchronously inside the focus change. Anything the same pass must read (the GUI's column and row reporting, read by a Rust loop between synthesized Tabs) goes in `focus-gained`. A component created and focused in the same pass (a detail focusing its first stop from `init`) gets its `changed has-focus` after `init` like any other change; up to Slint 1.18 it never saw one (slint-ui/slint#14894).

### Change handlers in one pass run newest first

The pending `changed` handlers are pushed at the front of a list and drained from the front (`change_tracker.rs`), so the handler of the property changed last runs first: a control gaining focus runs its handler before the one losing it. Don't let a losing handler clear shared state the gaining one set. A shell handler also can't tell in the same pass whether a widget's handler has answered a request, so the GUI's request-and-serve pattern pairs a tick with a served marker: `Nav.request-anchor` and `request-list-step` check it from a 1 ms `Timer`, and `request-column`'s marker is read by a list created in the same pass as the request.

### A programmatic `focus()` on an item that refuses walks on

`focus()` compiles to `set_focus_item(.., Programmatic)`, whose `move_focus` takes focus off the current item and walks `next_focus_item` until one accepts, the way Tab would from there. So `focus()` on a disabled control isn't a no-op: the current item loses focus, and it lands on the next stop in tree order or nowhere. Guard a programmatic `focus()` on a target that may be disabled. Up to Slint 1.18 the walk ignored visibility as well and could land in a hidden dialog's `FocusScope`, where every key was dropped (slint-ui/slint#14893); hidden items are skipped since 1.19, so what's closed needs no `enabled: open`.

### `row` is a reserved property name

`row` (and `col`, `rowspan`, `colspan`) are the GridLayout placement properties and can't be declared on a component; the GUI's row id is `nav-row`.

### Scroll-into-view has no margin

The focused item is brought into view edge to edge, through every enclosing scroll area, with no margin for its label or its neighbors. The GUI reports the focused stop's window rectangle on focus-in and nudges the `ScrollView`'s `content-y` to keep a margin around it.

### `TextInput` focus rules

`TextInput::focus_event` refuses focus while `enabled: false` (a programmatic `focus()` then walks on, see above), and enables the input method only on a focus-in that finds it not `read-only`, so a field that flips `read-only` under focus has to clear and retake focus for the keyboard to rise. A std `LineEdit` leaves the Tab order only through `enabled: false`, which dims it; a read-only one stays a stop. The GUI's text field is a bare `TextInput` in its own control box for that reason, with its ring's scope as the stop.
