# Component inventory: iasbuilt/webapp

Read from webapp `main` (de3490d, 2026-11-18). `src/components/common/` holds 41 folders; the rest of `src/` (pages, modules, state, graphql, layout) is application code. I read the sources of the components marked "read"; the others I classified from their folder contents and import lists, and the table says so.

Classes: **reusable** (domain-free, themeable as is), **adaptable** (a generic idea, but tied to Tailwind classes, hard-coded colours, Kendo icons, the webapp's `@/` modules or its grid engine, so it needs a rewrite or a port), **domain-coupled** (stays in the webapp).

## Extracted in 0.1.0

| Webapp source | Class | What happened |
| --- | --- | --- |
| `Button` | adaptable (read) | Re-written as `Button`: variants and sizes kept (`primary`, `quiet`, `danger`), hard-coded hex colours replaced by tokens, `ToggleButton3D` left behind (asset-grid specific). |
| `TextBox` | adaptable (read) | Re-written as `TextBox`: Tailwind classes and the search and clear icons dropped, label is always a real `<label>`, helper and error text are tied with `aria-describedby`. |
| `Checkbox` | reusable (read header) | Re-written on a native input with a label, plus the `indeterminate` property. |
| `Form/FormRow` | reusable (read) | Ported as `FormRow` and `ReadOnlyRow`, inline styles moved to token-driven CSS. |
| `Dialog` (+ `focusTrap`, `dialogTrigger`, `DialogActionsBar`) | adaptable (read) | Ported as `Dialog` and `DialogActions`: focus trap, Escape by React-tree depth, focus return, `data-autofocus`, backdrop press handling and the WebKit pointer fallback kept in about a tenth of the code. The XL grid escape contract (`escapeContract`) and the grid z-index band were left out, they only exist because dialogs there host the grid. |
| (none, new) | n/a | `Tabs`: the webapp has no tabs component (its tab-like UI sits in grid and company panels), so this one is written to the WAI-ARIA pattern. The decision gives "tabs/layout primitives" as one of the three. |
| `styles/tokens.ts`, `@iasbuilt/tokens` | adaptable | The domain-neutral subset (`global.*`, `feedback.*`, link and checkbox colours) became `@sharedui/design-tokens`. See `DECISIONS.md` for the two roles I re-picked so white text on them passes AA. |

## Not extracted yet

| Webapp source | Class | Why |
| --- | --- | --- |
| `Card`, `Badge`, `Chip`, `ChipList`, `Typography`, `RadioGroup`, `TextArea`, `ResizableSplitter` | reusable (imports only React, `@/styles/tokens` and the local `Icon`) | Same recipe as `Button` and `TextBox`. Not needed for the three components the issue asks for, so deferred rather than half-done. |
| `DropDownList`, `CheckboxDropdown`, `Popup`, `DatePicker` | adaptable | Depend on `Icon` and Kendo SVG icons (`@progress/kendo-svg-icons`), and `Popup` on portal positioning. Need a licence-clean icon decision first. |
| `Icon`, `FaIcon`, `Icons` | adaptable | Font Awesome and Kendo icon sets, with their own licences (see `LICENSING.md`). No icon set is shipped in 0.1.0; the dialog close glyph is inline SVG. |
| `AppNav`, `HierarchicalNavigation`, `ResultsList` | adaptable | Navigation shells mixing Font Awesome and Kendo icons and the webapp's tree model. A generic tree/nav primitive is worth building later. |
| `ConfirmDestructive`, `TextConfirmDeactivateDialog` | adaptable | Thin wrappers over the webapp `Dialog` with its strings. Straightforward on top of the shared `Dialog` once the webapp adopts it. |
| `Notification` | adaptable | Reads the webapp logger, the causl state and a media-query hook. |
| `MarkdownEditor`, `WysiwygEditor` | adaptable (README and imports only) | TipTap based. Neutral in principle, heavy in dependencies. |

## Domain-coupled, stay in the webapp

`AssetGrid`, `DataGrid`, `XLDataGrid`, `grid-shared` (grid engine adapters, imports from `@xldatagrid/*`, row models for assets), `ChangePasswordDialog` (GraphQL), `AdminOverlay`, `FilterPanel`, `SearchOverlay`, `UploadField`, `ColorPalette` (classified from names and imports, not read in detail), plus everything outside `components/common/`: `pages/`, `modules/` (assets, contacts, documents, permissions, plans, readiness, spaces), `state/`, `graphql/`, `layout/Header`, `planView`.

The data grid is not extracted at all. The decision is TanStack Table and TanStack Virtual (both MIT) for aibus, wrapped in a later `data-grid` package, not the webapp's XL engine.
