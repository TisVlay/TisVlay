# Row navigation in the detail panes

Research and design for the TV-navigation train ([docs/plans/tv-navigation.md](../plans/tv-navigation.md)), written 2026-11-07 for review before any code. The train's decision 3 left "true 2D navigation" to this doc; the decision now is to build the row model in that PR, before its step 8, because the Escape chain and the ringed help buttons both depend on the row vocabulary.

## The problem

The sidebar and the lists are regions since the train's selection-follows-focus commit: Up and Down step inside them and Right or Return enters the next column. The detail column is still one linear Tab order, so every stop in a row is one Down away from the next: a profile editor row is a select plus up to three icons, and stepping through ten rows is thirty presses, with the ring hopping between a control and its icons. A remote user expects Up and Down to jump between rows and Left and Right to move along a row, which is what every TV and console form does.

## How others generalize it

Three families, all in use today.

**Explicit graphs.** Every control names its neighbor per direction. Kodi skins give each control `<onup>`, `<ondown>`, `<onleft>` and `<onright>` ids, and a list's tags say where focus goes off the list's ends, not how it moves inside. Unity's `Navigation.Mode.Explicit` is the same per-selectable table; Android checks `nextFocusUp/Down/Left/Right` ids before any geometry; LRUD has `registerOverride(source, target, direction)`. Deterministic, authored by hand, and brittle when a row gains or loses a widget, which ours do (a reset icon exists only while a field is set locally).

**Geometric.** The next focus is the nearest focusable in the pressed direction. Android's `FocusFinder` keeps candidates in the half-plane beyond the source's edge, prefers those inside the "beam" the source projects along the major axis, and otherwise orders by `13·major² + minor²`, a weighting its source calls finely tuned; Android's keyboard clusters add a container level above it. The CSS spatial-navigation draft searches a container first (the viewport, scroll containers, anything with `spatial-navigation-contain: contain`), prefers "insiders" that overlap the origin, otherwise minimizes `distance = euclidean + displacement − alignment − sqrt(overlap)` with weights 30 (horizontal) or 2 (vertical) for displacement and 5 for alignment, offers a `grid` function that prefers aligned candidates, and bubbles to the ancestor container through `navnotarget` when nothing qualifies. Unity's `Automatic` mode is a heuristic of the same kind. No authoring, but it needs absolute geometry for every stop, which Slint doesn't expose to the language, and it misbehaves on ragged forms: a wide slider wins over the small icon the user meant.

**Structural.** A tree of containers, each with an orientation; an arrow moves between the siblings of the active container along that orientation, and a direction the container can't use bubbles to its parent. BBC's LRUD is the canonical TV library: nodes with `orientation: "vertical" | "horizontal"`, siblings ordered by `index`, `isWrapping` for lists that wrap, `isIndexAlign` to land on the same index when moving between parallel containers (lists behaving as a grid), `isStopPropagate` for a focusable parent that takes focus itself, and overrides for the exceptions. Norigin's library for React TV apps is the same idea with a geometric fallback inside containers. Nothing is authored per control: a row is a horizontal container and the form a vertical one, and the tree is the layout already written.

**Value controls on TV platforms.** Leanback's `SeekBarPreference` adjusts on D-pad Left and Right when `adjustable`, with `seekBarIncrement` per press, and Leanback's preference rows carry one control each, secondary actions living in sub-screens. Apple's guidelines keep focus on content elements and off controls like sliders and toggles. Neither platform has a per-row icon strip to reach past a slider, which is the case our rows have; no citable description of the PlayStation or Switch slider gesture turned up, so this design doesn't lean on one.

**What fits here.** The structural family: it needs no geometry, it's declarative, and it's what the train already built for the rail and the lists (regions with explicit stepping that stop at their ends). The detail is already a vertical stack of cards of rows, each row a horizontal line of stops in reading order; the tree exists, it only has to be told to the navigation.

## The design

The decisions taken on 2026-11-07, in the terms of the train's decisions 3 and 6.

