import type { Facade, RunContext, Completion } from "./facade.ts";
import type {
  ContextFacade,
  ContextLifecycle,
  ContextCommand,
  CaptureBeginRequest,
  RestoreBeginRequest,
} from "./context.ts";
import { check, restoreBinding, privateFrame } from "./context_validation.ts";
import { candidate, observed } from "./context_codec.ts";
import { ProtocolError } from "./protocol.ts";

export interface LifecycleHost {
  readonly signal: AbortSignal;
  write(frame: Uint8Array): Promise<void>;
  execute(
    attempt: string,
    run: (context: RunContext) => Promise<Completion>,
    retained: boolean,
  ): void;
  close(code?: string, frame?: Uint8Array): Promise<void>;
}
type Phase =
  | "ready"
  | "busy"
  | "start"
  | "payload"
  | "observe"
  | "held"
  | "running"
  | "settling"
  | "settled"
  | "exported"
  | "closed";
function hooks(facade?: Facade | ContextFacade): ContextLifecycle | undefined {
  if (!facade || !("lifecycle" in facade)) return undefined;
  check(
    typeof facade.run === "function" &&
      typeof facade.cancel === "function" &&
      typeof facade.dispose === "function",
  );
  const lifecycle = facade.lifecycle;
  check(
    lifecycle &&
      typeof lifecycle === "object" &&
      (lifecycle.capture || lifecycle.restore),
  );
  const capture = lifecycle.capture;
  const restore = lifecycle.restore;
  if (capture)
    check(
      typeof capture.prepare === "function" &&
        typeof capture.export === "function",
    );
  if (restore) {
    check(
      typeof restore.begin === "function" &&
        typeof restore.load === "function" &&
        typeof restore.observe === "function",
    );
    if (restore.continuation)
      check(typeof restore.continuation.prepare === "function");
  }
  return {
    capture: capture && {
      prepare: capture.prepare.bind(capture),
      export: capture.export.bind(capture),
    },
    restore: restore && {
      begin: restore.begin.bind(restore),
      load: restore.load.bind(restore),
      observe: restore.observe.bind(restore),
      continuation: restore.continuation && {
        prepare: restore.continuation.prepare.bind(restore.continuation),
      },
    },
  };
}
export class Lifecycle {
  private phase: Phase = "ready";
  private readonly hooks: ContextLifecycle | undefined;
  private capture?: CaptureBeginRequest;
  private loading?: RestoreBeginRequest;
  private binding?: string;
  private attempt?: string;
  private exported?: string;
  private pending?: Promise<void>;
  constructor(
    facade: Facade | ContextFacade | undefined,
    private readonly host: LifecycleHost,
  ) {
    this.hooks = hooks(facade);
  }
  get enabled(): boolean {
    return this.hooks !== undefined;
  }
  get active(): boolean {
    return this.phase === "running";
  }
  get terminal(): boolean {
    return ["settling", "settled", "exported"].includes(this.phase);
  }
  get capabilities(): string[] {
    const result: string[] = [];
    if (this.hooks?.capture)
      result.push("context_capture_v1", "context_export_v1");
    if (this.hooks?.restore)
      result.push("context_restore_v2", "context_observe_v1");
    if (this.hooks?.restore?.continuation) result.push("context_continue_v2");
    return result;
  }
  start(attempt: string): boolean {
    check(
      this.phase === "ready" || this.phase === "start",
      "invalid_context_state",
    );
    if (this.capture) check(this.capture.attempt_id === attempt);
    this.attempt = attempt;
    this.phase = "running";
    return !!this.capture;
  }
  settling(): void {
    check(this.active, "invalid_context_state");
    this.phase = "settling";
  }
  settled(): void {
    this.live();
    this.phase = "settled";
  }
  stop(): void {
    this.phase = "closed";
  }
  private live(): void {
    check(
      !this.host.signal.aborted && this.phase !== "closed",
      "adapter_stopped",
    );
  }
  private scope(received: { restore_id: string; load_id: string }): void {
    check(
      this.loading &&
        received.restore_id === this.loading.scope.restore_id &&
        received.load_id === this.loading.scope.load_id,
    );
  }
  private launch(
    work: () => Promise<void>,
    failure?: (error: unknown) => Uint8Array,
  ): void {
    this.phase = "busy";
    this.pending = Promise.resolve()
      .then(() => {
        this.live();
        return work();
      })
      .catch(async (error) => {
        if (this.host.signal.aborted || this.phase === "closed") return;
        await this.host.close(
          "context_operation_failed",
          error instanceof ProtocolError && error.code === "output_failed"
            ? undefined
            : failure?.(error),
        );
      });
  }
  accept(command: ContextCommand): void {
    try {
      this.dispatch(command);
    } catch (error) {
      if (
        command.type !== "context_export" ||
        !this.enabled ||
        this.host.signal.aborted
      )
        throw error;
      this.phase = "closed";
      this.pending = this.host.close(
        "context_operation_failed",
        privateFrame({
          type: "context_failed",
          attempt_id: command.request.attempt_id,
          operation_id: command.request.capture_id,
          code: "invalid_state",
        }),
      );
    }
  }
  private dispatch(command: ContextCommand): void {
    this.live();
    check(this.enabled, "context_unsupported");
    check(this.phase !== "busy", "invalid_context_state");
    switch (command.type) {
      case "context_capture_begin": {
        const r = command.request;
        check(
          this.phase === "ready" && this.hooks?.capture,
          "invalid_context_state",
        );
        this.capture = r;
        this.launch(async () => {
          await this.hooks!.capture!.prepare(r, this.host.signal);
          this.live();
          await this.host.write(
            privateFrame({ type: "context_capture_ready", ...r }),
          );
          this.live();
          this.phase = "start";
        });
        return;
      }
      case "restore_begin_v2": {
        const r = command.request;
        check(
          this.phase === "ready" && this.hooks?.restore,
          "invalid_context_state",
        );
        this.loading = r;
        this.binding = restoreBinding(r.scope, r.descriptor);
        this.launch(async () => {
          await this.hooks!.restore!.begin(r, this.host.signal);
          this.live();
          await this.host.write(
            privateFrame({
              type: "restore_prepared_v2",
              scope: r.scope,
              begin_binding: this.binding,
            }),
          );
          this.live();
          this.phase = "payload";
        });
        return;
      }
      case "restore_payload_v2": {
        const r = command.request;
        check(this.phase === "payload", "invalid_context_state");
        this.scope(r.scope);
        check(
          r.begin_binding === this.binding &&
            r.payload_length === this.loading!.descriptor.payload_length &&
            r.payload_digest === this.loading!.descriptor.payload_digest,
        );
        this.launch(async () => {
          await this.hooks!.restore!.load(r, this.host.signal);
          this.live();
          await this.host.write(
            privateFrame({
              type: "context_restored_v2",
              scope: r.scope,
              begin_binding: r.begin_binding,
              payload_digest: r.payload_digest,
              receipt: r.receipt,
            }),
          );
          this.live();
          this.phase = "observe";
        });
        return;
      }
      case "context_observe_v1": {
        const r = command.request;
        const permitted = this.phase === "observe";
        this.launch(
          async () => {
            check(permitted, "invalid_context_state");
            this.scope(r.binding.scope);
            check(r.binding.begin_binding === this.binding);
            const result = await this.hooks!.restore!.observe(
              r,
              this.host.signal,
            );
            this.live();
            await this.host.write(observed(r, result));
            this.live();
            this.phase = "held";
          },
          (error) =>
            privateFrame({
              type: "context_observation_rejected_v1",
              binding: r.binding,
              code:
                error instanceof ProtocolError && error.code === "context_limit"
                  ? "limit"
                  : error instanceof ProtocolError &&
                      error.code === "invalid_context_state"
                    ? "invalid_state"
                    : "validation",
              challenge: r.challenge,
            }),
        );
        return;
      }
      case "continue_context_v2": {
        const r = command.request;
        check(
          this.phase === "held" && this.hooks?.restore?.continuation,
          "invalid_context_state",
        );
        this.scope(r.scope);
        this.launch(async () => {
          const task = await this.hooks!.restore!.continuation!.prepare(
            r,
            this.host.signal,
          );
          this.live();
          check(task && typeof task.run === "function");
          const run = task.run.bind(task);
          await this.host.write(
            privateFrame({
              type: "continuation_accepted_v2",
              scope: r.scope,
              continuation: r.continuation,
              receipt: r.receipt,
            }),
          );
          this.live();
          this.attempt = r.continuation.attempt_id;
          this.phase = "running";
          this.host.execute(this.attempt, run, !!this.hooks?.capture);
        });
        return;
      }
      case "context_export": {
        const r = command.request;
        check(
          this.phase === "settled" && this.hooks?.capture,
          "invalid_context_state",
        );
        check(r.attempt_id === this.attempt);
        if (this.capture)
          check(
            r.capture_id === this.capture.capture_id &&
              r.format_digest === this.capture.format_digest,
          );
        this.launch(
          async () => {
            const result = await this.hooks!.capture!.export(
              r,
              this.host.signal,
            );
            this.live();
            await this.host.write(candidate(r, result));
            this.live();
            this.exported = r.capture_id;
            this.phase = "exported";
          },
          () =>
            privateFrame({
              type: "context_failed",
              attempt_id: r.attempt_id,
              operation_id: r.capture_id,
              code: "validation",
            }),
        );
        return;
      }
      case "context_release": {
        const r = command.request;
        check(
          this.phase === "exported" &&
            r.attempt_id === this.attempt &&
            r.operation_id === this.exported,
          "invalid_context_state",
        );
        this.phase = "closed";
        this.pending = this.host.close();
        return;
      }
      case "context_failed": {
        const r = command.request;
        check(
          r.attempt_id === (this.attempt ?? this.capture?.attempt_id) &&
            r.operation_id === (this.exported ?? this.capture?.capture_id),
          "invalid_context_state",
        );
        this.phase = "closed";
        this.pending = this.host.close("context_peer_failed");
      }
    }
  }
}
