## Slint compiler/runtime gotchas hit building the GUI

**Priority**: low (all worked around)
**Found by**: contextual-help overlay implementation

Running list of Slint footguns this project has hit and worked around. Each is a candidate upstream bug report; recorded here so the next person adding a Slint component doesn't re-derive the workaround.

### `Timer.restart()` ICEs the compiler inside an inlined component

When a `Timer` lives in a reusable component that gets inlined, `lower_restart_timer` can't find it post-inline and the compiler panics. Drive `running` directly (`running: true` / `false` in the handler) instead of calling `.restart()`.

### `changed <geometry>` reading `absolute-position` panics the layout cache

A `changed anchor` handler — or any handler that reads `absolute-position` *during* layout — trips a reentrant layout-cache panic. Defer the read to a post-layout event: bump a cheap non-geometry tick property from `changed viewport-y` / `changed width` and read `absolute-position` only in that tick's handler. The contextual-help overlay's anchor tracking uses this pattern ([gui/contextual-help.md](../gui/contextual-help.md)).

### Built-in `Tooltip` / `PopupWindow` are unfit for anchored persistent bubbles

- **`Tooltip`** hard-binds its popup to the pointer (jitter), uses `StyledText` so it can't word-wrap, and clamps position but not size (edge crop). Fine for short cursor-following hints only.
- **`PopupWindow`** captures focus on show (hover-steal → flicker), can't read the window size (so no center/flip placement), and its focus-grab with no working dismiss can **lock up the UI** for a click-persistent bubble. Its winit backend renders popups into the parent surface and clamps to the window anyway, so it offers no advantage over an in-window overlay for a small bubble.

Both drove the custom in-window overlay — see [gui/contextual-help.md](../gui/contextual-help.md).

### `TouchArea` can't catch clicks while passing hover through

A Slint `TouchArea` is binary: enabled it eats both clicks and hover; disabled it eats neither. There is no "block clicks but let hover through," so a full-window modal scrim and a live cursor are mutually exclusive — the help overlay forgoes a scrim (intercepting only the bubble rect) and dismisses on focus-out instead, which (because the window's `forward-focus` points at a full-area nav `FocusScope`) still yields a click-anywhere dismiss without any scrim. The same binary nature means a destroyed or disabled `TouchArea` never emits a final `has-hover = false`, so a trigger that goes non-interactive must clear its own hover state explicitly.
