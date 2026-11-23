import { Channel, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";
import type { Snapshot } from "../../ipc";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

// Asks the host to replay its compiled-in fixture over a channel. The webview passes nothing but the
// channel: no path, no command, no provider. Outside the Tauri shell there is no data source at all,
// and the hook says so instead of inventing one.
export function useFixtureReplay() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const replay = useCallback(async () => {
    if (!inTauri) {
      setError("This page is not running inside the Tauri shell, so there is no data source.");
      return;
    }
    setError(null);
    setSnap(null);
    const ch = new Channel<Snapshot>();
    ch.onmessage = setSnap;
    try {
      await invoke("replay_fixture", { onSnapshot: ch });
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void replay();
  }, [replay]);
  return { snap, error, replay };
}