- **A detail is a vertical region of rows; a row is a horizontal region of stops in reading order.** The row's first stop is its primary: the control in a form row, the only stop in a row that has one (a FROM THE TV row's "i", a DANGER ZONE button), the Edit or Cancel button of the CONFIG title band, Rename in the profile header, the "i" of a FIELDS toolbar. Rows without a reachable stop (a couch-mode INFO row, whose copy button isn't focusable there; a row whose only control is disabled) are skipped.
- **Up and Down move to the previous or next row and land on its primary** (not on the same column: the icons are secondary actions, and the control is the predictable landing). The detail stops at its first and last row, as the rail and the lists do; a desktop Tab still wraps.
- **Right walks out from the primary to the row's trailing stops, Left walks back.** Right on the last stop does nothing. Left on the primary isn't the row's to take: it bubbles to the shell's column jump and goes to the list, as today.
- **Every visible stop stays reachable by arrows, Return and Escape alone** (decision 1). The help "i" is the row's last stop and opens on Return (decision 6); Menu stays the shortcut for the focused row's help. Reset, restore and gate are stops too; nothing moves behind a contextual key, since CEC remotes have none.
- **A value control is entered, in couch mode.** Focused but not entered, a slider or a spin box is just the row's primary, so Left and Right walk the row. Return enters it: a slider's adjust mode takes Left and Right for the value and leaves on Return or Escape, with the ring saying so; a spin box's Return is today's editing (digits type, Left and Right step, Return commits, Escape reverts); a select's Return opens its list as today; switches and buttons act on Return. One press more per adjustment, and one gesture everywhere: Return goes into something (a pane, a detail, a control, a dialog), Escape comes out, arrows move between siblings. On a desktop nothing changes: arrows adjust a focused slider or spin box as in every desktop app, and Tab reaches the icons.
- **Escape gains inner levels** for step 8's chain: leave adjust mode; from a trailing stop, back to the row's primary; then the detail's row, the list, the rail.
- **Tab order.** The detail's stops all stay Tab stops; roving stops remain the rail's and the lists' (ARIA roves toolbars and lists, not forms).

### Rows, as inventoried

The detail panes' rows and their stops in reading order, from a read of the components on 2026-11-07. Static rows unless noted.

TV detail: STATUS has Power (`ThemedButton`, not a stop while a cycle is in flight or the TV can't be powered on) and Managed (`ThemedSwitch`, "i"); PROFILES has System and User (`ThemedSelect`, pencil `IconBtn`, "i"); FROM THE TV has three rows of text with an "i" each; PICTURE has Backlight (`OptimisticSlider`, "i") and Screen (`ThemedSwitch`, "i"), each behind an `if`; AUDIO has Volume (slider) and Mute (switch), no "i"; INPUT has Current and Primary (`ThemedSelect`, "i"); CONFIG's title band carries Edit, or Cancel and Save in edit mode, its view card is `LabeledRow`s with a `CopyBtn` each (not focusable in couch mode) and its edit card is the name `LineEditField` then `SchemaForm` rows of one stop each; INFO is `LabeledRow`s; DANGER ZONE has Re-pair and Remove TV. The header has no stops. An `InlineError` above the pane adds a row of action and dismiss buttons while shown.

Profile detail: the header's title row is Rename and Delete (disabled on the default profile), or the rename field, Save and Cancel while renaming; the Used-by row is a line of `LinkText`s in a sideways `Flickable`; each FIELDS toolbar is "i", Duplicate, Reset all (STREAMING and FROM THE TV sections have their own "i"); field rows come from `for` loops over three kinds, `ThemedSelect`, `ThemedSpinBox` or `ThemedSwitch`, then `ProfileRowTrailing`: restore and stream-gate `IconBtn`s for action rows (stops only while enabled), then a reset `IconBtn` only while the field is set locally.

Settings: every row has one stop (the couch-mode select and spin box, four notification switches, the test-notification button), so rows and stops coincide. About: the header is two visual lines, the brand link with the version chip, then Diagnostics and Report issue; component rows have at most a Config button; the license row has the notices button. Wizard: the discovered rows are the list region already; the manual-entry row is a vendor select, the address field and Continue; step 2's form rows have one stop each; the footer row is Cancel, Back, Next.

### Mechanism

Two ways to teach Slint the tree; the first is recommended.

**Tagged Tab order (recommended).** The Tab order already runs through every row's stops in reading order, and Slint's traversal already skips what's disabled, hidden or not focusable and scrolls the landing into view. So rows don't need to step: they tag. A row takes a unique id at creation (a counter on `Nav`) and passes it to its stops; every stop reports it on focus-in next to its column (`Nav.stop-focused(column, row)`). The shell's row step, in Rust where the Tab keys are already synthesized: for Down, dispatch Tab until the reported row changes (now on the next row's first stop, its primary); for Up, Backtab until the row changes, then Backtab while it stays the same and one Tab back, landing on that row's first stop; if the column changes instead (the order wrapped past the detail's end, or left the pane), undo the dispatches, so the detail stops at its ends. Right and Left inside a row: one Tab or Backtab, undone if the row changed; Left on the primary is left unhandled so the shell's column jump takes it. Escape to the primary is the Up walk without the row change. Stops that report no row (a dialog's buttons, a pane that isn't a form) fall back to today's single Tab, which is how the dialogs keep cycling. Cost: a `row` property on `Focusable`, `ThemedSelect`, `OptimisticSlider`, `ThemedSpinBox`, `LineEditField` and `ContextualHelpButton`, a `row` passed down by each row component (about a dozen component types), the Rust loops in `keys.rs`, couch adjust mode on the slider, and the tests. Each row step is a handful of synthesized keys, cheap. About a day.

