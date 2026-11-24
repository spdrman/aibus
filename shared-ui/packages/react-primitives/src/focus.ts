const TABBABLE = [
  'a[href]',
  'area[href]',
  'button',
  'input:not([type="hidden"])',
  'select',
  'textarea',
  'summary',
  '[contenteditable="true"]',
  '[tabindex]',
].join(',');

type Focusable = HTMLElement | SVGElement;

function isTabStop(el: Focusable): boolean {
  if (el.hasAttribute('disabled')) return false;
  if (el.closest('[inert],[aria-hidden="true"]')) return false;
  if (el.tabIndex < 0) return false;
  // jsdom has no layout, so only a laid-out element can be judged hidden by its size.
  const style = getComputedStyle(el);
  return style.display !== 'none' && style.visibility !== 'hidden';
}

/** Tab stops inside `root`, in DOM order. */
export function tabStops(root: HTMLElement): Focusable[] {
  return Array.from(root.querySelectorAll<Focusable>(TABBABLE)).filter(isTabStop);
}

/**
 * Keep Tab and Shift+Tab inside `root`. Wraps at both ends, and pulls focus back in when it sits
 * on the root itself or has escaped it. With no tab stop at all, Tab stays on the root.
 */
export function trapTab(root: HTMLElement | null, e: { key: string; shiftKey: boolean; preventDefault(): void }): void {
  if (e.key !== 'Tab' || !root) return;
  const stops = tabStops(root);
  if (stops.length === 0) {
    e.preventDefault();
    root.focus();
    return;
  }
  const first = stops[0];
  const last = stops[stops.length - 1];
  const active = document.activeElement;
  if (e.shiftKey && (active === first || active === root || !root.contains(active))) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (active === last || !root.contains(active))) {
    e.preventDefault();
    first.focus();
  }
}

/**
 * Which element should get focus back when a dialog closes. WebKit (Safari and the macOS webview)
 * does not focus a button on click, so `document.activeElement` is the body there; fall back to the
 * button the last pointer press landed on, if that press was recent.
 */
let lastPress: { el: Element; at: number } | null = null;
let tracking = false;
const PRESS_MAX_AGE_MS = 1000;

function trackPresses(): void {
  if (tracking || typeof document === 'undefined') return;
  tracking = true;
  document.addEventListener(
    'pointerdown',
    (e) => {
      if (e.target instanceof Element) lastPress = { el: e.target, at: Date.now() };
    },
    true,
  );
}

export function resolveReturnFocus(explicit?: HTMLElement | null): HTMLElement | null {
  if (explicit) return explicit;
  const active = document.activeElement;
  if (active instanceof HTMLElement && active !== document.body) return active;
  if (lastPress && Date.now() - lastPress.at < PRESS_MAX_AGE_MS) {
    const target = lastPress.el.closest<HTMLElement>('button,a[href],[tabindex],input,select,textarea,summary');
    if (target?.isConnected) return target;
  }
  return null;
}

trackPresses();
