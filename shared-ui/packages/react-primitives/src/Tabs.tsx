import { useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';

export interface TabItem {
  id: string;
  label: string;
  disabled?: boolean;
  content: ReactNode;
}

export interface TabsProps {
  items: TabItem[];
  /** Controlled selection. Omit it, with `defaultId`, for an uncontrolled tab set. */
  selectedId?: string;
  defaultId?: string;
  onSelect?: (id: string) => void;
  /** Accessible name for the tab list. */
  label: string;
}

/**
 * WAI-ARIA tabs with automatic activation: Left and Right move between enabled tabs and wrap, Home and End jump
 * to the ends, and only the selected tab is in the Tab order. Panels of unselected tabs are not rendered.
 */
export function Tabs({ items, selectedId, defaultId, onSelect, label }: TabsProps) {
  const base = useId();
  const firstEnabled = items.find((t) => !t.disabled)?.id;
  const [inner, setInner] = useState(defaultId ?? firstEnabled);
  const current = selectedId ?? inner;
  const refs = useRef(new Map<string, HTMLButtonElement>());

  const select = (id: string) => {
    if (selectedId === undefined) setInner(id);
    onSelect?.(id);
    refs.current.get(id)?.focus();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const enabled = items.filter((t) => !t.disabled);
    const at = enabled.findIndex((t) => t.id === current);
    let next: number;
    if (e.key === 'ArrowRight') next = (at + 1) % enabled.length;
    else if (e.key === 'ArrowLeft') next = (at - 1 + enabled.length) % enabled.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = enabled.length - 1;
    else return;
    e.preventDefault();
    if (enabled[next]) select(enabled[next].id);
  };

  const active = items.find((t) => t.id === current);
  return (
    <div className="ui-tabs">
      <div role="tablist" aria-label={label} className="ui-tabs__list" onKeyDown={onKeyDown}>
        {items.map((t) => (
          <button
            key={t.id}
            ref={(el) => {
              if (el) refs.current.set(t.id, el);
              else refs.current.delete(t.id);
            }}
            type="button"
            role="tab"
            id={`${base}-tab-${t.id}`}
            aria-selected={t.id === current}
            aria-controls={`${base}-panel-${t.id}`}
            tabIndex={t.id === current ? 0 : -1}
            disabled={t.disabled}
            className="ui-tabs__tab"
            onClick={() => select(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      {active ? (
        <div role="tabpanel" id={`${base}-panel-${active.id}`} aria-labelledby={`${base}-tab-${active.id}`} tabIndex={0} className="ui-tabs__panel">
          {active.content}
        </div>
      ) : null}
    </div>
  );
}
