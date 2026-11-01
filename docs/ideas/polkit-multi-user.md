# Polkit gating for multi-user shared workstations

Current trust model is "any local user who can reach the daemon's socket can mutate config": the `tisvlay` group, the active seat's user, and the greeter. Fine for single-user desktops (the typical TisVlay deployment); the daemon does the privileged work as the dedicated `tisvlay` system user.

For multi-user shared workstations (lab machines, family PCs where one user's edit shouldn't silently rebind another's TV), polkit is the canonical Linux gate. When this becomes a concern, the work is:

1. Gate each write rpc (the `write` entries in `tisvlay-protocol`'s `endpoint` tables) on `polkit::AuthorityProxy::check_authorization`; reads stay open. The hard part is the caller's identity: a zenoh query over the shared socket doesn't carry the peer's credentials the way a D-Bus call did, so this needs either per-caller identity on the link or the writes moved to an endpoint that has it.
2. Ship `org.tisvlay.policy.xml` declaring action IDs (one per privileged method or grouped by lifecycle).
3. Document the action IDs in the NixOS module so admins can pin the auth rules per their tenancy model.

No GUI-side changes — polkit prompts surface through the existing freedesktop polkit agent (KDE / GNOME both ship one). Adds zero overhead in the single-user case (polkit's `check_authorization` short-circuits when the caller is root or the daemon's own user).
