# shared-ui

Two small packages that aibus and the iAsBuilt webapp both consume, so a dialog or a text field is written once. Nothing in them knows about assets, facilities, GraphQL or a backend... (see "Keeping it neutral" below for how that is held).

| Package | What it is |
| --- | --- |
| `@sharedui/design-tokens` | Colour, spacing, type and shadow tokens as typed data and as CSS variables (`--ui-*`), light and dark. Also the WCAG contrast helper the tests use. |
| `@sharedui/react-primitives` | `Button`, `TextBox`, `Checkbox`, `FormRow` / `ReadOnlyRow`, `Dialog` / `DialogActions`, `Tabs`. React 18.3 or 19, peer dependency, no other runtime dependency. |

Both are MIT, versioned with semver (`0.1.0` now), and published independently. `consumers/aibus-smoke` is the aibus consumer: it imports the packages by name, never by path, and doubles as the isolated showcase (`npm run dev -w @sharedui/aibus-smoke`, add `?theme=dark` to force a theme).

## Using them

```tsx
import '@sharedui/design-tokens/tokens.css';      // once, at the app root
import '@sharedui/react-primitives/styles.css';
import { Button, Dialog, DialogActions, Tabs, TextBox } from '@sharedui/react-primitives';
```

Theme: light is the default, the OS dark preference switches to dark, and `<html data-theme="light|dark">` overrides both. Your own CSS should read the same variables (`var(--ui-surface)`, `var(--ui-text-primary)`), or `import { v } from '@sharedui/design-tokens'` and write `v('surface')` in inline styles.

Things worth knowing:

- `Button` defaults to `type="button"`. Variants are `primary`, `quiet`, `danger`.
- `TextBox` and `Checkbox` always render a real `<label>`, and `TextBox` wires `error` or `helperText` to the input with `aria-describedby`. There is no unlabelled mode on purpose.
- `Dialog` is `role="dialog"` and `aria-modal`, named by its title, traps Tab and Shift+Tab, closes on Escape and on a backdrop click (both can be turned off), and returns focus to the control that opened it, including in WebKit where a click does not focus a button. Mark the field that should get focus first with `data-autofocus`. With nested dialogs only the innermost answers Escape.
- `Tabs` follows the WAI-ARIA tabs pattern (arrows wrap over enabled tabs, Home and End, one tab stop). Only the selected panel is rendered.

## Rules for changes

1. Neutral only. A component takes data and callbacks, never a domain type, an endpoint or auth state.
2. Colours come from tokens. Add a token to both themes, and add its pairs to `contrast.test.ts` (4.5:1 for text, 3:1 for UI parts and large text).
3. Every new component gets keyboard and role tests, and shows up in the consumer showcase.
4. A new dependency needs its licence on the allow list in `scripts/licences.mjs` (MIT, Apache-2.0, BSD-2/3-Clause, ISC, 0BSD, Unicode-3.0, MIT-0) and a row in `LICENSING.md`.
5. Breaking change means a major version, and the webapp check below has to pass first.

## Checks

From `shared-ui/`:

```sh
npm install --ignore-scripts
npm run check                  # build, tests, aibus consumer build, licence gate
node scripts/webapp-consumer-check.mjs /path/to/iasbuilt/webapp   # the webapp consumer
```

The webapp check packs both packages, installs them into a scratch project that uses the webapp's own React, `@types/react` and TypeScript versions and its `tsconfig` options, type-checks a probe that imports every primitive, and renders it on the server. It reads the webapp checkout and changes nothing in it. It runs on a developer machine, not in aibus CI, because the webapp is private.

### Keeping it neutral

The packages have no dependency beyond React (and each other), so there is nothing to import a domain library from, and the licence gate lists every package a consumer installs. A domain import would also fail the consumer build, since the consumer carries no GraphQL, router or store.

## Where it came from

`INVENTORY.md` classifies the webapp components (what is reusable, adaptable or domain-coupled and why), `LICENSING.md` is the register, and `DECISIONS.md` records the calls made while extracting.
