import { createContext, useContext, useEffect, useId, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { resolveReturnFocus, trapTab } from './focus.js';

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  /** Buttons for the footer, usually inside <DialogActions>. */
  footer?: ReactNode;
  /** CSS width of the panel. Defaults to 32rem. */
  width?: string;
  disableEscapeClose?: boolean;
  disableBackdropClose?: boolean;
  /** Where focus goes on close. Defaults to the control that opened the dialog. */
  returnFocusTo?: HTMLElement | null;
}

// Nested dialogs: only the deepest open one answers Escape. Depth follows the React tree, not mount order,
// because child effects run before parent effects and DOM order depends on how the dialogs were opened.
const DepthContext = createContext(0);
const openDepths: number[] = [];

export function Dialog({
  open,
  onClose,
  title,
  children,
  footer,
  width = '32rem',
  disableEscapeClose = false,
  disableBackdropClose = false,
  returnFocusTo,
}: DialogProps) {
  const panel = useRef<HTMLDivElement>(null);
  const returnTo = useRef<HTMLElement | null>(null);
  const titleId = useId();
  const depth = useContext(DepthContext) + 1;

  // Capture the opener once per open, and give focus back on close or unmount.
  useEffect(() => {
    if (!open) return;
    returnTo.current = resolveReturnFocus(returnFocusTo);
    const t = setTimeout(() => {
      const root = panel.current;
      if (!root) return;
      root.querySelector<HTMLElement>('[data-autofocus]')?.focus();
      if (!root.contains(document.activeElement)) root.focus();
    }, 0);
    return () => {
      clearTimeout(t);
      const el = returnTo.current;
      returnTo.current = null;
      if (el?.isConnected) el.focus();
    };
    // returnFocusTo is read once, at open time.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Registered even when Escape is disabled: a dialog that will not close still covers the ones beneath it.
  useEffect(() => {
    if (!open) return;
    openDepths.push(depth);
    return () => {
      const at = openDepths.lastIndexOf(depth);
      if (at !== -1) openDepths.splice(at, 1);
    };
  }, [open, depth]);

  useEffect(() => {
    if (!open || disableEscapeClose) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      for (const other of openDepths) if (other > depth) return;
      e.preventDefault();
      onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open, onClose, disableEscapeClose, depth]);

  if (!open) return null;

  return createPortal(
    <div
      className="ui-dialog__backdrop"
      onClick={(e) => {
        if (!disableBackdropClose && e.target === e.currentTarget) onClose();
      }}
      onMouseDown={(e) => {
        // Pressing the backdrop must not drop focus to <body>, or Shift+Tab walks out of the modal.
        if (e.target !== e.currentTarget) return;
        e.preventDefault();
        const active = document.activeElement;
        if (active === null || active === document.body) panel.current?.focus();
      }}
    >
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className="ui-dialog"
        style={{ width }}
        onKeyDown={(e) => trapTab(panel.current, e)}
      >
        <div className="ui-dialog__header">
          <h2 id={titleId} className="ui-dialog__title">
            {title}
          </h2>
          <button type="button" className="ui-dialog__close" aria-label="Close" onClick={onClose}>
            <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
              <path d="M3 3l10 10M13 3L3 13" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" />
            </svg>
          </button>
        </div>
        <div className="ui-dialog__body">
          <DepthContext.Provider value={depth}>{children}</DepthContext.Provider>
        </div>
        {footer ? <div className="ui-dialog__footer">{footer}</div> : null}
      </div>
    </div>,
    document.body,
  );
}

/** Right-aligned row of buttons for a dialog footer. */
export function DialogActions({ children }: { children: ReactNode }) {
  return <div className="ui-dialog__actions">{children}</div>;
}
