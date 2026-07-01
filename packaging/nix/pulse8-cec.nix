# Out-of-tree build of the in-tree `pulse8-cec` HDMI-CEC driver.
#
# nixpkgs' kernel config enables the CEC core and native DisplayPort-AUX
# CEC (amdgpu/i915) but NOT the USB serial CEC adapters (`USB_PULSE8_CEC`
# is off). Rather than force a full kernel rebuild to flip that one
# symbol, we compile just this driver as an out-of-tree module against
# the already-built `kernel.dev`. It links only against exported symbols
# — the CEC core (`cec.ko`, which nixpkgs does build) and `serdev` — so
# no in-tree rebuild is needed.
#
# The source comes from `kernel.src`, so this auto-tracks the kernel
# version; consume it via `config.boot.kernelPackages.callPackage` so the
# vermagic always matches the running kernel.
{
  lib,
  stdenv,
  kernel,
}:

stdenv.mkDerivation {
  pname = "pulse8-cec";
  version = kernel.version;

  src = kernel.src;

  # The kernel tarball is huge; extract only the one driver directory
  # (its build depends on `kernel.dev`'s headers, not the source tree).
  unpackPhase = ''
    runHook preUnpack
    # Match the files *under* the driver dir (a bare dir pattern would
    # extract only the empty directory entry). strip-components=1 drops
    # the `linux-<ver>/` tarball prefix.
    tar --wildcards -xf "$src" --strip-components=1 \
      '*/drivers/media/cec/usb/pulse8/*'
    sourceRoot=drivers/media/cec/usb/pulse8
    runHook postUnpack
  '';

  nativeBuildInputs = kernel.moduleBuildDependencies;

  makeFlags = kernel.makeFlags ++ [
    "-C"
    "${kernel.dev}/lib/modules/${kernel.modDirVersion}/build"
    "M=$(PWD)"
    # The in-tree Makefile is `obj-$(CONFIG_USB_PULSE8_CEC) += pulse8-cec.o`;
    # forcing the symbol to `m` on the command line turns it into `obj-m`
    # for this external-module build.
    "CONFIG_USB_PULSE8_CEC=m"
  ];

  buildFlags = [ "modules" ];

  installPhase = ''
    runHook preInstall
    install -Dm444 pulse8-cec.ko \
      "$out/lib/modules/${kernel.modDirVersion}/extra/pulse8-cec.ko"
    runHook postInstall
  '';

  meta = {
    description = "Out-of-tree build of the in-tree pulse8-cec HDMI-CEC driver (USB CEC adapters without a full kernel rebuild)";
    longDescription = ''
      Compiles the Linux kernel's Pulse-Eight USB-CEC serial driver as a
      loadable module against the active kernel, so `/dev/cecN` appears for
      Pulse-Eight-class adapters on kernels (like nixpkgs') that don't build
      `CONFIG_USB_PULSE8_CEC`. Pair with an `inputattach --pulse8-cec` unit
      to attach the adapter's serial line discipline.
    '';
    license = lib.licenses.gpl2Only;
    platforms = lib.platforms.linux;
  };
}
