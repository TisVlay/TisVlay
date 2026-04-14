# NixOS VM test for the tisvlay module.
#
# Exercises the module end-to-end: imports it, enables the service,
# and verifies the system daemon reaches `active (running)`, owns the
# well-known D-Bus name, and responds to `GetSupportedVendors` with
# the expected vendor list.
#
# User services (`tisvlay-session`, `tisvlay-tray`) are not auto-started
# because their `wantedBy = graphical-session.target` requires a real
# display manager. This test verifies that the units are at least
# loaded correctly; exercising them end-to-end needs a graphical VM
# which is out of scope here.
#
# Invoke: `nix build .#checks.x86_64-linux.vm`
#         or `nix flake check`

{
  pkgs ? import <nixpkgs> { },
}:

pkgs.testers.runNixOSTest {
  name = "tisvlay";

  nodes.machine =
    { config, pkgs, ... }:
    {
      imports = [ ../../packaging/nix/module.nix ];

      services.tisvlay.enable = true;

      # Keep the VM small — tisvlay is the only service under test.
      virtualisation.memorySize = 1024;

      # The tray's graphical-session wantedBy means it never auto-starts
      # in a headless VM; no display manager needed for what we verify.
    };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("tisvlayd.service")

    # Daemon is active
    machine.succeed("systemctl is-active tisvlayd.service")

    # D-Bus well-known name is owned
    machine.wait_until_succeeds(
        "busctl --system status org.tisvlay.Daemon",
        timeout=30,
    )

    # Manager1 interface is reachable
    machine.succeed(
        "busctl --system introspect "
        "org.tisvlay.Daemon /org/tisvlay/Manager "
        "| grep -q 'org.tisvlay.Manager1'"
    )

    # GetSupportedVendors returns the LG backend (mock is behind a feature
    # flag that ships disabled by default in the Nix package).
    vendors = machine.succeed(
        "busctl --system call "
        "org.tisvlay.Daemon /org/tisvlay/Manager "
        "org.tisvlay.Manager1 GetSupportedVendors"
    )
    assert '"lg"' in vendors, f"expected lg vendor in {vendors!r}"

    # ObjectManager reports zero TVs on a fresh install (no pairing yet).
    # `GetManagedObjects` returns `a{oa{sa{sv}}}` — empty outer dict = 0.
    managed = machine.succeed(
        "busctl --system call "
        "org.tisvlay.Daemon /org/tisvlay "
        "org.freedesktop.DBus.ObjectManager GetManagedObjects"
    )
    # busctl renders empty array-of-dict as `a{...} 0`.
    assert " 0" in managed, \
        f"expected empty ObjectManager, got {managed!r}"

    # User units are loaded (even though not started in headless VM)
    machine.succeed(
        "systemctl --global list-unit-files tisvlay-session.service "
        "| grep -q tisvlay-session"
    )
    machine.succeed(
        "systemctl --global list-unit-files tisvlay-tray.service "
        "| grep -q tisvlay-tray"
    )

    # StateDirectory exists with correct ownership.
    machine.succeed("test -d /var/lib/tisvlay")
    owner = machine.succeed("stat -c '%U:%G' /var/lib/tisvlay").strip()
    assert owner == "tisvlay:tisvlay", f"unexpected owner {owner!r}"

    # Stable UID/GID from the module option (default 979).
    uid = machine.succeed("id -u tisvlay").strip()
    assert uid == "979", f"expected uid 979, got {uid!r}"

    # systemd-analyze security: exposure level must be `OK` or better.
    # A fully-hardened long-running daemon should land at exposure ≤ 2.0
    # ("OK" band). This guards against accidental regressions in
    # module.nix hardening directives.
    sec = machine.succeed("systemd-analyze security tisvlayd.service")

    # Dump the full analyzer output to the test log — future runs
    # (and anyone reading the build log) can see which settings the
    # daemon earns vs. loses on without re-running the VM.
    machine.log("systemd-analyze security tisvlayd.service:\n" + sec)

    # Parse the overall exposure line. Format (systemd 257+):
    #   → Overall exposure level for tisvlayd.service: 1.3 OK 🙂
    import re
    match = re.search(
        r"Overall exposure level for [^:]+:\s+([\d.]+)",
        sec,
    )
    assert match, f"no exposure score in output:\n{sec}"
    score = float(match.group(1))
    machine.log(f"tisvlayd systemd-analyze score: {score}")
    assert score <= 2.0, f"tisvlayd exposure {score} exceeds 2.0 budget"
  '';
}
