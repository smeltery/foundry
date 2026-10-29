import type { Facade } from "./facade.ts";
import type { ContextFacade } from "./context.ts";

export interface AdapterOptions {
  input: AsyncIterable<Uint8Array>;
  write(frame: Uint8Array): Promise<void>;
  facade?: Facade | ContextFacade;
  signal?: AbortSignal;
  shutdownTimeoutMs?: number;
}

export type AdapterResult = {
  status: "completed" | "cancelled" | "failed";
  code?: string;
};
