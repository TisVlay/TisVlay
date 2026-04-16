{
  lib,
  kernel,
  kernelModuleMakeFlags,
}:

kernel.stdenv.mkDerivation {
  pname = "tisvlay-backlight";
  version = "0.1.0";

  src = lib.cleanSource ./.;

  hardeningDisable = [ "pic" ];
  nativeBuildInputs = kernel.moduleBuildDependencies;

  makeFlags = kernelModuleMakeFlags ++ [
    "KDIR=${kernel.dev}/lib/modules/${kernel.modDirVersion}/build"
  ];

  installFlags = [ "INSTALL_MOD_PATH=${placeholder "out"}" ];
  installTargets = [ "modules_install" ];

  meta = {
    description = "Virtual backlight devices for network-controlled TVs (tisvlay)";
    license = lib.licenses.gpl2Only;
    platforms = lib.platforms.linux;
    broken = !kernel.withRust;
  };
}
