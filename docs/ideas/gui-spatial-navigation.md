# Row navigation in the detail panes

Research behind the GUI's row navigation, written 2026-11-07 before the code. What shipped is in [docs/gui/remote-navigation.md](../gui/remote-navigation.md): a tagged Tab order, walked from Rust, with rows stopping at the detail's ends. Git holds the design as it was reviewed. Kept here: how TV and game menus generalize directional navigation, and the ideas not built.

## The problem

The GUI's sidebar and lists were regions (Up and Down step inside them, Right or Return enters the next column) while the detail was one linear Tab order, so every stop in a row was one Down away from the next: a profile editor row is a select plus up to three icons, and stepping through ten rows was thirty presses. A remote user expects Up and Down to jump between rows and Left and Right to move along a row, which is what every TV and console form does.

## How others generalize it

Three families, all in use today.

**Explicit graphs.** Every control names its neighbor per direction. Kodi skins give each control `<onup>`, `<ondown>`, `<onleft>` and `<onright>` ids, and a list's tags say where focus goes off the list's ends, not how it moves inside. Unity's `Navigation.Mode.Explicit` is the same per-selectable table; Android checks `nextFocusUp/Down/Left/Right` ids before any geometry; LRUD has `registerOverride(source, target, direction)`. Deterministic, authored by hand, and brittle when a row gains or loses a widget, which ours do (a reset icon exists only while a field is set locally).

**Geometric.** The next focus is the nearest focusable in the pressed direction. Android's `FocusFinder` keeps candidates in the half-plane beyond the source's edge, prefers those inside the "beam" the source projects along the major axis, and otherwise orders by `13·major² + minor²`, a weighting its source calls finely tuned; Android's keyboard clusters add a container level above it. The CSS spatial-navigation draft searches a container first (the viewport, scroll containers, anything with `spatial-navigation-contain: contain`), prefers "insiders" that overlap the origin, otherwise minimizes `distance = euclidean + displacement − alignment − sqrt(overlap)` with weights 30 (horizontal) or 2 (vertical) for displacement and 5 for alignment, offers a `grid` function that prefers aligned candidates, and bubbles to the ancestor container through `navnotarget` when nothing qualifies. Unity's `Automatic` mode is a heuristic of the same kind. No authoring, but it needs absolute geometry for every stop, which Slint doesn't expose to the language, and it misbehaves on ragged forms: a wide slider wins over the small icon the user meant.

**Structural.** A tree of containers, each with an orientation; an arrow moves between the siblings of the active container along that orientation, and a direction the container can't use bubbles to its parent. BBC's LRUD is the canonical TV library: nodes with `orientation: "vertical" | "horizontal"`, siblings ordered by `index`, `isWrapping` for lists that wrap, `isIndexAlign` to land on the same index when moving between parallel containers (lists behaving as a grid), `isStopPropagate` for a focusable parent that takes focus itself, and overrides for the exceptions. Norigin's library for React TV apps is the same idea with a geometric fallback inside containers. Nothing is authored per control: a row is a horizontal container and the form a vertical one, and the tree is the layout already written.

**Value controls on TV platforms.** Leanback's `SeekBarPreference` adjusts on D-pad Left and Right when `adjustable`, with `seekBarIncrement` per press, and Leanback's preference rows carry one control each, secondary actions living in sub-screens. Apple's guidelines keep focus on content elements and off controls like sliders and toggles. Neither platform has a per-row icon strip to reach past a slider, which is the case our rows have; no citable description of the PlayStation or Switch slider gesture turned up, so this design doesn't lean on one.

**What fits here.** The structural family: it needs no geometry, it's declarative, and it's what the GUI already had for the rail and the lists (regions with explicit stepping that stop at their ends). The detail is already a vertical stack of cards of rows, each row a horizontal line of stops in reading order; the tree exists, it only has to be told to the navigation.

## Still deferred

The ideas the PR didn't build: an inline row editor for profile rows (one Return opens a row's full editor instead of its control), and a couch-mode hint strip showing the keys the focused stop takes (Return, Left/Right, Menu), as console menus' button legends do. True 2D navigation (the geometric family above) stays unbuilt: the tagged Tab order covers every pane the GUI has.

## Sources

- BBC LRUD, [usage](https://github.com/bbc/lrud/blob/master/docs/usage.md): orientation, index, `isWrapping`, `isIndexAlign`, `isStopPropagate`, overrides.
- Android `FocusFinder` ([source](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/core/java/android/view/FocusFinder.java)): `isCandidate`, `beamBeats`, `getWeightedDistanceFor`, `findNextUserSpecifiedFocus`; [TV navigation guide](https://developer.android.com/training/tv/start/navigation.html): `nextFocus*` and the nearest-in-direction default.
- [CSS Spatial Navigation Level 1](https://www.w3.org/TR/css-nav-1/): containers, insiders, the distance formula, `spatial-navigation-function: grid`, `navnotarget`.
- [Kodi default control tags](https://kodi.wiki/view/Default_control_tags): `onup`, `ondown`, `onleft`, `onright`, `onback`.
- [Unity `Navigation.Mode`](https://docs.unity3d.com/2022.2/Documentation/Manual/script-SelectableNavigation.html): Automatic, Explicit, Horizontal, Vertical.
- [Norigin Spatial Navigation](https://github.com/NoriginMedia/Norigin-Spatial-Navigation): focusable containers with an automatic spatial algorithm.
- [AndroidX `SeekBarPreference`](https://developer.android.com/reference/androidx/preference/SeekBarPreference): `adjustable`, `seekBarIncrement`; [Android settings on other form factors](https://developer.android.com/guide/topics/ui/settings/handle-other-form-factors): Leanback preferences for TV.
- [Apple HIG, focus and selection](https://developer.apple.com/design/human-interface-guidelines/focus-and-selection): directional focus, focus on content rather than controls.
