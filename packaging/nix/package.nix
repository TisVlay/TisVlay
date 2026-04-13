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

  cargoHash = "sha256-n/neNRUdxB33b/FyQLtAjiAbyuKHhjxcTlUCApvcuBo=";

  nativeBuildInputs = [
    pkg-config
  ];

  nativeCheckInputs = [
    dbus
  ];

  meta = {
    description = "TV Display control for Linux";
    longDescription = ''
      TODO
    '';
    homepage = "https://github.com/ruffsl/tisvlay";
    license = lib.licenses.asl20;
    maintainers = [ ];
    platforms = lib.platforms.linux;
    mainProgram = "tisvlay";
  };
}
