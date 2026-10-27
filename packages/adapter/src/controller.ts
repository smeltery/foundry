import { isAbsolute } from "node:path";
import type { Facade, RunContext } from "./facade.ts";
import {
  commands,
  encode,
  json,
  PROTOCOL_VERSION,
  MESSAGE_ACK_CAPABILITY,
  ProtocolError,
  type Command,
  type Event,
  type Json,
} from "./protocol.ts";

export interface AdapterOptions {
  input: AsyncIterable<Uint8Array>;
  write(frame: Uint8Array): Promise<void>;
  facade?: Facade;
  shutdownTimeoutMs?: number;
}

export type AdapterResult = { status: "completed" | "cancelled" | "failed"; code?: string };
type PendingTool = { resolve(value: Json): void; reject(error: Error): void };

class Controller {
  readonly done: Promise<AdapterResult>;
  private resolveDone!: (result: AdapterResult) => void;
  private readonly abort = new AbortController();
  private attempt?: string;
  private ended = false;
  private cancelled = false;
  private cancelTimer?: ReturnType<typeof setTimeout>;
  private writes = Promise.resolve();
  private queuedWrites = 0;
  private tools = new Map<string, PendingTool>();
  private calls = new Set<string>();
  private messages = new Map<string, { text: string; receipt: string; accepted: boolean }>();
  private steering = Promise.resolve();

  constructor(
    private readonly options: AdapterOptions,
    private readonly timeout: number,
  ) {
    this.done = new Promise((resolve) => {
      this.resolveDone = resolve;
    });
  }

  private write(event: Event): Promise<void> {
    if (this.queuedWrites >= 32) return Promise.reject(new ProtocolError("output_backpressure"));
    let frame: Uint8Array;
    try {
      frame = encode(event);
    } catch (error) {
      return Promise.reject(error);
    }
    this.queuedWrites++;
    const next = this.writes.then(() => this.options.write(frame));
    this.writes = next.catch(() => {});
    return next.finally(() => {
      this.queuedWrites--;
    });
  }

  async emit(event: Event): Promise<void> {
    if (this.ended || this.cancelled) return;
    try {
      await this.write(event);
    } catch (error) {
      void this.fail(error instanceof ProtocolError ? error.code : "output_failed");
      throw new ProtocolError("output_failed");
    }
  }

  private rejectTools(): void {
    for (const pending of this.tools.values()) pending.reject(new ProtocolError("adapter_stopped"));
    this.tools.clear();
  }

  private async finish(result: AdapterResult, event?: Event): Promise<void> {
    if (this.ended) return;
    this.ended = true;
    clearTimeout(this.cancelTimer);
    this.abort.abort();
    this.rejectTools();
    let timer: ReturnType<typeof setTimeout> | undefined;
    // An unresponsive output consumer must not defeat bounded cancellation.
    const deadline = new Promise<void>((_resolve, reject) => {
      timer = setTimeout(() => reject(new ProtocolError("shutdown_timeout")), this.timeout);
    });
    try {
      const cleanup = async () => {
        if (event) await this.write(event);
        else await this.writes;
        await this.options.facade?.dispose?.();
      };
      await Promise.race([cleanup(), deadline]);
    } catch {
      if (result.status !== "cancelled")
        result = { status: "failed", code: "adapter_shutdown_failed" };
    } finally {
      clearTimeout(timer);
      this.resolveDone(result);
    }
  }

  async fail(code: string): Promise<void> {
    if (this.cancelled) return this.finish({ status: "cancelled" });
    await this.finish(
      { status: "failed", code },
      this.attempt
        ? {
            type: "failed",
            attempt_id: this.attempt,
            code,
            message: "Adapter operation failed.",
          }
        : undefined,
    );
  }

  private context(): RunContext {
    return {
      signal: this.abort.signal,
      text: (text) => this.emit({ type: "text", attempt_id: this.attempt!, text }),
      tool: async (callId, name, input) => {
        if (this.ended || this.cancelled) throw new ProtocolError("adapter_stopped");
        if (
          !callId.trim() ||
          !name.trim() ||
          this.calls.has(callId) ||
          this.calls.size >= 1024 ||
          this.tools.size >= 64
        ) {
          void this.fail("invalid_tool_request");
          throw new ProtocolError("invalid_tool_request");
        }
        const value = json(input);
        this.calls.add(callId);
        const reply = new Promise<Json>((resolve, reject) => {
          this.tools.set(callId, { resolve, reject });
        });
        // Attach immediately, including while output is backpressured.
        void reply.catch(() => {});
        try {
          await this.emit({
            type: "tool_call",
            attempt_id: this.attempt!,
            call_id: callId,
            name,
            input: value,
          });
        } catch (error) {
          this.tools.delete(callId);
          throw error;
        }
        return reply;
      },
    };
  }

