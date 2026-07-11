# Remote session awareness — profile-pane storyboards

Feature in planning: tisvlay learns "a remote desktop session is active" (Sunshine, RustDesk, krdp/RDP, hooks) as a per-user fact, and profiles decide behavior — configurable actions on remote connect/disconnect, plus per-event overrides while remote is active, so remote unlocks and virtual-input idle-resumes stop waking the physical TV. This doc currently holds the GUI storyboards for the one **pending decision: the fact model**. Once decided, this file becomes the full plan (per [README](README.md)).

## Common to all three models

Every model adds the same base surface to the profile editor (mockups below show the **user** profile; **system** profiles get the same treatment for their wake-ish rows — On DPMS on, On wake, On DPMS off):

- Two new event rows in the FIELDS card, matching the existing per-event dropdown idiom: `On remote connect`, `On remote disconnect` (actions: `none / screen_off / screen_on / power_off / power_on`, restore-input glyph on wake actions, inherit/reset gutter behavior — all for free from the field-table machinery).
- Two delay spinboxes: `Remote connect delay` (default 10 s — the action debounces so stream negotiation doesn't thrash the TV) and `Remote disconnect delay` (default 1 s). Arming of the overrides below is immediate-on/delayed-off and does NOT wait on the connect delay — otherwise the DPMS wake path stays open during negotiation.
- A new **WHILE REMOTE ACTIVE** card stacked below FIELDS (uppercase mini-title convention): per-event override rows remapping event→action while the fact is armed. Unset row = inherit the normal action; `none` = suppress. Same row anatomy as FIELDS (gutter accent, reset glyph).

The models differ only in **what arms the fact** — and that's exactly what shows in the pane.

## Model A — one fact, source-kind policy (recommended)

One wire key publishing all live detection sources tagged by kind (`manual` / `sunshine` / `logind` / `portal:<exe>` / `screencast`). Attributed kinds always arm; one profile Switch decides whether the unattributed "screen is being captured" signal (PipeWire node, no app identity) also arms. Flipping it on covers unknown portal apps, attribution misses (e.g. session-service restart mid-remote-session), and doubles as opt-in capture-suppression (OBS, tab-share).

```
USER PROFILE                                    [⧉] [↻]
┌ FIELDS ────────────────────────────────────────────┐
│ On session lock         [ screen_off       ▾ ]     │
│ On session unlock       [ screen_on        ▾ ] ⟳   │
│ On lock wake            [ screen_on        ▾ ] ⟳   │
│ On idle timeout         [ screen_off       ▾ ]     │
│ Idle timeout (locked)   [   60 ▴▾ ] seconds        │
│ Idle timeout (unlocked) [  300 ▴▾ ] seconds        │
│ On idle resume          [ screen_on        ▾ ] ⟳   │
│ On remote connect       [ none             ▾ ]  NEW│
│ On remote disconnect    [ none             ▾ ]  NEW│
│ Remote connect delay    [   10 ▴▾ ] seconds     NEW│
│ Remote disconnect delay [    1 ▴▾ ] seconds     NEW│
└────────────────────────────────────────────────────┘

WHILE REMOTE ACTIVE  ⓘ                            NEW
┌────────────────────────────────────────────────────┐
│ Screen capture arms       ⭘ off   (Switch)         │
│   · remote apps / hooks / logind always arm        │
│   · on: any screencast (OBS, tab-share) also arms  │
│ On session unlock       [ none             ▾ ]     │
│ On lock wake            [ none             ▾ ]     │
│ On idle resume          [ none             ▾ ]     │
│   unset override row = behave as normal            │
└────────────────────────────────────────────────────┘

1 fact key · 1 override block · +1 policy switch
```

## Model B — attributed only

Same pane minus the Switch: the generic screencast signal only corroborates attributed sessions (active vs parked) and never arms by itself. Cleanest pane and simplest policy — but an unknown portal streamer gets no protection, there is no capture-suppression option at all, and an attribution miss (restart mid-session, eavesdrop-disabled distro) leaves you unarmed with no fallback knob.

```
USER PROFILE                                    [⧉] [↻]
┌ FIELDS ────────────────────────────────────────────┐
│ On session lock         [ screen_off       ▾ ]     │
│ On session unlock       [ screen_on        ▾ ] ⟳   │
│ On lock wake            [ screen_on        ▾ ] ⟳   │
│ On idle timeout         [ screen_off       ▾ ]     │
│ Idle timeout (locked)   [   60 ▴▾ ] seconds        │
│ Idle timeout (unlocked) [  300 ▴▾ ] seconds        │
│ On idle resume          [ screen_on        ▾ ] ⟳   │
│ On remote connect       [ none             ▾ ]  NEW│
│ On remote disconnect    [ none             ▾ ]  NEW│
│ Remote connect delay    [   10 ▴▾ ] seconds     NEW│
│ Remote disconnect delay [    1 ▴▾ ] seconds     NEW│
└────────────────────────────────────────────────────┘

WHILE REMOTE ACTIVE  ⓘ                            NEW
┌────────────────────────────────────────────────────┐
│ On session unlock       [ none             ▾ ]     │
│ On lock wake            [ none             ▾ ]     │
│ On idle resume          [ none             ▾ ]     │
│   unset override row = behave as normal            │
└────────────────────────────────────────────────────┘

sources: hooks · sunshine · logind · attributed portal
generic screencast = corroboration only, never arms
```

## Model C — two separate facts

`remote_session` (attributed) and `screen_captured` (generic) as independent wire keys, each with its own event pair and its own override card. Most honest modeling and the only one giving capture its own distinct actions (e.g. different behavior while recording vs while remoted) — at the cost of four event rows, two override cards, two keys through wire/world/SDK/tray, and a precedence rule for when both facts are armed at once.

```
USER PROFILE                                    [⧉] [↻]
┌ FIELDS ────────────────────────────────────────────┐
│ …existing 7 rows…                                  │
│ On remote connect       [ none             ▾ ]  NEW│
│ On remote disconnect    [ none             ▾ ]  NEW│
│ On capture start        [ none             ▾ ]  NEW│
│ On capture end          [ none             ▾ ]  NEW│
│ Remote connect delay    [   10 ▴▾ ] seconds     NEW│
│ Remote disconnect delay [    1 ▴▾ ] seconds     NEW│
└────────────────────────────────────────────────────┘

WHILE REMOTE ACTIVE  ⓘ                            NEW
┌────────────────────────────────────────────────────┐
│ On session unlock       [ none             ▾ ]     │
│ On lock wake            [ none             ▾ ]     │
│ On idle resume          [ none             ▾ ]     │
└────────────────────────────────────────────────────┘

WHILE SCREEN CAPTURED  ⓘ                          NEW
┌────────────────────────────────────────────────────┐
│ On session unlock       [ none             ▾ ]     │
│ On lock wake            [ none             ▾ ]     │
│ On idle resume          [ none             ▾ ]     │
└────────────────────────────────────────────────────┘

2 fact keys · 2 event pairs · 2 override blocks
+ precedence rule when both facts are active
```

## Recommendation

**Model A.** It keeps the fact/policy split pure (the fact publishes raw sources; the profile computes armed), covers B's semantics at switch-off and most of C's at switch-on, adds exactly one control to the pane, and keeps the wire/SDK/tray surface single-key. C's extra expressiveness (distinct capture actions) has no motivating use case yet; if one appears, A's source-tagged payload migrates forward without a wire break.

## Decisions locked so far (2026-07-11)

- **Scope**: system + session — the fact is detected/published by the session service, mirrored to the system socket (existing `event/session/*` tap pattern); the world gates `dispatch_system_events` (DPMS-on is the main TV-wake path), the session gates its own actions.
- **Policy shape**: while-remote override block + first-class remote connect/disconnect events (above).
- **v1 detectors**: manual/hook RPC verb (Sunshine `prep-cmd` integration documented) + PipeWire screencast watcher + D-Bus portal monitor (`BecomeMonitor`, attribution) + Sunshine `/serverinfo` poller + logind `Remote=yes` watcher. InputCapture-only portal sessions (lan-mouse) are filtered unconditionally.
- **PipeWire crate**: `pipewire-native` git-pinned to a current upstream rev (crates.io 0.1.4 has a process-killing decode abort; fix unreleased), with a single documented `allow-git` exception in `packaging/licensing/deny.toml` and a revert-on-0.1.5+ note.
- **Debounce**: arming immediate-on/delayed-off; connect/disconnect *actions* debounced (10 s / 1 s defaults, profile knobs).
