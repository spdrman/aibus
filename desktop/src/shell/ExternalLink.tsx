import type { ReactNode } from "react";
import { defaultOpener, safeExternalUrl } from "./safeUrl";

export function ExternalLink({ href, children, open = defaultOpener }: { href: string; children?: ReactNode; open?: (url: string) => void }) {
  const url = safeExternalUrl(href);
  // Unsafe or malformed: plain text, so the reader can still see what was offered but cannot click it.
  if (!url) return <span className="muted">{children ?? href}</span>;
  return (
    <a
      href={url}
      rel="noopener noreferrer"
      onClick={(e) => {
        e.preventDefault();
        open(url);
      }}
    >
      {children ?? url}
      <span className="ui-sr-only"> (opens in your browser)</span>
    </a>
  );
}
