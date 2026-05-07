{
  lib,
  rustPlatform,
  cargo-about,
  pkg-config,
  dbus,
}:

rustPlatform.buildRustPackage {
  pname = "tisvlay";
  version = "0.1.0";

  src = lib.cleanSource ../../.;

  cargoLock.lockFile = ../../Cargo.lock;

  nativeBuildInputs = [
    # build.rs (tisvlay-gui) and preBuild both invoke cargo-about for
    # the same THIRD-PARTY-LICENSES.html — embedded in the binary and
    # installed to share/doc/ respectively.
    cargo-about
    pkg-config
  ];

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
}
