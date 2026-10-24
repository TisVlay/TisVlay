# Security policy

## Supported versions

Security fixes go into the next release from `main`. Only the latest minor release is supported; earlier minors don't get backports.

That covers everything this repository ships: the crates on crates.io (`tisvlay-core`, `tisvlay-protocol` and the `tisvlay` SDK), the `.deb`, `.rpm` and Arch packages on the releases page, and the Nix package and NixOS module. A vulnerability in a TV's own firmware belongs with the TV's vendor.

## Reporting a vulnerability

Please don't open a public issue. Report it privately instead, through GitHub's [private vulnerability reporting](https://github.com/TisVlay/TisVlay/security/advisories/new) form: the repository's Security tab, then "Report a vulnerability".

It helps to include:

- the affected version, and how TisVlay was installed;
- what an attacker can do, and from where: the local network, another local user, or a browser;
- steps to reproduce, or a proof of concept.

## What to expect

TisVlay is maintained by volunteers, so reports are handled on a best-effort basis, with no fixed timeline. Replies come in the report's advisory thread, where the fix and its disclosure can be worked out together.

## Disclosure

Once a fix is released, it's published as a GitHub security advisory, which can request a CVE, crediting the reporter unless they'd rather not be named. A vulnerability in a published crate also gets a [RustSec advisory](https://rustsec.org/), so `cargo audit` and `cargo deny` users are warned.