  accept(command: Command): void {
    if (this.ended) return;
    if (command.type === "start") {
      if (this.attempt) throw new ProtocolError("duplicate_start");
      this.attempt = command.attempt_id;
      if (command.version !== PROTOCOL_VERSION) throw new ProtocolError("unsupported_version");
      if (command.session.active_attempt_id !== this.attempt)
        throw new ProtocolError("stale_attempt");
      if (!isAbsolute(command.session.directory)) throw new ProtocolError("invalid_directory");
      if (!this.options.facade) {
        void this.fail("adapter_not_configured");
        return;
      }
      const run = async () => {
        try {
          const completion = await this.options.facade!.run(command.session, this.context());
          if (this.cancelled) {
            await this.finish({ status: "cancelled" });
            return;
          }
          if (this.tools.size) {
            await this.fail("pending_tool_calls");
            return;
          }
          if ([...this.messages.values()].some((message) => !message.accepted)) {
            await this.fail("unaccepted_messages");
            return;
          }
          if (
            completion &&
            (!["success", "failure", "action_required"].includes(completion.outcome) ||
              typeof completion.result !== "string")
          ) {
            await this.fail("invalid_completion");
            return;
          }
          await this.finish(
            { status: "completed" },
            {
              type: "settled",
              attempt_id: this.attempt!,
              outcome: completion?.outcome ?? "action_required",
              result: completion?.result ?? "",
            },
          );
        } catch {
          await this.fail("adapter_failed");
        }
      };
      void run();
      return;
    }
    if (!this.attempt) throw new ProtocolError("start_required");
    if (command.attempt_id !== this.attempt) throw new ProtocolError("stale_attempt");
    if (command.type === "cancel") {
      this.cancelled = true;
      this.abort.abort();
      this.rejectTools();
      if (!this.cancelTimer)
        this.cancelTimer = setTimeout(() => {
          void this.finish({ status: "cancelled" });
        }, this.timeout);
      void Promise.resolve()
        .then(() => this.options.facade?.cancel?.(command.mode))
        .catch(() => {});
      if (command.mode === "hard") void this.finish({ status: "cancelled" });
      return;
    }
    if (this.cancelled) return;
    if (command.type === "tool_result") {
      const pending = this.tools.get(command.call_id);
      if (!pending) throw new ProtocolError("unknown_tool_result");
      this.tools.delete(command.call_id);
      pending.resolve(command.result);
      return;
    }
    if (!this.options.facade?.steer) throw new ProtocolError("steering_unsupported");
    const previous = this.messages.get(command.message_id);
    if (previous !== undefined) {
      if (previous.text !== command.text || previous.receipt !== command.receipt)
        throw new ProtocolError("message_conflict");
      if (previous.accepted)
        void this.emit({
          type: "message_accepted",
          attempt_id: this.attempt,
          message_id: command.message_id,
          receipt: command.receipt,
        }).catch(() => {});
      return;
    }
    if (this.messages.size >= 1024) throw new ProtocolError("message_limit");
    const delivery = { text: command.text, receipt: command.receipt, accepted: false };
    this.messages.set(command.message_id, delivery);
    this.steering = this.steering
      .then(async () => {
        if (this.ended || this.cancelled) return;
        await this.options.facade!.steer!(command.message_id, command.text, this.abort.signal);
        if (this.ended || this.cancelled) return;
        delivery.accepted = true;
        await this.emit({
          type: "message_accepted",
          attempt_id: this.attempt!,
          message_id: command.message_id,
          receipt: command.receipt,
        });
      })
      .catch(() => {
        void this.fail("steering_failed");
      });
  }

  async read(): Promise<void> {
    try {
      await this.emit({
        type: "ready",
        version: PROTOCOL_VERSION,
        capabilities: this.options.facade?.steer ? [MESSAGE_ACK_CAPABILITY] : [],
      });
      for await (const command of commands(this.options.input)) {
        if (this.ended) return;
        this.accept(command);
      }
      if (!this.ended) await this.fail("input_closed");
    } catch (error) {
      await this.fail(error instanceof ProtocolError ? error.code : "input_failed");
    }
  }
}

/** No credentials or tools are discovered. One process invocation owns one attempt.
 * The caller must close its input transport after this resolves to interrupt reads.
 */
export async function runAdapter(options: AdapterOptions): Promise<AdapterResult> {
  const timeout = options.shutdownTimeoutMs ?? 1000;
  if (!Number.isFinite(timeout) || timeout < 1 || timeout > 60_000)
    throw new ProtocolError("invalid_shutdown_timeout");
  const controller = new Controller(options, timeout);
  void controller.read();
  return controller.done;
}
