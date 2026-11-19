/**
 * Semantic tokens, light and dark. Every colour is a neutral UI role (surface, text, border,
 * action, feedback); nothing here names a business concept. The values are the domain-neutral
 * subset of the iAsBuilt colour tokens (global.*, feedback.*, actions.link/checkbox), with the
 * action and danger fills re-picked so white (light) or navy (dark) text on them clears WCAG AA
 * 4.5:1. The contrast test in `contrast.test.ts` measures every pair listed in `PAIRS`.
 */
export const colorTokens = {
  light: {
    'bg-app': '#FFFFFF',
    'bg-subtle': '#F8FAFC',
    'bg-page': '#F4F6F9',
    'bg-inset': '#F1F5F9',
    surface: '#FFFFFF',
    'surface-raised': '#FFFFFF',
    'surface-modal': '#FFFFFF',
    'surface-disabled': '#F1F5F9',
    'surface-selected': '#D1E2F7',
    scrim: 'rgba(15, 23, 42, 0.55)',
    'text-primary': '#0F172A',
    'text-secondary': '#334155',
    'text-tertiary': '#64748B',
    'text-disabled': '#94A3B8',
    'text-link': '#1A5FB4',
    'text-danger': '#B91C1C',
    'text-success': '#15803D',
    'text-warning': '#B45309',
    'text-info': '#1D4ED8',
    border: '#7D8BA1',
    'border-subtle': '#E2E8F0',
    'border-strong': '#64748B',
    'border-focus': '#1A5FB4',
    'border-danger': '#B91C1C',
    divider: '#E2E8F0',
    'focus-ring': '#1A5FB4',
    'action-bg': '#1A5FB4',
    'action-bg-hover': '#134E94',
    'action-fg': '#FFFFFF',
    'action-quiet-hover': '#EBF3FB',
    'danger-bg': '#B91C1C',
    'danger-bg-hover': '#991B1B',
    'danger-fg': '#FFFFFF',
    'danger-subtle': '#FEF2F2',
    'control-checked': '#1A5FB4',
    'control-checked-fg': '#FFFFFF',
  },
  dark: {
    'bg-app': '#0B1120',
    'bg-subtle': '#0F172A',
    'bg-page': '#0B1120',
    'bg-inset': '#020617',
    surface: '#0F172A',
    'surface-raised': '#1E293B',
    'surface-modal': '#1E293B',
    'surface-disabled': '#1E293B',
    'surface-selected': '#1E3A8A',
    scrim: 'rgba(0, 0, 0, 0.65)',
    'text-primary': '#F8FAFC',
    'text-secondary': '#CBD5E1',
    'text-tertiary': '#94A3B8',
    'text-disabled': '#64748B',
    'text-link': '#62A0EA',
    'text-danger': '#F87171',
    'text-success': '#4ADE80',
    'text-warning': '#FCD34D',
    'text-info': '#60A5FA',
    border: '#7B8AA3',
    'border-subtle': '#1E293B',
    'border-strong': '#94A3B8',
    'border-focus': '#62A0EA',
    'border-danger': '#F87171',
    divider: '#334155',
    'focus-ring': '#62A0EA',
    'action-bg': '#62A0EA',
    'action-bg-hover': '#A4C5EE',
    'action-fg': '#0B1120',
    'action-quiet-hover': '#172554',
    'danger-bg': '#F87171',
    'danger-bg-hover': '#FCA5A5',
    'danger-fg': '#2D0606',
    'danger-subtle': '#2D0606',
    'control-checked': '#62A0EA',
    'control-checked-fg': '#0B1120',
  },
} as const;

export type ColorToken = keyof typeof colorTokens.light;
export type Theme = keyof typeof colorTokens;

/** Theme-independent scale. Fonts are the platform stacks, so no font file is shipped or licensed. */
export const scaleTokens = {
  'space-1': '4px',
  'space-2': '8px',
  'space-3': '12px',
  'space-4': '16px',
  'space-5': '24px',
  'radius-sm': '4px',
  'radius-md': '6px',
  'radius-lg': '10px',
  'font-sans':
    "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif",
  'font-mono': "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
  'font-size-sm': '0.875rem',
  'font-size-md': '1rem',
  'font-size-lg': '1.125rem',
  'shadow-lg': '0 20px 40px rgba(2, 6, 23, 0.28)',
  'z-modal': '1000',
} as const;

export const varName = (token: string): string => `--ui-${token}`;

/** `var(--ui-…)` reference, for inline styles and CSS-in-JS. */
export const v = (token: ColorToken | keyof typeof scaleTokens): string => `var(${varName(token)})`;
