# Security & Supply Chain

Every release is built and signed by the project's release workflow, and ships with what you need to check it: checksums, signatures for the packages and the binaries inside them, and an SBOM (software bill of materials) for each binary. Releases are immutable: once published, a release's files can't be replaced.

The commands below use these variables; set `FILE` to the package you downloaded:

```sh
{{#include ../generated/release-version.txt}}
export BASE=https://github.com/TisVlay/TisVlay/releases/download/v${VERSION}
FILE=tisvlay_${VERSION}-1_amd64.deb
```

## Checksums

The quickest check, with no extra tools. It confirms the download arrived intact:

```sh
wget ${BASE}/checksums-sha256.txt
sha256sum --ignore-missing -c checksums-sha256.txt
```

The checksum file comes from the same releases page as the package, so it can't show that the release itself is genuine. The signatures can.

## Signatures

Each package, and each binary inside it, is signed when it's built, through [Sigstore](https://www.sigstore.dev/). A signature proves which repository, commit and workflow produced the file.

<details>
<summary id="verify-with-gh">With the GitHub CLI</summary>

With the [GitHub CLI](https://cli.github.com/), signed in to any GitHub account:

```sh
gh attestation verify "$FILE" \
  --repo TisVlay/TisVlay \
  --signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml \
  --source-ref refs/tags/v${VERSION}
```

- Keep `--signer-workflow`. Without it, a signature from any workflow in the repository would pass, including ones that run pull-request code.
- The packages carry the binaries unchanged, so the same command also works on an installed system: `gh attestation verify /usr/bin/tisvlay …`.

</details>

<details>
<summary id="verify-with-cosign">Without a GitHub account (cosign)</summary>

Use [cosign](https://docs.sigstore.dev/cosign/) with the signature bundle from the releases page:

```sh
wget ${BASE}/tisvlay-${VERSION}-$(uname -m).provenance.sigstore.json
cosign verify-blob-attestation \
  --bundle tisvlay-${VERSION}-$(uname -m).provenance.sigstore.json \
  --certificate-identity https://github.com/TisVlay/TisVlay/.github/workflows/build-packages.yml@refs/tags/v${VERSION} \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --type https://slsa.dev/provenance/v1 \
  "$FILE"
```

</details>

To check that a file is one the release published, `gh release verify-asset v${VERSION} "$FILE"`.

## SBOMs

Each binary has an SBOM: the crates it was built from, with their versions, licenses and checksums, in CycloneDX format. The releases page has them as `tisvlay-${VERSION}-$(uname -m).cdx.json` and `tisvlay-gui-${VERSION}-$(uname -m).cdx.json`, and they're signed like the packages.

<details>
<summary id="verify-sbom">Check and print a binary's SBOM</summary>

```sh
gh attestation verify /usr/bin/tisvlay \
  --repo TisVlay/TisVlay \
  --signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml \
  --source-ref refs/tags/v${VERSION} \
  --predicate-type https://cyclonedx.org/bom \
  --format json --jq '.[0].verificationResult.statement.predicate'
```

</details>

## Scanning an installed system

The binaries also carry their own dependency list, built in by [cargo-auditable](https://github.com/rust-secure-code/cargo-auditable). So a vulnerability scanner can check an installed system against published advisories, with nothing to download from the release:

```sh
cargo audit bin /usr/bin/tisvlay /usr/bin/tisvlay-gui
```

[Trivy](https://trivy.dev/) and [grype](https://github.com/anchore/grype) read the same list:

```sh
trivy rootfs --scanners vuln /usr/bin/tisvlay
grype /usr/bin/tisvlay
```

A finding isn't necessarily new: the advisories the project has already assessed are listed, each with its reason, in [`deny.toml`](https://github.com/TisVlay/TisVlay/blob/main/packaging/licensing/deny.toml).

## NixOS

Nix checks the cache's signing key on every prebuilt path it substitutes (see [Prebuilt binaries](../getting-started/installation.md#prebuilt-binaries-optional)), so there's nothing to verify by hand. The Nix package's binaries carry the same dependency list, so the scanning above works on them too.
