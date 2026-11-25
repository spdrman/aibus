import { Button } from "@sharedui/react-primitives";
import { useEffect, useState } from "react";
import { loadPref, savePref } from "./prefs";
import type { Workspace } from "./registry";
import { useTheme } from "./theme";

export const SHORTCUTS = [
  ["Ctrl/Cmd + 1 to 9", "Switch workspace"],
  ["Ctrl/Cmd + Shift + L", "Toggle light and dark theme"],
  ["Ctrl/Cmd + /", "Show or hide this list"],
] as const;

// Navigation, theme and shortcuts around a list of workspaces. Every workspace stays mounted and the
// inactive ones are only hidden, so switching tabs never drops a workspace's state (an open dock
// layout, a scroll position, a selection).
export function Shell({ workspaces, header }: { workspaces: Workspace[]; header?: React.ReactNode }) {
  const ids = workspaces.map((w) => w.id);
  const [active, setActive] = useState(() => loadPref("workspace", ids[0], (v): v is string => typeof v === "string" && ids.includes(v)));
  const [help, setHelp] = useState(false);
  const { choice, toggle } = useTheme();

  const go = (id: string, focus = false) => {
    setActive(id);
    savePref("workspace", id);
    if (focus) document.getElementById(`ws-nav-${id}`)?.focus();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      if (/^[1-9]$/.test(e.key) && !e.shiftKey && workspaces[Number(e.key) - 1]) {
        e.preventDefault();
        go(workspaces[Number(e.key) - 1].id, true);
      } else if (e.shiftKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        toggle();
      } else if (e.key === "/") {
        e.preventDefault();
        setHelp((h) => !h);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const onNavKey = (e: React.KeyboardEvent) => {
    const at = ids.indexOf(active);
    const next = e.key === "ArrowRight" || e.key === "ArrowDown" ? (at + 1) % ids.length : e.key === "ArrowLeft" || e.key === "ArrowUp" ? (at - 1 + ids.length) % ids.length : e.key === "Home" ? 0 : e.key === "End" ? ids.length - 1 : -1;
    if (next >= 0) {
      e.preventDefault();
      go(ids[next], true);
    }
  };

  return (
    <div className="shell">
      <header className="shell__bar">
        <nav aria-label="Workspaces" className="shell__nav" onKeyDown={onNavKey}>
          {workspaces.map((w, i) => (
            <Button
              key={w.id}
              id={`ws-nav-${w.id}`}
              variant={w.id === active ? "primary" : "quiet"}
              size="sm"
              aria-current={w.id === active ? "page" : undefined}
              tabIndex={w.id === active ? 0 : -1}
              title={i < 9 ? `Ctrl/Cmd + ${i + 1}` : undefined}
              onClick={() => go(w.id)}
            >
              {w.label}
            </Button>
          ))}
        </nav>
        <div className="shell__tools">
          <Button variant="quiet" size="sm" onClick={toggle} title="Toggle light and dark theme">
            Theme: {choice}
          </Button>
          <Button variant="quiet" size="sm" aria-expanded={help} onClick={() => setHelp((h) => !h)}>
            Shortcuts
          </Button>
        </div>
      </header>
      {help && (
        <section className="shell__help" aria-label="Keyboard shortcuts">
          <dl>
            {SHORTCUTS.map(([keys, what]) => (
              <div key={keys} className="shell__shortcut">
                <dt><kbd>{keys}</kbd></dt>
                <dd>{what}</dd>
              </div>
            ))}
          </dl>
        </section>
      )}
      {header}
      <main className="shell__main">
        {workspaces.map((w) => (
          <section key={w.id} hidden={w.id !== active} aria-label={w.label} className="shell__workspace">
            {w.render()}
          </section>
        ))}
      </main>
    </div>
  );
}
