import type { ReactNode } from "react";

export type NoticeKind = "empty" | "missing-cli" | "auth-failed" | "error";

// Empty and error states. Errors use role="alert" so a screen reader announces them when they
// appear; empty states are a status region. The title names the problem, the body says what to do.
export function StateNotice({ kind, title, children, action }: { kind: NoticeKind; title: string; children?: ReactNode; action?: ReactNode }) {
  const isError = kind !== "empty";
  return (
    <section className="notice" data-kind={kind} role={isError ? "alert" : "status"} aria-labelledby={`notice-${kind}`}>
      <h2 id={`notice-${kind}`}>{title}</h2>
      {children && <p>{children}</p>}
      {action}
    </section>
  );
}

export function MissingCliNotice({ provider, command }: { provider: string; command: string }) {
  return (
    <StateNotice kind="missing-cli" title={`${provider} CLI not found`}>
      aibus could not find <code>{command}</code> on your PATH. Install it, then check again.
    </StateNotice>
  );
}

export function AuthFailedNotice({ provider, hint }: { provider: string; hint: string }) {
  return (
    <StateNotice kind="auth-failed" title={`${provider} sign-in failed`}>
      {hint}
    </StateNotice>
  );
}
