## Slint live-preview interpreter panics on a navigation callback

**Priority**: low (dev-tooling only; the shipped/compiled app is unaffected)
**Found by**: GUI dev loop — clicking the TVs-pane "Edit system profile" pencil under `GUI_LIVE=true` (July 2026)
**Status**: open upstream ([slint#6426](https://github.com/slint-ui/slint/issues/6426)); worked around in the dev loop by running compiled + MCP (`GUI_LIVE=false GUI_MCP=true just gui-start`). No code change — users never run the interpreter.

### Reproduced

`GUI_LIVE=true GUI_MCP=true just gui-start` (Slint 1.17.0, KDE Plasma Wayland), then TVs pane → a TV's PROFILES row → click the Edit (pencil) `IconBtn`. The window dies immediately with:

```
thread 'main' panicked at slint-interpreter-1.17.0/eval.rs:2332:14:
accessing deleted parent (issue #6426)
```

With `GUI_LIVE=false` (the compiled UI) the identical click navigates to the Profiles pane as intended — no crash. `GUI_MCP` is orthogonal: compiled + MCP is fine; the crash needs the live-preview interpreter.

### Context

`GUI_LIVE=true` runs the UI through the **Slint interpreter** (`slint-interpreter`, the live-preview hot-reload path) instead of the compiled/generated code. Panes are conditionally instantiated (`ui/main.slint`: `if root.active-pane == Pane.tvs: TvsPane { … }`), so when the Edit pencil's callback (`open-profile-by-uuid`, wired in `src/lib.rs`) flips `active-pane` to `Profiles`, the whole `TvsPane` subtree — including the `IconBtn` that is mid-callback — is deleted; the interpreter then walks to the now-deleted element's parent and panics. The compiled backend generates code that survives the reparent, so only the interpreter is hit.

This is the general shape of [slint#6426](https://github.com/slint-ui/slint/issues/6426) ("accessing deleted parent"): a callback whose source element is destroyed as a side effect of the callback (upstream: a hover-conditional Button that opens a popup; here: a pane-conditional button that navigates away). The "issue #6426" text is upstream's own panic marker (added ~1.15), replacing an older `Option::unwrap()` panic.

### Risk

None for users — the interpreter never ships, and every packaged build (compiled, `GUI_LIVE=false`) handles the navigation fine. The cost is developer friction: the live-preview + MCP loop can't drive any control whose handler navigates or otherwise deletes its own subtree (a pane switch; a dialog-closing action that tears down its opener), which is exactly where MCP-driven inspection of the *result* is most useful.

### Mitigation

- **Run compiled + MCP for navigation testing** — `GUI_LIVE=false GUI_MCP=true just gui-start`. MCP element IDs come from the global `SLINT_EMIT_DEBUG_INFO` (`.cargo/config.toml`), so the element tree, clicks, and screenshots all work against the compiled UI with no interpreter to crash. Reserve `GUI_LIVE=true` for hot-reloading layout/style edits you can observe without triggering a navigation callback.
- **`visible:` instead of `if` (rejected).** The upstream workaround keeps the element in the tree and toggles `visible` rather than conditionally instantiating it; applied to the pane switch that means always-instantiating all four panes and toggling visibility — a real per-launch memory/build cost to placate a dev-only interpreter quirk, not worth contorting the shipped UI for.

### Re-evaluation triggers

- [slint#6426](https://github.com/slint-ui/slint/issues/6426) closes / a Slint release fixes the interpreter's deleted-parent handling → drop this concern; live-preview can then drive navigation directly. Coordinate any bump with the exact-version pin — see [Slint `unstable-winit-030` feature dependency](slint-unstable-winit-030.md).
- The live-preview + MCP loop becomes routine and the compiled-build detour proves too slow → reconsider the `visible:`-toggle restructure, or file a focused upstream repro (a pane-conditional button navigating away) against #6426.
