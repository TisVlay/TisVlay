# OpenSSF Scorecard

Considered while building the signed release pipeline ([packaging/README.md](../../packaging/README.md#release-signing-and-verification)) and deferred. [OpenSSF Scorecard](https://github.com/ossf/scorecard) scores a repository's supply-chain practices across about 18 checks, and its GitHub Action (`ossf/scorecard-action`) can run weekly, publish a README badge, and report findings to the Security tab.

## Why it's deferred

- **zizmor already gates the workflow checks.** Pinned-Dependencies, Token-Permissions and Dangerous-Workflow overlap with zizmor's audits, which run as a prek hook and in CI's `checks` job on every change, and fail the change rather than lower a score afterwards.
- **What's left is mostly a badge and repo-level settings.** Branch-Protection, Security-Policy, Signed-Releases, Maintained, Vulnerabilities and the like. Most of those are met since the signed release pipeline (0.4.0): rulesets on `main` and `v*` tags, `SECURITY.md`, and Sigstore bundles attached to every release.
- **Some checks can't score well for a project with one main maintainer.** Code-Review wants human approval, or a merger other than the committer, on recent changes. Contributors wants recent commit authors from at least three companies.

## When to pick it up

- Downstream users or packagers ask for a Scorecard score, or deps.dev shows a stale or missing one that matters to them.
- A second regular maintainer makes Code-Review meaningful.

## How

A `scorecard.yml` workflow on a weekly schedule and on pushes to `main`. It would hold `id-token: write` (to publish results for the badge) and `security-events: write` (to upload SARIF), with `ossf/scorecard-action` pinned like every other action, plus the badge in `README.md`. Branch-Protection reads some settings only with an admin token; without one it scores what a non-admin can see, and treats the admin-only requirements as met.
