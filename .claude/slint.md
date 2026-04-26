# Slint notes — what I learned while building tisvlay-gui

Self-notes for future me. The upstream Slint skill at
[/home/ruffsl/ws/lgtv/libs/slint/docs/skills/slint/SKILL.md](/home/ruffsl/ws/lgtv/libs/slint/docs/skills/slint/SKILL.md)
covers the basics; this file captures the things I stumbled on that
the skill doesn't say outright.

## MCP server

Slint ships an in-process MCP server in `i-slint-backend-selector`'s
`mcp` feature. It speaks JSON-RPC over Streamable HTTP at
`http://127.0.0.1:<port>/mcp`. There's no separate process — the GUI
binary itself listens.

### Setup gotchas

- **Pin `i-slint-backend-selector` to the exact `slint` version with `=`.**
  It does not follow semver. Mismatch → build fails. Resolved Slint
  version on crates.io is `1.16.1`, so:
  ```toml
  slint = "1.16"
  i-slint-backend-selector = { version = "=1.16.1", features = ["mcp"], optional = true }
  ```

- **`SLINT_EMIT_DEBUG_INFO=1` must be set at *build time*, not just at
  runtime.** The element tree is dead without it — `get_element_tree`
  returns only the root and `query_element_descendants` returns empty
  arrays. Set it on the `cargo build` invocation:
  ```sh
  SLINT_EMIT_DEBUG_INFO=1 cargo build -p tisvlay-gui --features dev-mcp
  SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315 ./target/debug/tisvlay-gui
  ```

- **Disabling Slint's `accessibility` default feature does NOT break
  MCP introspection.** I read the source at
  [internal/backends/testing/mcp_server.rs](/home/ruffsl/ws/lgtv/libs/slint/internal/backends/testing/mcp_server.rs)
  to confirm. `accessibility` is required only for
  `invoke_accessibility_action` and the `matchElementAccessibleRole`
  query filter — neither has been load-bearing. Element trees,
  screenshots, and `click_element` work without it. Drop accessibility
  with confidence if it's causing zbus-tokio panics in `accesskit_unix`.

### Initialize handshake

Three calls in order, each as a separate POST:

```sh
curl http://localhost:9315/mcp -X POST \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
       "params":{"protocolVersion":"2025-06-18",
                 "capabilities":{},
                 "clientInfo":{"name":"claude-code","version":"1"}}}'

curl http://localhost:9315/mcp -X POST \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'

# Then any tool/call.
```

### Read text trees, not screenshots

This is the biggest win and I kept forgetting it. **Before reading
pixel coordinates out of a screenshot, dump the element tree as text.**
Sizes, absolute positions, and parent-child relationships are exact —
no parsing of fuzzy PNG data, no guessing at scale factors.

```sh
HANDLE=$(curl ... query_element_descendants ... | jq -c '.result.content[0].text | fromjson | .elementHandles[0]')
curl ... get_element_tree elementHandle=$HANDLE maxElements=300 \
  | jq -r '.result.content[0].text' \
  | jq '.elements | map({type: .typeNamesAndIds[0].typeName, abs: .absolutePosition, size: .size})'
```

Use screenshots only to verify *visual* qualities (color, alignment,
font weight) that the tree can't express.

### Handle invalidation

Element handles have an `index` AND a `generation` field. Each property
write or model rebuild bumps the generation, invalidating prior
handles. **Chain query → use in a single curl pipeline.** Don't store
a handle in a shell var and reuse later — it goes stale within
milliseconds when the GUI is doing anything.

```sh
# Good — fetch handle, immediately use it:
HANDLE=$(curl ... query_element_descendants ...)
curl ... click_element elementHandle=$HANDLE
```

If a handle goes stale you'll get `Element handle ... refers to element
that was destroyed`. Re-query.

### Query syntax — the gotcha

`matchElementTypeNameOrBase` does NOT mean "match including subclasses";
it matches **either the type name OR a known base type**, and only
works for built-in element types (`Rectangle`, `TouchArea`, `Text`,
etc.). For user-defined components like my `Btn` or `TvListRow`, use
`matchElementTypeName` (exact). The `OrBase` form returned `{}` for
those — confused me for a while.

```json
// User component:
{"matchElementTypeName": "Btn"}

// Built-in:
{"matchElementTypeNameOrBase": "TouchArea"}

