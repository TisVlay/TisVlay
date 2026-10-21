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
  kernelModuleMakeFlags,
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

  # `kernelModuleMakeFlags` (the toolchain, plus the kmodigest `cmd_sign`
  # override), not `kernel.makeFlags`: the latter adds kernel-build-only
  # flags (`O=$(buildRoot)`, `--eval=undefine modules`) that break an
  # external `M=` module build.
  makeFlags = kernelModuleMakeFlags ++ [
    "-C"
    "${kernel.dev}/lib/modules/${kernel.modDirVersion}/build"
    "M=$(PWD)"
    # The in-tree Makefile is `obj-$(CONFIG_USB_PULSE8_CEC) += pulse8-cec.o`;
    # forcing the symbol to `m` on the command line turns it into `obj-m`
    # for this external-module build.
    "CONFIG_USB_PULSE8_CEC=m"
  ];

  buildFlags = [ "modules" ];

  # Kbuild's `modules_install`, not a plain copy: its `cmd_sign` step is
  # where nixpkgs' kmodigest (wired in via `kernelModuleMakeFlags`) signs the
  # module and writes the `.ko.cer` that the module aggregator merges into
  # `modules.cer`. On kernels without kmodigest it's just an install.
  # DEPMOD=true skips depmod; the aggregator regenerates the maps itself.
  # INSTALL_MOD_STRIP=1 strips before signing: the module is compressed
  # here, so stdenv's strip phase can't reach its debug info afterwards.
  installTargets = [ "modules_install" ];
  installFlags = [
    "INSTALL_MOD_PATH=${placeholder "out"}"
    "INSTALL_MOD_DIR=extra"
    "INSTALL_MOD_STRIP=1"
    "DEPMOD=true"
  ];

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
