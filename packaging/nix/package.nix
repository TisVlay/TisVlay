{
  lib,
  rustPlatform,
  callPackage,
  cargo-about,
  pkg-config,
  dbus,
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
  postInstall = ''
    install -Dm0644 packaging/licensing/NOTICE -t $out/share/doc/tisvlay/
    install -Dm0644 packaging/licensing/THIRD-PARTY-LICENSES.html -t $out/share/doc/tisvlay/
  '';

  postFixup = ''
    patchelf --add-rpath "${lib.makeLibraryPath finalAttrs.rpathLibs}" \
      $out/bin/tisvlay-gui \
      $out/bin/tisvlay-session
  '';
  dontPatchELF = true;

  useNextest = true;

  # Tests need the `mock` feature (and other test-only features) across
  # the workspace; the build itself ships default features only.
  cargoTestFlags = [ "--all-features" ];

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
