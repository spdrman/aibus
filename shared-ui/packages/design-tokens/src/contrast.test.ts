import { describe, expect, it } from 'vitest';
import { colorTokens, contrastRatio, type ColorToken, type Theme } from './index.js';

type Pair = [fg: ColorToken, bg: ColorToken, min: number, what: string];

/** Text on its ground needs 4.5:1, UI components and large text 3:1 (WCAG 2.2 AA). */
const PAIRS: Pair[] = [
  ['text-primary', 'bg-app', 4.5, 'body text'],
  ['text-primary', 'surface-modal', 4.5, 'dialog text'],
  ['text-primary', 'bg-page', 4.5, 'text on page'],
  ['text-secondary', 'bg-app', 4.5, 'secondary text'],
  ['text-secondary', 'surface-modal', 4.5, 'secondary dialog text'],
  ['text-tertiary', 'bg-app', 4.5, 'helper text'],
  ['text-tertiary', 'surface-modal', 4.5, 'helper text in dialog'],
  ['text-link', 'bg-app', 4.5, 'link'],
  ['text-danger', 'bg-app', 4.5, 'error text'],
  ['text-danger', 'surface-modal', 4.5, 'error text in dialog'],
  ['action-fg', 'action-bg', 4.5, 'primary button label'],
  ['action-fg', 'action-bg-hover', 4.5, 'primary button label, hover'],
  ['danger-fg', 'danger-bg', 4.5, 'danger button label'],
  ['danger-fg', 'danger-bg-hover', 4.5, 'danger button label, hover'],
  ['control-checked-fg', 'control-checked', 4.5, 'check mark'],
  ['text-primary', 'surface-selected', 4.5, 'selected row text'],
  ['text-link', 'action-quiet-hover', 4.5, 'quiet button label, hover'],
  ['border', 'bg-app', 3, 'input border'],
  ['border', 'surface-modal', 3, 'input border in dialog'],
  ['focus-ring', 'bg-app', 3, 'focus ring'],
  ['focus-ring', 'surface-modal', 3, 'focus ring in dialog'],
  ['border-focus', 'bg-app', 3, 'focused input border'],
  ['border-danger', 'bg-app', 3, 'invalid input border'],
  ['action-bg', 'bg-app', 3, 'primary button edge'],
];

describe('WCAG AA contrast, both themes', () => {
  for (const theme of ['light', 'dark'] as Theme[]) {
    for (const [fg, bg, min, what] of PAIRS) {
      it(`${theme}: ${what} (${fg} on ${bg}) >= ${min}:1`, () => {
        const ratio = contrastRatio(colorTokens[theme][fg], colorTokens[theme][bg]);
        expect(ratio, `${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(min);
      });
    }
  }

  it('both themes define the same tokens', () => {
    expect(Object.keys(colorTokens.dark).sort()).toEqual(Object.keys(colorTokens.light).sort());
  });
});