**Row ordinals with explicit focus.** Each row gets an ordinal (static rows a literal, `for` rows a section base plus index, conditional sections an offset from their model lengths), the detail keeps a focus-row tick the rows observe to focus their primary, each row implements its own horizontal step over its children, and a row with no stops passes the request on in the same direction. Deterministic and all in Slint, but every row type carries stepping code, the ordinal bookkeeping spans conditional sections, and skipping disabled or hidden stops is reimplemented per row. Two to three days.

### Path forward

1. `Nav`: a row counter, `stop-focused(column, row)`, the row of the focused stop; `Focusable` and the five other stop types take a `row` and report it.
2. The row components pass their id down: `FormRow`, `LabeledRow`, the CONFIG title band, the three profile field rows and `ProfileRowTrailing`, the FIELDS toolbars, the profile and About headers, `SchemaFieldRow`, the DANGER rows, the wizard's manual-entry and footer rows.
3. `keys.rs`: row step and stop step as Tab loops with the wrap undo; `nav-scope` sends Up and Down in the detail there, and Left and Right first to the row, then to the column jump.
4. `OptimisticSlider`: couch adjust mode on Return, with its ring state; `ThemedSpinBox` already has the editing state.
5. Tests: Down lands on the next row's control past its icons; Up lands on the previous row's control; Right and Left walk a profile row's icons and stop at its ends; a couch slider walks the row until Return and adjusts after; Escape returns to the primary; a dialog still cycles on Down.
6. VM check with the virtual keyboard: the profile editor and the TV detail by rows.

Step 8 then adds the Escape levels, and step 9's ringed "i" lands as each row's last stop.

### Still deferred

The train's step 10 also meant this doc to hold the ideas it doesn't build: an inline row editor for profile rows (one Return opens a row's full editor instead of its control), and a couch-mode hint strip showing the keys the focused stop takes (Return, Left/Right, Menu), as console menus' button legends do.

## Sources

- BBC LRUD, [usage](https://github.com/bbc/lrud/blob/master/docs/usage.md): orientation, index, `isWrapping`, `isIndexAlign`, `isStopPropagate`, overrides.
- Android `FocusFinder` ([source](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/core/java/android/view/FocusFinder.java)): `isCandidate`, `beamBeats`, `getWeightedDistanceFor`, `findNextUserSpecifiedFocus`; [TV navigation guide](https://developer.android.com/training/tv/start/navigation.html): `nextFocus*` and the nearest-in-direction default.
- [CSS Spatial Navigation Level 1](https://www.w3.org/TR/css-nav-1/): containers, insiders, the distance formula, `spatial-navigation-function: grid`, `navnotarget`.
- [Kodi default control tags](https://kodi.wiki/view/Default_control_tags): `onup`, `ondown`, `onleft`, `onright`, `onback`.
- [Unity `Navigation.Mode`](https://docs.unity3d.com/2022.2/Documentation/Manual/script-SelectableNavigation.html): Automatic, Explicit, Horizontal, Vertical.
- [Norigin Spatial Navigation](https://github.com/NoriginMedia/Norigin-Spatial-Navigation): focusable containers with an automatic spatial algorithm.
- [AndroidX `SeekBarPreference`](https://developer.android.com/reference/androidx/preference/SeekBarPreference): `adjustable`, `seekBarIncrement`; [Android settings on other form factors](https://developer.android.com/guide/topics/ui/settings/handle-other-form-factors): Leanback preferences for TV.
- [Apple HIG, focus and selection](https://developer.apple.com/design/human-interface-guidelines/focus-and-selection): directional focus, focus on content rather than controls.
