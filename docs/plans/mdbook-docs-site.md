# User-facing documentation site: mdBook → Read the Docs

## Status (after merging main, 2026-06-11)

Implementation §1–§3 and §5–§7 are **done** on this branch (CLI lib refactor `a33d250`, scaffold + content + RTD `9cb1dde`, mermaid `3e16c0a`): the site builds, the CLI reference is auto-generated with a CI freshness gate, RTD is connected, and the README is the concise variant linking to the site. §4's v1 pages all have real content (introduction, installation, quick-start, pairing, CLI, configuration, firewall-discovery, troubleshooting, requirements); the four planned stubs (profiles, session-automation, gui-tray, FAQ) remain "work in progress" pages by design.

### Post-merge reconciliation (PRs #74–#80 landed on main since the content was written)

- [x] **`configuration.md` system-profile example — resolved as "no book change."** Investigation inverted the item: the book's `on_shutdown = "power_off"` matches the canon (the code default `SessionAction::PowerOff`, `config.rs`'s doc-comment, and `docs/design.md`'s annotated example). The `screen_off` in `docs/configs/daemon.toml` is the lone outlier, introduced by #74's cherry-pick from the Epson draft branch with no stated rationale — resolved with the maintainer: the sample is reverted to the code default on this branch.
- [x] **Found instead: the user-config filename was wrong.** `configuration.md`'s file table said user profiles live in `~/.config/tisvlay/config.toml`; the code says `~/.config/tisvlay/session.toml` (XDG `config.toml` is the *dev-mode daemon's* path). Fixed the table (now also lists `tray.toml`) and split the reload command into the daemon (`Manager1.ReloadConfig`, system bus) and session (`Session1.ReloadConfig`, session bus) variants.
- [x] **Troubleshooting: mock-backend triage section** — added "Isolating tisvlay from the TV (mock backend)". GUI-wizard route documented as primary (the schema form prefills `pairing = "accept"`); config-file + `ReloadConfig` as the headless alternative. The CLI route is deliberately not documented: `tisvlay pair --vendor mock` sends only `address`, the daemon does no schema-default merge, so the mock's serde default `pairing = "manual"` waits on a test handle that doesn't exist in production — filed as [docs/concerns/cli-pair-vendor-fields.md](../concerns/cli-pair-vendor-fields.md).
- [ ] **`installation.md` Cargo section** — #80 plumbed crates.io publishing, but nothing is published until the first release is finalized. When `0.1.0` lands on crates.io, add `cargo install tisvlay-cli` as a real channel (binary-only, no service files — point at the distro packages for the full stack). Until then from-source stays primary. (Sequenced on the first publish, not doable now.)
- [x] **CLI reference freshness** — `just docs-cli-check` passes post-merge; #74's CLI touch didn't change the clap surface. No book page enumerates power states in prose, so no other staleness from that model change.
- Non-items, checked: #75's "guide" is the developer-facing `docs/backends/implementing-a-vendor.md` (no book collision); #78/#79 (Session pane removal, back/forward nav) don't contradict the `gui-tray` stub; `requirements.md` is unaffected by poll-only backends / the WoL helper.

### Stub fill-in (second handoff, decisions with the user)

Scope: all four stubs, text-first (no screenshots — current-UI captures are a follow-up only the maintainer can do), profiles layered on top of configuration.md's schema reference (nothing moved).

- [x] `guide/profiles.md` — reserved defaults, per-field resolution chain (incl. the admin-baseline layer for user profiles), assignment via CLI/tray/GUI, GUI editor + hand-edit routes, the `automate_when` × `managed` × input gate table, two worked patterns.
- [x] `guide/session-automation.md` — session vs daemon split, user-event table (greeter-wake rationale, idle independence), system events incl. the opt-in DPMS pair and its `screen_on`-escalation property, service health checks, reliability note.
- [x] `guide/gui-tray.md` — panes (TVs / Profiles / Settings / About), pairing wizard, Alt+←/→ + mouse back/forward history nav, tray menu + scroll-wheel behavior, notification categories table. Verified the `.desktop` entry ships (nfpm `contents` + Nix `makeDesktopItem`), so "launch from the application menu" is accurate.
- [x] `reference/faq.md` — eight Q&As (CEC rationale, supported TVs, optional services, OLED safety, scripting via `--json`/D-Bus, config locations, `client_key` sensitivity incl. the verified `0750`/`UMask=0077` permissions, multi-TV), closing pointer to Troubleshooting.

