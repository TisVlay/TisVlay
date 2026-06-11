{
  lib,
  rustPlatform,
  callPackage,
  cargo-about,
  pkg-config,
  dbus,
  makeDesktopItem,
  copyDesktopItems,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "tisvlay";
  version = "0.1.0";

  src = lib.cleanSource ../../.;

  cargoLock.lockFile = ../../Cargo.lock;

  # tisvlay-gui dlopens Slint's femtovg + winit runtime libs;
  # tisvlay-session dlopens libwayland-client.so via wayland-sys for
  # its greeter-wake idle notifier. patchelf doesn't see dlopen
  # targets, so they don't end up on the default rpath — splice them
  # on explicitly in postFixup and disable the default patchelf pass
  # so it doesn't strip them back out. The session technically only
  # needs wayland; sharing the GUI's lib set keeps the rpath plumbing
  # single-sourced (the unused entries are stat()'d once at startup
  # and the libs are already in the closure). The set is shared with
  # flake.nix's dev shell via slint-runtime.nix.
  rpathLibs = callPackage ./slint-runtime.nix { };

  nativeBuildInputs = [
    # build.rs (tisvlay-gui) and preBuild both invoke cargo-about for
    # the same THIRD-PARTY-LICENSES.html — embedded in the binary and
    # installed to share/doc/ respectively.
    cargo-about
    pkg-config
    # Setup-hook that copies `desktopItems` into $out/share/applications/.
    # Standard nixpkgs idiom — 622 packages use this pattern.
    copyDesktopItems
  ];

  # GUI desktop entry. Generated via makeDesktopItem (the
  # schema-validated nixpkgs helper) rather than hand-rolling
  # the file or inlining a string in module.nix — `.desktop`
  # entries are application metadata, not system policy, and
  # this is the universal convention. Mirrors the static
  # packaging/freedesktop/org.tisvlay.Gui.desktop used verbatim
  # by nfpm .deb / .rpm builds; the two are kept in sync by
  # hand (drift visible at code-review time).
  #
  # `exec` stays relative — environment.systemPackages adds the
  # package's bin/ to /run/current-system/sw/bin/, which is on
  # the user's $PATH, and freedesktop launchers consult $PATH
  # for relative Exec entries.
  desktopItems = [
    (makeDesktopItem {
      name = "org.tisvlay.Gui";
      desktopName = "TisVlay";
      genericName = "TV Display Control";
      comment = "Control connected TVs from the desktop";
      exec = "tisvlay-gui";
      tryExec = "tisvlay-gui";
      # Freedesktop icon-theme name (no bundled artwork yet — the
      # tray uses the same placeholder). Swap to a packaged
      # icon name when custom art lands.
      icon = "video-display";
      categories = [
        "Settings"
        "HardwareSettings"
      ];
      keywords = [
        "tv"
        "television"
        "remote"
        "control"
      ];
      startupNotify = true;
      startupWMClass = "org.tisvlay.Gui";
      dbusActivatable = true;
      # Emits SingleMainWindow=true per spec 1.5. The helper also
      # writes Version=1.5 automatically, so neither needs an
      # extraConfig override.
      singleMainWindow = true;
      implements = [ "org.freedesktop.Application" ];
    })
  ];

  buildInputs = finalAttrs.rpathLibs;

  nativeCheckInputs = [
    dbus
  ];

  # Disable cargo-about's clearlydefined.io fallback — the Nix sandbox
  # has no network. Sed targets the build copy only; the checked-in
  # about.toml keeps clearlydefined enabled for dev / CI runs.
  preBuild = ''
    sed -i '1ino-clearly-defined = true' packaging/licensing/about.toml
    cargo about generate \
      --config packaging/licensing/about.toml \
      --workspace --all-features --locked --fail \
      packaging/licensing/about.html.hbs \
      -o packaging/licensing/THIRD-PARTY-LICENSES.html
  '';

  # Match the nfpm /usr/share/doc/tisvlay/ layout so the GUI's
  # current_exe()-relative path resolution finds the file.
  #
  # The D-Bus session activation file installs from the static
  # template + substituteInPlace because dbus-daemon execs the
  # `Exec=` line literally without consulting $PATH, so the
  # template's `/usr/bin/tisvlay-gui` (correct on Debian /
  # Fedora) would ENOENT on NixOS. `--replace-fail` makes the
  # package build fail loudly if the static template's Exec
  # format drifts — no silent breakage.
  postInstall = ''
    install -Dm0644 packaging/licensing/NOTICE -t $out/share/doc/tisvlay/
    install -Dm0644 packaging/licensing/THIRD-PARTY-LICENSES.html -t $out/share/doc/tisvlay/
    install -Dm0644 packaging/systemd/org.tisvlay.Gui.service -t $out/share/dbus-1/services/

    substituteInPlace $out/share/dbus-1/services/org.tisvlay.Gui.service \
      --replace-fail 'Exec=/usr/bin/tisvlay-gui' "Exec=$out/bin/tisvlay-gui"
  '';

  postFixup = ''
    patchelf --add-rpath "${lib.makeLibraryPath finalAttrs.rpathLibs}" \
      $out/bin/tisvlay-gui \
      $out/bin/tisvlay-session
  '';
  dontPatchELF = true;

  # Build scoping comes from `workspace.default-members` in the root
  # Cargo.toml (the shipping binary set) — buildRustPackage's bare
  # `cargo build` picks it up, so the crate list lives in one place
  # instead of being repeated here as `-p` flags. Without it the default
  # build would also compile the dev-only crates (tisvlay-mock /
  # tisvlay-test / tisvlay-harness).

  # Ship the daemon's "Mock (testing)" backend for field debugging, in
  # step with the justfile's package-build. Scoped to tisvlayd — only the
  # daemon builds the backend registry.
  buildFeatures = [ "tisvlayd/mock" ];

  useNextest = true;

  # Tests opt back into the full workspace: `--workspace` because
  # `default-members` would otherwise narrow the default `cargo test` to
  # the shipping binaries, and the dev-only crates' tests (which need the
  # `mock` and other test-only features) are still wanted here.
  cargoTestFlags = [ "--workspace" "--all-features" ];

  meta = {
    description = "TV Display control for Linux";
    homepage = "https://github.com/ruffsl/tisvlay";
    changelog = "https://github.com/ruffsl/tisvlay/releases";
    license = lib.licenses.asl20;
    maintainers = [
      {
        name = "ruffsl";
        github = "ruffsl";
      }
    ];
    platforms = lib.platforms.linux;
    mainProgram = "tisvlay";
  };
})
