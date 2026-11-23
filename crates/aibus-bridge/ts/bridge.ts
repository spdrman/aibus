// The hand-written half of the bridge (#306): typed calls and the subscription cursor. The DTOs
// come from ../bindings, generated from the Rust. `invoke` is injected so this module does not
// depend on Tauri; the Tauri command that forwards a request to `aibus_bridge::handle_json` lands
// with the desktop shell (#307).
import type { BridgeError } from "../bindings/BridgeError";
import type { EventBatch } from "../bindings/EventBatch";
import type { ProjectionDto } from "../bindings/ProjectionDto";
import type { Reply } from "../bindings/Reply";
import type { Request } from "../bindings/Request";
import type { Response } from "../bindings/Response";
import type { RunDto } from "../bindings/RunDto";

export const BRIDGE_SCHEMA = 1;

export type Invoke = (requestJson: string) => Promise<string>;

export class BridgeFailure extends Error {
  constructor(readonly error: BridgeError) {
    super(`${error.code}: ${error.message}`);
  }
}

async function call(invoke: Invoke, command: Request["command"]): Promise<Reply> {
  const request: Request = { schema_version: BRIDGE_SCHEMA, command };
  const response: Response = JSON.parse(await invoke(JSON.stringify(request)));
  if (response.status === "error") throw new BridgeFailure(response.error);
  return response.reply;
}

export async function listRuns(invoke: Invoke): Promise<RunDto[]> {
  const reply = await call(invoke, { cmd: "list_runs" });
  if (reply.kind !== "runs") throw new Error(`unexpected reply ${reply.kind}`);
  return reply.runs;
}

export async function loadProjection(invoke: Invoke, run: string): Promise<ProjectionDto> {
  const reply = await call(invoke, { cmd: "projection", run });
  if (reply.kind !== "projection") throw new Error(`unexpected reply ${reply.kind}`);
  return reply.projection;
}

/** Mirrors `aibus_bridge::Subscriber`: a batch applies only when it continues from the cursor. */
export class Subscriber {
  constructor(readonly run: string, public cursor: string | null = null) {}

  async fetch(invoke: Invoke, limit: number | null = null): Promise<EventBatch> {
    const reply = await call(invoke, { cmd: "events", run: this.run, after: this.cursor, limit });
    if (reply.kind !== "events") throw new Error(`unexpected reply ${reply.kind}`);
    return reply.batch;
  }

  /** Returns false on a gap (resubscribe by calling `fetch` again from the unchanged cursor). */
  apply(batch: EventBatch): boolean {
    const ids = batch.events.map((e) => e.event_id);
    const ascending = ids.every((id, i) => i === 0 || ids[i - 1] < id);
    const last = ids.length ? ids[ids.length - 1] : batch.after;
    const ok =
      batch.schema_version === BRIDGE_SCHEMA &&
      batch.run === this.run &&
      batch.after === this.cursor &&
      ascending &&
      batch.next === last &&
      (ids.length === 0 || this.cursor === null || ids[0] > this.cursor);
    if (ok) this.cursor = batch.next;
    return ok;
  }

  resync(projection: ProjectionDto): void {
    this.cursor = projection.head;
  }
}
