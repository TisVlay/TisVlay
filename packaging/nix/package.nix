{
  lib,
  rustPlatform,
  pkg-config,
}:

rustPlatform.buildRustPackage {
  pname = "tisvlay";
  version = "0.1.0";

  src = lib.cleanSource ../../.;

  cargoHash = "sha256-v3DZ6+6TfS2W6bZCrIPbgBRzQ+gslpYJxjk/9hEod94=";

  nativeBuildInputs = [
    pkg-config
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
