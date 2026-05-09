# DE-driven theming via xdg-desktop-portal Settings

The Slint `fluent` style doesn't pick up the user's KDE accent color or custom palette. True DE-driven theming would require:

- xdg-desktop-portal `Settings` namespace queries for the accent color and color-scheme preference.
- Writing the resolved values into `Palette.accent-background` / `Palette.background` etc. at runtime.
- Watching for Settings-namespace change signals so the UI re-tints when the user changes their KDE settings.

Scoped out of Plan 4's theming sweep (which settled on `fluent` + a stable token surface as the pragmatic v1) because the Settings-namespace work has no user-visible payoff over fluent's defaults until a user complains the GUI looks out-of-place in their themed desktop.

Revisit when a user reports the visual mismatch, or when `cosmic` (the alternative with looser Slint integration) becomes the right default and fluent's hard-coded look starts feeling stale.

