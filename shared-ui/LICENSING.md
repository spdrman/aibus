# Licensing register

Policy: everything that ships in, or is pulled in by, these packages is MIT-compatible. Allowed: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, plus 0BSD, Unicode-3.0 and MIT-0 where a transitive dependency needs them. No GPL, AGPL, LGPL with linking, per-seat or proprietary terms. `npm run licences` enforces it with `license-checker-rseidelsohn --onlyAllow` (a violation exits non-zero), once for the production closure of the shipped packages and once for everything installed in the workspace.

## Our packages

| Package | Version | Licence | Ships |
| --- | --- | --- | --- |
| `@sharedui/design-tokens` | 0.1.0 | MIT | tokens (see origin below), CSS, no fonts, no icons |
| `@sharedui/react-primitives` | 0.1.0 | MIT | React components and CSS, one inline SVG (the dialog close cross, drawn for this package) |
| `@sharedui/aibus-smoke` | 0.1.0 | MIT, private | not published |

Runtime dependencies of the shipped packages: none. React, react-dom and the tokens package are peer dependencies. No font files are shipped (the font stacks are the platform fonts), and no icon set.

## Origin of the token values

The colour values come from `iasbuilt/tokens` (`@iasbuilt/tokens` 0.1.4, `global.*`, `feedback.*`, `actions.link` and `actions.checkbox` in `light.json` and `dark.json`). That package is marked UNLICENSED, all rights reserved. Putting the values in a public MIT package is the owner's decision of 2026-11-14 ("public, MIT-compatible only"), and the colour data and the focus-trap and dialog behaviour that I ported are iAsBuilt's to relicense. **This is the one row that needs the owner's explicit sign-off before the packages are published or the repo is split out**, since the licence file on `iasbuilt/tokens` says otherwise today. Two roles are my own picks and not iAsBuilt's values: the light `border` and the action and danger fills (see `DECISIONS.md`). I did not copy the data grid, CAD, comments or per-button tokens.

## Dependencies installed in the workspace (tooling only, nothing shipped)

Counts from the gate on 2026-11-18 (`npm run licences`, "everything installed"): MIT 101, Apache-2.0 4, ISC 4, BSD-2-Clause 2, BSD-3-Clause 2, MIT-0 1. Notable direct ones:

| Package | Licence | Role |
| --- | --- | --- |
| react, react-dom 19.2.0 | MIT | peer dependency, showcase |
| typescript 5.9.3 | Apache-2.0 | build |
| vite 7.1.12, vitest 4.0.8 | MIT | showcase build, tests |
| jsdom 26.1.0 | MIT | tests |
| @testing-library/react, dom, jest-dom, user-event | MIT | tests |
| @csstools/color-helpers (via jsdom) | MIT-0 | transitive, allowed because MIT-0 is MIT without the notice duty |

Two things I kept out on purpose. `@vitejs/plugin-react` pulls in Babel and `caniuse-lite` (CC-BY-4.0, outside the list), so the showcase uses Vite's built-in esbuild JSX instead. The checker itself runs through `npx` and is not in the tree it audits, because its own `spdx-*` and `glob` dependencies carry CC and BlueOak-1.0.0 licences.

## What the webapp ships that these packages do not (so it stays out)

`@progress/kendo-*` (Kendo theme and SVG icons, Progress licence terms to be checked before any extraction), `@fortawesome/*` (icons CC-BY-4.0, fonts OFL-1.1, code MIT), `@mui/material` (MIT), TipTap (MIT, with paid Pro extensions we must not use), `@xldatagrid/*` (iAsBuilt). None of these is a dependency of the shared packages. Rust side: aibus's `cargo deny check` is unchanged, this change adds no crate.
