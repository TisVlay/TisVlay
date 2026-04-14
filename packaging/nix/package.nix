{
  lib,
  rustPlatform,
  pkg-config,
  dbus,
}:

rustPlatform.buildRustPackage {
  pname = "tisvlay";
  version = "0.1.0";

  src = lib.cleanSource ../../.;

  cargoLock.lockFile = ../../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
  ];

  nativeCheckInputs = [
    dbus
  ];

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
