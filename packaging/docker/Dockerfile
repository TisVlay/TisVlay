# Reproducible build environment mirroring GitHub Actions ubuntu-latest.
# Useful for verifying the tisvlay-gui system-lib footprint without Nix
# and for debugging CI failures locally.
#
#   docker build -t tisvlay-ci -f packaging/docker/Dockerfile .
#   docker run --rm -v "$PWD:/work" -w /work tisvlay-ci just check
#
# The host workspace is bind-mounted, so cargo writes target/ back to
# the host — clean target/ first if it holds artifacts from a different
# libc/toolchain. The first cargo invocation inside the container reads
# rust-toolchain.toml and pulls the pinned toolchain via rustup.

FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/root/.cargo \
    RUSTUP_HOME=/root/.rustup \
    PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# Slint runtime/build deps (femtovg + winit, no Qt). Tracks
# slintRuntimeLibs in flake.nix and the System libs step in
# .github/workflows/ci.yml — keep all three in sync.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        build-essential \
        pkg-config \
        just \
        gettext \
    && rm -rf /var/lib/apt/lists/*

# Bootstrap rustup with a minimal stable toolchain. rust-toolchain.toml
# in the workspace will swap it for the pinned version on first invoke.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile default \
 && rustup component add llvm-tools-preview

# Test/coverage runners that `just test` and `just test-ci` require.
RUN cargo install --locked cargo-nextest cargo-llvm-cov

# Slint runtime/build deps (femtovg + winit, no Qt). Tracks
# slintRuntimeLibs in flake.nix and the System libs step in
# .github/workflows/ci.yml — keep all three in sync.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        build-essential \
        pkg-config \
        just \
        gettext \
        libfontconfig1-dev \
        # libgl-dev \
        # libxkbcommon-dev \
        # libwayland-dev \
        # libxcb1-dev \
        # libx11-dev \
        # libxcursor-dev \
        # libxi-dev \
        # libxrandr-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work
