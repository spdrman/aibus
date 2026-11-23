# Deferred work

Scope I cut from an issue to close it with a reduced definition of done. Each section names the issue
it came from and where the rest is picked up. Nothing here has its own issue; the EPIC's "Deferred work"
section points back to this file.

## #305 B1.2 Shared React components (PR #374)

What #305 closes with: six primitives (`Button`, `TextBox`, `Checkbox`, `FormRow`, `Dialog`, `Tabs`) and
the design tokens in `shared-ui/`, the npm licence gate, and the `consumers/aibus-smoke` consumer that
imports them by package name. What I left for later...

- **Desktop app consumption moves to #307 (B1.4).** `desktop/` has no React frontend yet, so the aibus
  consumer today is the smoke app. B1.4 builds the shell from these primitives and tokens: it adds
  `@sharedui/design-tokens` and `@sharedui/react-primitives` to the desktop `package.json`, imports them
  by name (no copied source), and the smoke app stops being the only consumer.
- **Licence sign-off on the token values.** The colour values come from `iasbuilt/tokens`, which is
  UNLICENSED today. The PR waits on the owner's sign-off before it merges (see `shared-ui/LICENSING.md`).
- **Separate semver repo.** The packages live in `shared-ui/` here; `git subtree split --prefix=shared-ui`
  makes the repo once the owner picks its org and name. Publishing the `@sharedui` scope waits on that too.
- **More components.** Card, Badge, Chip, RadioGroup and the rest that `shared-ui/INVENTORY.md` classes as
  reusable. Icons wait on a licence-clean icon set.
- **Data grid package.** TanStack Table plus Virtual, when B1.4 or later has a real use for it.
- **Webapp adoption.** Moving the webapp onto the packages is a webapp change and needs paulotten's review.
  The local `scripts/webapp-consumer-check.mjs` run is the cross-consumer check until then.
