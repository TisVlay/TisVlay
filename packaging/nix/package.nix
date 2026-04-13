{
  lib,
  rustPlatform,
  pkg-config,
  cargo-nextest,
  dbus,
}:

rustPlatform.buildRustPackage {
  pname = "tisvlay";
  version = "0.1.0";

  src = lib.cleanSource ../../.;

  cargoHash = "sha256-n/neNRUdxB33b/FyQLtAjiAbyuKHhjxcTlUCApvcuBo=";

  nativeBuildInputs = [
    pkg-config
  ];

  nativeCheckInputs = [
    cargo-nextest
    dbus
  ];

  checkPhase = ''
    cargo nextest run --workspace --all-features
  '';

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
