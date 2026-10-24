# Ship an SBOM file inside the distro packages

Considered in the release supply-chain plan ([docs/plans/release-supply-chain.md](../plans/release-supply-chain.md)) and deferred. Releases carry one SBOM per binary per architecture, as release assets and as signed attestations, but the `.deb`, `.rpm` and `.pkg.tar.zst` themselves contain no SBOM file.

## Why it's deferred

- **There's no convention to follow.** No cross-distro location exists for SBOM files on an installed Linux system, and the Filesystem Hierarchy Standard defines none. Debian and Fedora treat the package database as the inventory, and tools such as debsbom, distro2SBOM, syft and Trivy generate SBOMs from that database rather than looking for files.
- **Scanners already read the binaries.** The packaged binaries are built with cargo-auditable, and Trivy, grype, syft and `cargo audit bin` read its embedded dependency list directly. Nothing found shows any of them picking up a loose SBOM file under `/usr/share/doc`.
- **People already have the licenses.** License data is the one thing a file would add over the embedded list, and `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` already ships it in readable form.

## Precedents

- Talos Linux installs SPDX files under `/usr/share/spdx/`, and its extensions' under `/usr/local/share/spdx/`.
- Python's PEP 770 standardized `.dist-info/sboms/` inside wheels (accepted in 2025; auditwheel and maturin write there). It covers wheels only, not installed systems.
- Docker's BuildKit SBOM write-up used `/usr/local/share/sbom/` as an example location, not a rule.

## When to pick it up

- A distro policy or the Filesystem Hierarchy Standard names a location for SBOM files.
- A scanner starts reading SBOM files from installed systems.
- A user or a compliance review asks for one.

## How

Add an nfpm `contents` entry in `crates/tisvlay-cli/Cargo.toml` that installs `target/sbom/<triple>/{tisvlay,tisvlay-gui}.cdx.json` at the agreed location. The release build already runs `just sbom` before nfpm, so nothing needs reordering. The file can't describe the package it ships in, since adding it changes that package's digest, so it describes the binaries, as the released SBOMs do.
