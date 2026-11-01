## What and why

<!-- What changes, and the problem it solves. Link the issue if there is one. -->

## Plan

<!-- docs/plans/<name>.md, or "not needed" for a small fix. -->

## Tested on

<!-- Which TV and backend, or the mock. -->

## Checklist

See the [contributing guide](https://github.com/TisVlay/TisVlay/blob/main/.github/CONTRIBUTING.md).

- [ ] The title follows Conventional Commits, and a wire-visible change is a `feat` or breaking, never a `fix`.
- [ ] A plan-driven PR: every commit subject follows them too, and the plan records any deviations.
- [ ] `prek run -a` passes, or the docs-only check for a docs change.
- [ ] Tests cover the change, where it applies.
- [ ] Generated docs (`just docs-gen`) and translation templates (`just i18n-extract`) are regenerated, if the change affects them.
