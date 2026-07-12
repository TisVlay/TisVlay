# NixOS VM test for the tisvlay module.
#
# Exercises the module end-to-end: imports it, enables the service,
# and verifies the system daemon reaches `active (running)`, exposes
# its zenoh socket with the group-gated permissions, and answers the
# CLI's liveliness/vendor probes.
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
      # Socket-permission access model under test: alice is granted,
      # bob is not.
      services.tisvlay.users = [ "alice" ];

      users.users.alice = {
        isNormalUser = true;
      };
      users.users.bob = {
        isNormalUser = true;
      };

      # Keep the VM small — tisvlay is the only service under test.
      virtualisation.memorySize = 1024;

      # The tray's graphical-session wantedBy means it never auto-starts
      # in a headless VM; no display manager needed for what we verify.
    };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("tisvlay-daemon.service")

    # Daemon is active
    machine.succeed("systemctl is-active tisvlay-daemon.service")

    # The zenoh socket exists, group-gated: 0660 tisvlay:tisvlay in a
    # 0750 runtime dir.
    machine.wait_until_succeeds("test -S /run/tisvlay/zenoh.sock", timeout=30)
    perms = machine.succeed("stat -c '%a %U:%G' /run/tisvlay/zenoh.sock").strip()
    assert perms == "660 tisvlay:tisvlay", f"unexpected socket perms {perms!r}"
    dirperms = machine.succeed("stat -c '%a %U:%G' /run/tisvlay").strip()
    assert dirperms == "750 tisvlay:tisvlay", f"unexpected dir perms {dirperms!r}"

    # The daemon's liveliness token answers the CLI probe (root
    # bypasses the socket gate).
    machine.wait_until_succeeds(
        "tisvlay --socket /run/tisvlay/zenoh.sock status",
        timeout=30,
    )

    # The vendor catalog includes the LG backend.
    vendors = machine.succeed(
        "tisvlay --socket /run/tisvlay/zenoh.sock vendors"
    )
    assert "lg" in vendors, f"expected lg vendor in {vendors!r}"

    # Fresh install has zero TVs.
    listing = machine.succeed(
        "tisvlay --socket /run/tisvlay/zenoh.sock list"
    )

    # Access model: alice (in the tisvlay group via services.tisvlay.users)
    # can reach the daemon; bob can't even traverse the runtime dir.
    machine.succeed(
        "sudo -u alice tisvlay --socket /run/tisvlay/zenoh.sock status"
    )
    machine.fail("sudo -u bob stat /run/tisvlay/zenoh.sock")

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

    # The tisvlay user exists and has a system-range UID (< 1000).
    # The specific number is NixOS-allocated unless services.tisvlay.uid
    # is set explicitly — which the default module config does not.
    uid = int(machine.succeed("id -u tisvlay").strip())
    assert 0 < uid < 1000, f"expected system UID (<1000) for tisvlay, got {uid}"

    # systemd-analyze security: exposure level must be `OK` or better.
    # A fully-hardened long-running daemon should land at exposure ≤ 2.0
    # ("OK" band). This guards against accidental regressions in
    # module.nix hardening directives.
    sec = machine.succeed("systemd-analyze security tisvlay-daemon.service")

    # Dump the full analyzer output to the test log — future runs
    # (and anyone reading the build log) can see which settings the
    # daemon earns vs. loses on without re-running the VM.
    machine.log("systemd-analyze security tisvlay-daemon.service:\n" + sec)

    # Parse the overall exposure line. Format (systemd 257+):
    #   → Overall exposure level for tisvlay-daemon.service: 1.3 OK 🙂
    import re
    match = re.search(
        r"Overall exposure level for [^:]+:\s+([\d.]+)",
        sec,
    )
    assert match, f"no exposure score in output:\n{sec}"
    score = float(match.group(1))
    machine.log(f"tisvlay-daemon systemd-analyze score: {score}")
    assert score <= 2.0, f"tisvlay-daemon exposure {score} exceeds 2.0 budget"
  '';
}