### Post-merge reconciliation (#82–#83 landed on main after the content + stubs were written)

Merged `origin/main` into this branch (single conflict — `docs/plans/README.md`, kept this plan's entry over #83's reset-to-none; `daemon.toml` auto-merged with the new fields; `just check` green, 556 tests). The book content needed the screen-state model folded in:

- [x] **`on_greeter_wake` → `on_lock_wake`** — #83 renamed the user-profile field (it fires at the in-session lock screen, not the logged-out greeter). Updated `configuration.md`, `session-automation.md` (event table + prose), and `requirements.md`.
- [x] **`on_dpms_off` / `on_dpms_on` defaults flipped `none` → `screen_off` / `screen_on`** (#82's "DPMS-mirror defaults"). Rewrote the `session-automation.md` guidance: the mirror is on out of the box, and the lock/idle and DPMS paths now both run by design (overlap is idempotent / reconnect-gated), replacing the old "enable one path or the other."
- [x] **Dropped `on_no_session`** — #83 removed the field; pulled it from the `configuration.md` schema and the `session-automation.md` system-events list.
- [x] **Documented the greeter idle client** (#82, previously absent from the book) — the system-profile idle pair (`idle_timeout_seconds` / `on_idle_timeout` / `on_idle_resume`) + opt-in `services.tisvlay.greeter.enable`, in `session-automation.md` and the `configuration.md` system-profile schema.
- [x] **Latched-screen-state reliability note** — added that the desired screen state survives a reconnect / the login→session handoff (the user-visible upshot of #83's latch + reconcile).
- Non-items, checked: `profiles.md` describes resolution abstractly (no field-name churn); `cli-generated.md` unchanged (#82/#83 don't touch the clap surface — `docs-cli-check` green); the concise root README stays feature-level and accurate.

### Post-merge reconciliation (#84–#85 landed on main)

Merged `origin/main` into this branch (single conflict — `flake.nix`'s dev-shell `packages` list: took main's treefmt-reformatted list and re-inserted `mdbook`/`mdbook-mermaid`; `daemon.toml` auto-merged; `just check` green, 557 tests). #84 is GUI-internal polish (no book surface). #85's "simplify config defaults" needed the discovery/greeter framing folded in:

- [x] **SSDP `bind_port` now defaults to 1901** (#85, was an OS-chosen ephemeral port). Reframed `operations/firewall-discovery.md` from "Recommended: pin the source port" to "Open the pinned source port" — the port is fixed by default, so the user only opens the firewall (added the `services.tisvlay.openFirewall = true` route, since the module's `bindPort` also defaults to 1901). Marked 1901 as the default in the `configuration.md` `[discovery]` comment (`0` = ephemeral).
- [x] **Greeter client auto-enables on NixOS** (#85). `session-automation.md` now notes `services.tisvlay.greeter.enable` defaults on when plasma-login-manager is the active display manager (the only supported greeter today), rather than being a manual opt-in.
- Non-items, checked: the shared-SSDP-socket coalescing (#85) is internal (no user-facing surface); `doCheck = false` / `checks.tests` is a packaging/dev-build change, not user-guide material; #84's profiles-pane number-field UX is GUI-internal (no book page describes the widget); `cli-generated.md` unchanged — #84/#85 don't touch the `tisvlay` clap surface (`docs-cli-check` green).

### Post-merge reconciliation (#91–#102 landed on main)

Merged `origin/main` into this branch. Three conflicts, all mechanical: `Cargo.lock` (took main's dep-bump + `tisvlay-cec` lockfile, then rebuilt to re-add the branch's `clap-markdown` dev-dep); `docs/plans/README.md` (kept this plan's Active-plans entry over #91's placeholder reword); `crates/tisvlay-cli/src/main.rs` (kept the lib-refactor's `use tisvlay_cli::{…}` split and added #94's `Capabilities` import — the capability-gated `print_tv_status` auto-merged in, since the branch only moved clap *type definitions* to `lib.rs`, not the runtime helper). `just check` green (614 tests, clippy `-D warnings`, fmt); `docs-cli-check` + `docs-build` green. #94 (HDMI-CEC) is the substance — the book had three statements that outright contradicted a now-shipped CEC backend, plus new user-facing surface to cover. Coverage decided with the user: **proportionate** — fold CEC into the pages where LG already appears, no new nav chapter.

- [x] **Fixed the three now-false CEC statements.** `introduction.md` ("driven over the network *rather than* HDMI-CEC"), `reference/faq.md` ("doesn't preclude CEC … *as future backends*"), and `reference/requirements.md` ("Network control only — there is *no HDMI-CEC path*") all predated #94. Reframed to two transports: LG WebOS over the network (fullest feature set, no extra hardware, the only option on a CEC-less GPU like Nvidia) and HDMI-CEC (any CEC-capable TV, power + screen-blank, via a USB or amdgpu/i915 DP→HDMI adapter).
- [x] **Folded CEC into the LG-adjacent pages.** `installation.md` gained an "HDMI-CEC backend (optional)" section (adapter hardware, the `/dev/cecN` node, `services.tisvlay.cec.enable`, the LGPL `linux-cec` note); `pairing.md` a credential-free "HDMI-CEC: no pairing needed" section (GUI device-picker + `[tv.cec]` config route — deliberately *not* a `tisvlay pair --vendor cec` CLI command, which hits the same no-schema-default-merge gap as the mock backend, see [cli-pair-vendor-fields.md](../concerns/cli-pair-vendor-fields.md)); `configuration.md` a `[tv.cec]` block (`device` / `physical_address` / `emulate_screen_via_power`, verified against `crates/tisvlay-cec/src/config.rs`) plus a generalized `power_on` action row; `requirements.md` a backend-table row, adapter note, and reworked limitations; the FAQ's "Which TVs" answer and the intro mermaid diagram (a CEC-TV node reached over HDMI-CEC). All new cross-page anchors verified present in the rendered HTML.
- [x] **#92 (GUI contextual help)** — a one-line "Contextual help" note on `gui-tray.md` (the "i" toggletip + hover tips).
- Non-items, checked: #91/#93/#102 (cargo/CI dep bumps) carry no book surface beyond the `Cargo.lock` resolution; #95 (cargo-about `i-slint-renderer-software` coverage) is packaging/licensing, not user-guide material; #101 (session idle-watch survives config reload) is an internal reliability fix with no user-facing statement to correct; `cli-generated.md` unchanged — the merge doesn't touch the `tisvlay` clap surface (`docs-cli-check` green). Still deferred: the `cargo install tisvlay-cli` channel (unpublished) and the GUI/Tray screenshots.

### Post-merge reconciliation (the v0.3.0 rewrite — #106 onward, 2026-07-12)

The branch had forked at #102, **before** the D-Bus → zenoh + bevy_ecs rewrite (v0.3.0, #106) and everything after it (notably #110 remote-session awareness). Merged `origin/main` into the branch (`f5511ec`, 109 commits of divergence): CONFLICTING → MERGEABLE, `just build` + CLI clap self-check + `docs-cli-check` all green. The CLI doc-gen was re-wired — main's rewritten clap tree moved into `crates/tisvlay-cli/src/lib.rs` (`pub`), `main.rs` imports it, `gen-cli-docs` (now at `src/bin/`, not `examples/`) renders it, and `cli-generated.md` was regenerated (old `power on`/`screen` verbs → new `watch`/`set-volume`/`remote-session`). Old D-Bus clap tree, its dev-deps, and old-CLI snapshot tests dropped.

A four-agent read-only audit (getting-started+reference / guide-behavioral / CLI+operations / feature-farming) then mapped the book against current `main`. **Every prose chapter predates the rewrite;** the auto-generated CLI reference is now exact, and the conceptual bones survive almost everywhere, but the *surface* (commands, config paths, IPC model) is wrong throughout and four major features have no coverage.

**Cross-cutting corrections (apply in nearly every chapter):**
1. `tisvlayd` → `tisvlay-daemon` (binary + `.service`).
2. D-Bus IPC → **zenoh over Unix sockets**. Delete every `busctl` / `org.tisvlay.*` / `ReloadConfig` / system-vs-session-bus / `--user` / `tisvlayd --session` / "D-Bus policy". Auth = **`tisvlay` group membership on a 0660 socket** (`docs/architecture.md:94-96`; `packaging/nix/module.nix:48`).
3. CLI grammar: control verbs take a **positional TV UUID**; `pair "<name>" --vendor lg --params '{"ip":…,"mac":…}'`; renames (`repair`, `managed <id> <bool>`, `set-volume`, `assign`, `user-profile assign`); no `--tv`/`-t`, no relative volume, no mute-toggle; `--json` is read-verbs only. Ground truth: `crates/tisvlay-cli/src/lib.rs`.
4. Paths/versions: `config.toml` → `daemon.toml` (`store.rs:79`); `0.1.0` → `0.3.0`.
5. Observability: `busctl monitor` → `tisvlay watch`; `GetManagedObjects` → `tisvlay list`.

**Per-chapter plan (existing 13):**

| Chapter | Verdict | Headline actions |
|---|---|---|
| introduction | rewrite (arch half) | daemon name; drop D-Bus + fix mermaid; add greeter, remote-session, reliability to Features |
| getting-started/installation | heavy-edit | `tisvlay-daemon` unit; add mandatory `services.tisvlay.users` grant; `cec`/`greeter`/`openFirewall` opts; socket-group auth; drop "D-Bus policy"; `0.3.0` |
| getting-started/quick-start | rewrite | every command literal (positional id, `set-volume`, `mute <bool>`); `tisvlay` group prereq |
| getting-started/pairing | rewrite | `pair <name> --params`; MAC in JSON; `repair`; `schema`; MAC auto-fill & CEC device probe |
| guide/cli | accurate + gap | generated ref current; optional prose; fix `cli.md:2` comment path (`examples/` → `src/bin/`) |
| guide/configuration | heavy-edit | drop `ReloadConfig`; `daemon.toml`; EventAction table grammar; `while_remote`/`remote_screencast_arms`; locked/unlocked idle; `restore_input`; `seed.toml` |
| guide/profiles | heavy-edit | fix assignment verbs; add `while_remote` gate layer; keep resolution chains + `automate_when` table |
| guide/session-automation | heavy-edit | daemon name; remote-session suppression; per-lock-state idle + drift; `restore_input`; expand reliability; keep DPMS/lifecycle/greeter |
| guide/gui-tray | heavy-edit | drop D-Bus; REMOTE SESSION card + `while_remote`/`restore` toggles + tray "Remote session active" indicator |
| operations/firewall-discovery | light-edit | pair syntax ×2; `--ssdp-port` (no `[discovery] bind_port` key); broaden to streaming discovery + MAC enrichment |
| operations/troubleshooting | heavy-edit | rewrite `busctl`/`ReloadConfig` sections → `list`/`watch`/`add`; ~11 command fixes; `automate_when` is policy |
| reference/requirements | heavy-edit | drop "D-Bus [for the daemon]"; socket-group auth; IPv6 note; keep hardware/CEC/WoL/DPMS |
| reference/faq | heavy-edit | rewrite `busctl` scripting answer → key space/`--json`/`watch`; `daemon.toml`; drop `-t` |

**New chapters to add** (SUMMARY delta): **Remote-Session Awareness** (User Guide — flagship, from `docs/remote/detection.md`, + Sunshine `global_prep_cmd` recipe); **Networking & Reliability** (Operations — routable dials, carrier/route watchers, verify-retry + latch); **HDMI-CEC** (User Guide, or fold into a Vendors chapter); **Observability** or an expanded CLI Reference (`watch`, `status`/`list`, liveliness, `vendors`/`schema`). Broaden **Firewall & Discovery** to cover streaming discovery + manual-entry MAC enrichment. Nothing is deleted wholesale — every chapter maps to a still-real feature.

**Writing progress** (decided with the user: I draft, surface fixes first, then new chapters):
- [x] Batch A — introduction, getting-started/{installation, quick-start, pairing} rewritten against `lib.rs`/`module.nix` (`9aba954`).
- [x] Batch B — guide/{configuration, profiles, session-automation, gui-tray}, verified vs `config.rs` + the real `daemon.toml`/`seed.toml` (`8da8cac`).
- [x] Batch C — operations/{firewall-discovery, troubleshooting}, reference/{requirements, faq}, `cli.md` intro + comment-path fix (`11fcc99`).
- [x] Batch D — new chapters `guide/remote-sessions.md` + `operations/networking-reliability.md` + `SUMMARY.md`. **HDMI-CEC** stays folded into the existing pages (installation/pairing/configuration/requirements/faq) per the pre-rewrite maintainer decision — no separate nav chapter; **Observability** folded into the CLI Reference intro (`watch`/`status`/`list`), not a standalone page.

**Remaining polish** (non-blocking): `cli.md`'s own `# CLI Reference` H1 sits above the generated file's `# Command-Line Help for tisvlay` H1 (a double heading) — the clean fix is wiring `gen-cli-docs` to `help_markdown_custom` with the generated title suppressed (see Gotchas). A field review of the rewritten commands/config against a live v0.3.0 stack, and the still-deferred GUI/Tray screenshots, are the maintainer's to do.

## Context

tisvlay has no user-facing docs site today — `docs/*.md` is ~95% developer-facing (design, testing, ideas, plans), and the `main` README is a 67-line stub. We want a published user guide (install / pair / CLI / config / firewall / troubleshooting) on **Read the Docs**.

After research comparing the two RTD-supported generators, decisions (made with the user):
- **Tool: mdBook.** Rust-native single binary, in nixpkgs (zero Python), mature/stable (`book.toml`), first-class RTD support — fits the existing Rust + Nix dev shell. Zensical (the Material/MkDocs successor) is feature-rich but **early alpha (v0.0.43)** and pulls a Python toolchain into a Rust+Nix repo; revisit at 1.0 if versioned/i18n docs are ever needed.
- **v1 scope: skeleton + key pages.** Working site + full nav; write the high-value pages (install, quickstart, pairing, CLI reference, configuration, firewall, troubleshooting); stub the rest (profiles deep-dive, session automation, GUI/tray, FAQ).
- **CLI reference: auto-generated from clap** (`clap-markdown`), so it never drifts from `--help`.

**Content seed — the `origin/docs` branch.** An earlier effort (`github.com/ruffsl/TisVlay/tree/docs`, 19 commits, ~45 behind the merge-base) rewrote the README into a **566-line end-user guide** whose sections map almost 1:1 onto the mdBook nav, plus an LG backend README and a system-tray screenshot (`docs/screenshots/system_tray.png` via `.gitattributes` LFS). It is **not** an SSG setup — pure markdown content. Mine it as the primary source for most pages, **reconciling against current `main`** (it predates `on_dpms_off`/`on_dpms_on`, the DPMS/action naming, IPv6, the recent refactors). Read it with `git show docs:README.md` and `git show docs:crates/tisvlay-lg/README.md`.

The mdBook lives at **`docs/book/`** (`book.toml` + `src/`), namespaced so it never collides with the developer `docs/*.md`.

## Implementation

### 1. CLI crate: expose `Cli` + add the markdown generator
`crates/tisvlay-cli` is binary-only (`Cli`/`Commands` are private in `main.rs`). To render the clap tree, move the clap **type definitions** into a new `crates/tisvlay-cli/src/lib.rs` (`pub struct Cli` + `pub` subcommand/value enums), slim `main.rs` to `use tisvlay_cli::{…}` (runtime helpers + match arms stay in `main.rs`), and add `[lib] name = "tisvlay_cli"` alongside the existing `[[bin]]`. The integration test `crates/tisvlay-cli/tests/pair_snapshot.rs` spawns the **binary** (`CARGO_BIN_EXE_tisvlay`), so its 5 snapshots are unaffected as long as the bin still emits identical output.
- Add `clap-markdown = "0.1"` to `[dev-dependencies]` (dev-dep + example → never ships in the release binary).
- New `crates/tisvlay-cli/examples/gen-cli-docs.rs`: `print!("{}", clap_markdown::help_markdown_custom::<tisvlay_cli::Cli>(&opts))` with the generated title suppressed (avoids a double-H1 under the page's own heading).

### 2. mdBook scaffold — `docs/book/`
- `book.toml`: title `tisvlay`, `src = "src"`, `default-theme = "navy"`, search enabled, `git-repository-url` + `edit-url-template = ".../edit/main/docs/book/{path}"` (the `docs/book/` prefix is required since the book root isn't the repo root). Pull the repo URL from `[workspace.package].repository`.
- Enable **`mdbook-mermaid`** in v1 (the Architecture page reuses the `docs` branch's mermaid diagram; plugin is in nixpkgs). **Defer `mdbook-admonish`** to a later iteration to keep the 3-environment install burden (Nix shell / CI / RTD) minimal.
- `src/SUMMARY.md` nav:
  - `[Introduction]` (prefix chapter)
  - **Getting Started**: Installation, Quick Start, Pairing
  - **User Guide**: CLI Reference *(auto)*, Configuration, Profiles *(stub)*, Session Automation *(stub)*, GUI & Tray *(stub)*
  - **Operations**: Firewall & Discovery, Troubleshooting
  - **Reference**: Requirements & Compatibility, FAQ *(stub)*
- Every SUMMARY entry needs a real file (mdBook errors on missing/empty links); stubs get a heading + "Coming soon."

### 3. Wire the auto-generated CLI reference
`just docs-cli` runs the example → writes `docs/book/src/guide/cli-generated.md` (**committed**). The hand-written `src/guide/cli.md` adds intro/examples then `{{#include cli-generated.md}}` (relative to the including file; the partial is not a SUMMARY chapter). RTD runs a **plain `mdbook build`** and never invokes cargo — freshness is enforced in CI instead.

### 4. Content pages (adapt from `origin/docs` README, reconcile vs main)
Map the `docs` branch README sections onto pages: Features/Supported Devices/Architecture(+mermaid) → Introduction; Install (Debian/Ubuntu, Fedora/RHEL, Bazzite/Silverblue rpm-ostree, Arch, Nix flake, Cargo) → Installation; Pairing + day-to-day control → Quick Start + Pairing; Profiles/resolution/options/action-vocabulary/example-configs → Configuration (+ Profiles stub); firewall recipes → adapt `docs/firewall.md` into Operations/Firewall & Discovery; `docs/diagnostics.md` → Troubleshooting; Requirements/Limitations → Reference/Requirements. **Reconcile every config/CLI mention against current `main`** (current clap surface — the CLI page is auto-gen so it's self-correcting; current `SystemProfileConfig`/`UserProfileConfig` fields incl. `on_dpms_off`/`on_dpms_on`; current packaging artifact names). Reuse the tray screenshot on the GUI & Tray page (carry over the `.gitattributes` LFS rule for `docs/screenshots/`).

### 5. `.readthedocs.yaml` (repo root)
```yaml
version: 2
build:
  os: ubuntu-lts-latest
  tools:
    rust: latest
  commands:
    - cargo install mdbook --version 0.5.x --locked
    - cargo install mdbook-mermaid --version <pin> --locked
    - mdbook-mermaid install docs/book
    - mdbook build docs/book --dest-dir $READTHEDOCS_OUTPUT/html
```
Pin mdbook to the **0.5.x** line that nixpkgs ships (avoid local-vs-RTD skew; cold `cargo install` has no cross-build cache, so pinning bounds build time). The RTD Addons flyout still injects under `build.commands` (the "build.commands disables the flyout" note is outdated).

### 6. Tooling integration
- **`.gitignore`**: add `/docs/book/book/` (mdBook output is **not** currently ignored — would flood `git status`).
- **`flake.nix`**: add `mdbook` + `mdbook-mermaid` to the dev-shell `packages = with pkgs; [ … ]` list (lines ~52-99). All in nixpkgs (`mdbook` 0.5.2, `mdbook-mermaid` 0.17.0).
- **`justfile`** — new `[group('docs')]`: `docs-build` (`mdbook build docs/book`), `docs-serve` (`mdbook serve docs/book --open`), `docs-cli` (regen the partial; `@echo "… review and commit."` per the `i18n-extract` idiom), `docs-cli-check: docs-cli` then `git diff --exit-code` (drift gate). Bare commands (no embedded `nix develop -c`), matching `build`/`test`.
- **CI** (`.github/workflows/ci.yml`, which does **not** use Nix): a **separate `docs` job** — `taiki-e/install-action` for `mdbook` + `just`, then `just docs-cli-check` (fails if a clap change wasn't re-generated) and `just docs-build`. Keep it off the tuned `ci` job's cache/critical path.

### 7. README disposition
Keep `main`'s README **concise** (features + quickstart + a prominent link to the docs site); the comprehensive content from the `docs` branch lands in the book rather than re-bloating the README. (Recommendation — adjust at execution if you'd rather keep a fuller README too.)

## Critical files
- New: `crates/tisvlay-cli/src/lib.rs`, `crates/tisvlay-cli/examples/gen-cli-docs.rs`, `docs/book/book.toml`, `docs/book/src/SUMMARY.md` + chapter files, `.readthedocs.yaml`
- Edit: `crates/tisvlay-cli/src/main.rs`, `crates/tisvlay-cli/Cargo.toml`, `justfile`, `flake.nix`, `.github/workflows/ci.yml`, `.gitignore`, `README.md`
- Reference (read-only content source): `git show docs:README.md`, `git show docs:crates/tisvlay-lg/README.md`, `docs/firewall.md`, `docs/diagnostics.md`, `docs/configs/*.toml`, current `crates/tisvlay-core/src/config.rs`

## Gotchas (verified)
- `{{#include}}` paths are relative to the including file.
- `clap-markdown` emits its own H1 + preamble + command-overview TOC — suppress the title via `help_markdown_custom` to avoid a double-H1; the deep `profile user set` tree nests several heading levels (fine for reference).
- `edit-url-template` must carry the `docs/book/` prefix or "Edit this page" 404s.
- Local mdbook (nixpkgs 0.5.x) vs RTD (`cargo install`) version skew — pin both to the same 0.5.x.
- The lib refactor: only type **definitions** move; verify `propagate_version`/`global`/`default_value_t` attributes survive verbatim, then `just build` + `just test` (snapshots green).

## Verification
1. `nix develop -c just docs-cli` then `nix develop -c just docs-build` → site builds into `docs/book/book/`; open `index.html`, confirm nav, search, the included CLI reference, and the mermaid architecture diagram render.
2. `nix develop -c just docs-cli-check` → clean (committed partial matches the clap tree).
3. `nix develop -c just check` → the cli lib refactor keeps the workspace + `pair_snapshot` tests green.
4. `nix develop -c just docs-serve` for live-reload authoring while filling pages.
5. Connect the repo on Read the Docs, point at `.readthedocs.yaml`, confirm a successful cloud build + the Addons flyout.
