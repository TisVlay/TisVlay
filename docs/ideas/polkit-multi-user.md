# Polkit gating for multi-user shared workstations

Current trust model is "any local user with D-Bus access can mutate config." Fine for single-user desktops (the typical TisVlay deployment); the daemon does the privileged work as the dedicated `tisvlay` system user.

For multi-user shared workstations (lab machines, family PCs where one user's edit shouldn't silently rebind another's TV), polkit is the canonical Linux gate. When this becomes a concern, the work is:

1. Add `polkit::AuthorityProxy::check_authorization` to each privileged `Manager1` / `Tv1` method handler. Read-only properties stay open.
2. Ship `org.tisvlay.policy.xml` declaring action IDs (one per privileged method or grouped by lifecycle).
3. Document the action IDs in the NixOS module so admins can pin the auth rules per their tenancy model.

No GUI-side changes — polkit prompts surface through the existing freedesktop polkit agent (KDE / GNOME both ship one). Adds zero overhead in the single-user case (polkit's `check_authorization` short-circuits when the caller is root or the daemon's own user).

