# System libraries Slint's femtovg renderer + winit's wayland / X11
# backends dlopen at runtime. patchelf can't see dlopen targets, so
# package.nix splices them onto the GUI binary's rpath in postFixup,
# and flake.nix's dev shell exports them via LD_LIBRARY_PATH so
# `cargo run` works in-tree.
#
#   - fontconfig: femtovg renderer (default)
#   - libGL:      OpenGL ES backend
#   - libxkbcommon + wayland: winit Wayland backend
#   - libxcb / libx11 / libxcursor / libxi / libxrandr:
#     winit X11 backend (X11 sessions only)
{
  fontconfig,
  libGL,
  libxkbcommon,
  wayland,
  libxcb,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
}:

[
  fontconfig
  libGL
  libxkbcommon
  wayland
  libxcb
  libx11
  libxcursor
  libxi
  libxrandr
]
