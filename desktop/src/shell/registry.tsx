import type { ReactNode } from "react";

// A workspace is one entry in a list. The shell renders whatever the list holds, so adding one (the
// Replay and Compare workspace of #314 is next) is one entry here and no change to the shell.
export interface Workspace {
  id: string;
  label: string;
  render: () => ReactNode;
}
