import type { AdapterOptions, AdapterResult } from "./adapter_options.ts";
export type { AdapterOptions, AdapterResult } from "./adapter_options.ts";
import { Lifecycle } from "./lifecycle.ts";
import { incoming } from "./adapter_input.ts";
import { AdapterShutdown } from "./adapter_shutdown.ts";
import { AdapterOutput } from "./adapter_output.ts";
import { isAbsolute } from "node:path";
import type { RunContext, Completion } from "./facade.ts";
import {
  encode,
  json,
  PROTOCOL_VERSION,
  MESSAGE_ACK_CAPABILITY,
  ProtocolError,
  type Command,
  type Event,
  type Json,
} from "./protocol.ts";

type PendingTool = { resolve(value: Json): void; reject(error: Error): void };

class Controller {
  readonly done: Promise<AdapterResult>;
  private resolveDone!: (result: AdapterResult) => void;
  private readonly abort = new AbortController();
  private attempt?: string;
  private ended = false;
  private cancelled = false;
  private cancelTimer?: ReturnType<typeof setTimeout>;
  private readonly output: AdapterOutput;
  private readonly shutdown: AdapterShutdown;
  private lifecycle!: Lifecycle;
  private task?: Promise<Completion | void>;
  private cancellation?: Promise<void>;
  private terminalSent = false;
  private readonly stop = () => {
    this.cancel("hard");
  };
  private tools = new Map<string, PendingTool>();
  private calls = new Set<string>();
  private messages = new Map<
    string,
    { text: string; receipt: string; accepted: boolean }
  >();
  private steering = Promise.resolve();

  constructor(
    private readonly options: AdapterOptions,
    timeout: number,
  ) {
    this.output = new AdapterOutput(options.write);
    this.shutdown = new AdapterShutdown(timeout);
    this.done = new Promise((resolve) => {
      this.resolveDone = resolve;
    });
  }

  private write(event: Event): Promise<void> {
    return this.output.send(encode(event));
  }

  async emit(event: Event): Promise<void> {
    if (this.ended || this.cancelled) return;
    try {
      await this.write(event);
    } catch (error) {
      void this.fail(
        error instanceof ProtocolError ? error.code : "output_failed",
      );
      throw new ProtocolError("output_failed");
    }
  }

  private rejectTools(): void {
    for (const pending of this.tools.values())
      pending.reject(new ProtocolError("adapter_stopped"));
    this.tools.clear();
  }

  private async finish(
    result: AdapterResult,
    event?: Event,
    frame?: Uint8Array,
  ): Promise<void> {
    if (this.ended) return;
    this.ended = true;
    clearTimeout(this.cancelTimer);
    this.abort.abort();
    this.lifecycle?.stop();
    this.rejectTools();
    if (result.status !== "completed") this.requestCancel("hard");
    this.shutdown.begin();
    try {
      await this.shutdown.finish(
        () =>
          frame
            ? this.output.send(frame)
            : event
              ? this.write(event)
              : this.output.drain(),
        () => this.options.facade?.dispose?.(),
        this.cancellation,
        frame !== undefined,
      );
    } catch {
      if (result.status !== "cancelled" || this.lifecycle?.enabled)
        result = { status: "failed", code: "adapter_shutdown_failed" };
    } finally {
      this.options.signal?.removeEventListener("abort", this.stop);
      this.resolveDone(result);
    }
  }

