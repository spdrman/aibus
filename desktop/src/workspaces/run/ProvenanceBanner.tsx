import { Button } from "@sharedui/react-primitives";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { Snapshot, StackInfo } from "../../ipc";
import { inTauri } from "./useFixtureReplay";

// Always on screen: says whether what is shown is a recorded fixture or a live run. The spike only
// ever gets a fixture, and there is deliberately no control that pretends to start a live one.
export function ProvenanceBanner({ snap, error, onReplay }: { snap: Snapshot | null; error: string | null; onReplay: () => void }) {
  const [stack, setStack] = useState<StackInfo | null>(null);
  useEffect(() => {
    if (inTauri) invoke<StackInfo>("stack_info").then(setStack).catch(() => {});
  }, []);
  const prov = snap?.provenance;
  return (
    <div className="banner" role="status">
      {prov?.kind === "fixture" ? (
        <>
          <strong>Fixture</strong>
          <span>
            {prov.label}. Capture {prov.capture}, Claude Code {prov.cli_version}. Step {snap!.step}/{snap!.steps}.
          </span>
        </>
      ) : prov?.kind === "live" ? (
        <strong>Live</strong>
      ) : (
        <span>{error ?? "Starting..."}</span>
      )}
      <Button variant="primary" size="sm" onClick={onReplay}>Replay</Button>
      {stack && (
        <span className="stack">
          Tauri {stack.tauri}, {stack.os}/{stack.arch}, webview {stack.webview ?? "unknown"}
        </span>
      )}
      {error && prov && <span>{error}</span>}
    </div>
  );
}
