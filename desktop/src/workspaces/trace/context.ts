import { createContext, useContext } from "react";
import type { Snapshot } from "../../ipc";

// What every trace view reads: the latest snapshot and the shared selection (a node or a timeline
// row's agent), so clicking in one view highlights the same thing in the others.
export interface TraceState {
  snap: Snapshot | null;
  selected: string | null;
  select: (id: string | null) => void;
}

export const TraceContext = createContext<TraceState>({ snap: null, selected: null, select: () => {} });
export const useTrace = () => useContext(TraceContext);