// Qualified user component id:
{"matchElementId": "Btn::touch"}
```

### click_element on custom components

A user component must contain a `TouchArea` for clicks to register;
the MCP server doesn't synthesize clicks from raw geometry. Two
patterns that work:

1. Component **inherits Rectangle**; `TouchArea` is a child (TouchArea
   inherits the Rectangle's size). This is what `Btn`, `TvListRow`,
   `ThemedCheckBox` use.
2. Component **wraps content + TouchArea inside a layout**, with
   explicit `width: 100%; height: 100%` on the TouchArea.

What does NOT work: a non-Rectangle component with a Rectangle child
and a sibling TouchArea — the TouchArea's preferred size is 0, click
events miss it. (Fixed `Btn` by switching it to `inherits Rectangle`.)

### Limitations to plan around

- **`set_element_value` only works on Slint inputs** (TextInput,
  Slider, Spinbox, etc.). It can't toggle a `bool` property on
  AppWindow or set a model. For programmatic state changes from MCP,
  the workaround is to fire a callback (e.g., via
  `invoke_accessibility_action` on a button that has the right
  callback), or have the user click for you.

- **Sidebar nav doesn't always click via MCP** in my custom
  `SidebarItem`. It worked in some configurations and not others. I
  haven't fully diagnosed it — possibly ScrollView interactions or
  layout-collapse issues. Workaround: `dispatch_key_event` (Tab to
  focus, Enter), or set the navigation property directly via a Rust
  handler invoked through some other path.

- **Animated transitions can land your screenshot mid-frame.**
  `take_screenshot` is synchronous but the UI may still be
  transitioning. Insert a `sleep 1` after a click before screenshotting
  if state should have settled.

## Slint-side patterns I committed to

### Fluent + unfocused = washed out chrome — build themed widgets

Slint's std-widgets defer entirely to the platform style. On Linux
that's Fluent. **Fluent dims chrome heavily on unfocused windows AND
ignores user-supplied Theme tokens.** MCP screenshots are typically
captured against an unfocused window, so std-widget Buttons /
LineEdits / ComboBoxes / CheckBoxes appear nearly invisible.

Two consequences:
1. You'll be tempted to dismiss the unreadability as "MCP screenshot
   artifact." It's *partly* that, but Fluent also fights design
   fidelity even when focused.
2. The right answer for any input that the design pins down is a
   custom themed widget — not a wrapper around the std-widget. See
   [crates/tisvlay-gui/ui/widgets.slint](../crates/tisvlay-gui/ui/widgets.slint):
   `Btn`, `ThemedTextInput`, `ThemedCheckBox`, `ThemedSelect`. All of
   them drive every color from `Theme.*` directly, no platform style
   in the loop.

`ThemedSelect` was the hardest — std `ComboBox`'s dropdown is
non-trivial to replicate. Used Slint's native `PopupWindow` for the
dropdown list. Works well enough; the popup uses platform styling
briefly but that's tolerable.

### Layout collapse pitfalls

- **Slint Rectangles don't lay out children.** A layout
  (HorizontalBox, VerticalBox, etc.) inside a non-layout Rectangle
  takes its *preferred* size, not the Rectangle's. Add explicit
  `width: 100%; height: 100%` on the inner layout, or make the wrapper
  inherit Rectangle so the layout fills.

- **Per-side borders don't exist.** No `border-bottom-width` etc. Use
  a thin Rectangle as a separator at `y: parent.height - 1px`.

- **GroupBox internal layout collapses single VerticalLayout children
  to preferred width.** Set `width: 100%` on the inner VerticalLayout
  so its row HorizontalBoxes don't get squeezed. Otherwise sliders
  inside collapse to ~16 px.

- **`alignment: center` on a Layout is MAIN-axis (horizontal for
  HorizontalLayout).** Don't use it expecting cross-axis (vertical)
  centering — that's the default for cross-axis already.

### Send / Slint thread bridge

`ModelRc<T>` wraps an `Rc`, so it's `!Send`. Don't try to ship one
through `slint::invoke_from_event_loop`. Pattern: build a plain
`Vec<T>` (`Send`) on the tokio side, transfer it through the closure,
wrap into `ModelRc::new(VecModel::from(rows))` *inside* the
event-loop closure.

For struct fields read from a `ModelRc` row: don't bind them through
deeply nested `<=>` chains. Slint's struct-from-model bindings have
been unreliable across step transitions in my testing. Publish the
fields you need as separate properties on the AppWindow and set them
explicitly from Rust on selection / data change. Tedious but
predictable.

### Properties + callbacks chain

When wiring a property through three levels (e.g. AppWindow → TvsPane
→ PairingWizard), forgetting to forward at any layer silently turns
the inner property into a default-valued in property. The compiler
won't complain. Symptom: data set in Rust shows as "—" / empty in the
inner component. Trace the chain in the slint sources end-to-end when
this happens — `grep -nE 'wizard-selected-vendor' ui/` is enough to
spot the missing layer.

### `dev-mcp` feature gating

Keep the MCP server gated behind a Cargo feature. Production builds
should ship without it. My pattern:

```toml
[features]
dev-mcp = ["dep:i-slint-backend-selector"]
```

Then build with `--features dev-mcp` for development. Remember: the
feature pulls the dependency in; the env vars (`SLINT_EMIT_DEBUG_INFO`,
`SLINT_MCP_PORT`) actually activate it at runtime.

## Operational rituals

The user-driven workflow that worked for me:

1. **I rebuild + restart the GUI.** User does not. They only click in
   the live UI when I ask, since they aren't running my Bash for me.
2. **After a code change, kill + relaunch via Bash:** `pkill -x
   tisvlay-gui; sleep 1; SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315
   ./target/debug/tisvlay-gui &> /tmp/gui-mcp.log &`
3. **I screenshot via MCP, then read the PNG with the Read tool.**
   Visual feedback loop that doesn't require the user.
4. **For interactive verification (real network, real TV), the user
   drives.** I tell them what to click in plain English; they tell me
   the result. I check journald (`journalctl SYSLOG_IDENTIFIER=tisvlay-gui
   --since "2 minutes ago" --no-pager`) for confirmation when callbacks
   should have fired.

## When MCP is the wrong tool

- **Multi-step UI flows.** Click navigation through nested custom
  components has been flaky. Drive interactive flows by asking the
  user; verify via screenshots + journal logs.
- **Live reload.** `slint-viewer --auto-reload <file>.slint` is the
  fast iteration path for pure layout/style work — but it doesn't run
  Rust callbacks, so anything tied to backend state has to be done in
  the full app.
- **Property inspection.** `get_element_properties` returns geometry
  + opacity + types only. It does NOT expose user-defined Slint
  property values. Add an `info!` log on the Rust side to introspect
  state.