  async fail(code: string): Promise<void> {
    if (this.cancelled) return this.finish({ status: "cancelled" });
    await this.finish(
      { status: "failed", code },
      this.attempt && !this.terminalSent
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
      text: (text) => {
        if (
          this.ended ||
          this.cancelled ||
          this.abort.signal.aborted ||
          !this.lifecycle.active
        )
          return Promise.reject(new ProtocolError("adapter_stopped"));
        return this.emit({ type: "text", attempt_id: this.attempt!, text });
      },
      tool: async (callId, name, input) => {
        if (this.ended || this.cancelled || !this.lifecycle.active)
          throw new ProtocolError("adapter_stopped");
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
      if (!this.lifecycle.enabled) this.attempt = command.attempt_id;
      if (command.version !== PROTOCOL_VERSION)
        throw new ProtocolError("unsupported_version");
      if (command.session.active_attempt_id !== command.attempt_id)
        throw new ProtocolError("stale_attempt");
      if (!isAbsolute(command.session.directory))
        throw new ProtocolError("invalid_directory");
      if (!this.options.facade) {
        void this.fail("adapter_not_configured");
        return;
      }
      const retained = this.lifecycle.start(command.attempt_id);
      this.execute(
        command.attempt_id,
        (context) => this.options.facade!.run(command.session, context),
        retained,
      );
      return;
    }
    if (!this.attempt) throw new ProtocolError("start_required");
    if (command.attempt_id !== this.attempt)
      throw new ProtocolError("stale_attempt");
    if (command.type === "cancel") {
      this.cancel(command.mode);
      return;
    }
    if (this.cancelled) return;
    if (!this.lifecycle.active) throw new ProtocolError("adapter_stopped");
    if (command.type === "tool_result") {
      const pending = this.tools.get(command.call_id);
      if (!pending) throw new ProtocolError("unknown_tool_result");
      this.tools.delete(command.call_id);
      pending.resolve(command.result);
      return;
    }
    if (!this.options.facade?.steer)
      throw new ProtocolError("steering_unsupported");
    const previous = this.messages.get(command.message_id);
    if (previous !== undefined) {
      if (
        previous.text !== command.text ||
        previous.receipt !== command.receipt
      )
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
    const delivery = {
      text: command.text,
      receipt: command.receipt,
      accepted: false,
    };
    this.messages.set(command.message_id, delivery);
    this.steering = this.steering
      .then(async () => {
        if (this.ended || this.cancelled) return;
        await this.options.facade!.steer!(
          command.message_id,
          command.text,
          this.abort.signal,
        );
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

  private cancel(mode: "graceful" | "hard"): void {
    if (this.ended) return;
    this.cancelled = true;
    this.abort.abort();
    this.rejectTools();
    this.requestCancel(mode);
    this.shutdown.begin();
    if (!this.cancelTimer)
      this.cancelTimer = setTimeout(() => {
        void this.finish({ status: "cancelled" });
      }, this.shutdown.remaining());
    if (mode === "hard" || !this.lifecycle?.active)
      void this.finish({ status: "cancelled" });
  }

  private requestCancel(mode: "graceful" | "hard"): void {
    if (!this.cancellation) {
      this.cancellation = Promise.resolve().then(() =>
        this.options.facade?.cancel?.(mode),
      );
      void this.cancellation.catch(() => {});
    }
  }

  private execute(
    attempt: string,
    run: (context: RunContext) => Promise<Completion | void>,
    retained: boolean,
  ): void {
    this.attempt = attempt;
    this.task = Promise.resolve().then(() => {
      if (this.abort.signal.aborted) throw new ProtocolError("adapter_stopped");
      return run(this.context());
    });
    void this.task
      .then(async (completion) => {
        if (this.ended) return;
        if (this.cancelled) return this.finish({ status: "cancelled" });
        if (this.tools.size) return this.fail("pending_tool_calls");
        if ([...this.messages.values()].some((message) => !message.accepted))
          return this.fail("unaccepted_messages");
        if (
          completion &&
          (!["success", "failure", "action_required"].includes(
            completion.outcome,
          ) ||
            typeof completion.result !== "string")
        )
          return this.fail("invalid_completion");
        this.lifecycle.settling();
        const event: Event = {
          type: "settled",
          attempt_id: attempt,
          outcome: completion?.outcome ?? "action_required",
          result: completion?.result ?? "",
        };
        this.terminalSent = true;
        if (retained) {
          await this.write(event);
          if (!this.ended && !this.cancelled) this.lifecycle.settled();
        } else await this.finish({ status: "completed" }, event);
      })
      .catch(() => this.fail("adapter_failed"));
  }

  async read(): Promise<void> {
    try {
      this.lifecycle = new Lifecycle(this.options.facade, {
        signal: this.abort.signal,
        write: (frame) =>
          this.output.send(frame).catch(() => {
            throw new ProtocolError("output_failed");
          }),
        execute: (attempt, run, retained) =>
          this.execute(attempt, run, retained),
        close: (code, frame) =>
          this.finish(
            code ? { status: "failed", code } : { status: "completed" },
            undefined,
            frame,
          ),
      });
      this.options.signal?.addEventListener("abort", this.stop, { once: true });
      if (this.options.signal?.aborted) {
        this.stop();
        return;
      }
      await this.emit({
        type: "ready",
        version: PROTOCOL_VERSION,
        capabilities: [
          ...(this.options.facade?.steer ? [MESSAGE_ACK_CAPABILITY] : []),
          ...this.lifecycle.capabilities,
        ],
      });
      for await (const item of incoming(
        this.options.input,
        this.lifecycle.enabled,
      )) {
        if (this.ended) return;
        if (item.kind === "ordinary") this.accept(item.command);
        else this.lifecycle.accept(item.command);
      }
      if (!this.ended) await this.fail("input_closed");
    } catch (error) {
      await this.fail(
        error instanceof ProtocolError ? error.code : "input_failed",
      );
    }
  }
}

/** No credentials or tools are discovered. One process invocation owns one attempt.
 * The caller must close its input transport after this resolves to interrupt reads.
 */
export async function runAdapter(
  options: AdapterOptions,
): Promise<AdapterResult> {
  const timeout = options.shutdownTimeoutMs ?? 1000;
  if (!Number.isFinite(timeout) || timeout < 1 || timeout > 60_000)
    throw new ProtocolError("invalid_shutdown_timeout");
  const controller = new Controller(options, timeout);
  void controller.read();
  return controller.done;
}
