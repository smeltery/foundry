import type { CancelMode } from "./protocol.ts";
import type { Facade, RunContext, Completion } from "./facade.ts";

export type ContextId = string;
export type ContextReceipt = string;
export type ContextDigest = string;
export interface ContextFormat {
  readonly profile: ContextId;
  readonly format_version: ContextId;
  readonly producer_build: ContextId;
}
export interface InputCoverage {
  readonly input_id: ContextId;
  readonly input_digest: ContextDigest;
  readonly native_ids: readonly ContextId[];
}
export interface ResultCoverage {
  readonly attempt_id: ContextId;
  readonly call_id: ContextId;
  readonly result_digest: ContextDigest;
  readonly native_ids: readonly ContextId[];
}
export interface ContextCoverage {
  readonly completed_turn_id: ContextId;
  readonly inputs: readonly InputCoverage[];
  readonly results: readonly ResultCoverage[];
}
export interface CaptureBeginRequest {
  readonly attempt_id: ContextId;
  readonly capture_id: ContextId;
  readonly format_digest: ContextDigest;
  readonly configuration_digest: ContextDigest;
  readonly challenge: ContextReceipt;
}
export interface ExportRequest {
  readonly attempt_id: ContextId;
  readonly capture_id: ContextId;
  readonly format_digest: ContextDigest;
  readonly cut_digest: ContextDigest;
  readonly challenge: ContextReceipt;
}
export interface LoadingScope {
  readonly restore_id: ContextId;
  readonly load_id: ContextReceipt;
}
export interface RestoreDescriptor {
  readonly capture_id: ContextId;
  readonly checkpoint_id: ContextId;
  readonly record_digest: ContextDigest;
  readonly witness_digest: ContextDigest;
  readonly format: ContextFormat;
  readonly configuration_digest: ContextDigest;
  readonly payload_length: number;
  readonly payload_digest: ContextDigest;
  readonly retention_binding: ContextDigest;
}
export interface RestoreBeginRequest {
  readonly scope: LoadingScope;
  readonly descriptor: RestoreDescriptor;
}
export interface RestorePayloadRequest {
  readonly scope: LoadingScope;
  readonly begin_binding: ContextDigest;
  readonly payload_length: number;
  readonly payload_digest: ContextDigest;
  readonly receipt: ContextReceipt;
  readonly bytes: Uint8Array;
}
export interface ObservationBinding {
  readonly scope: LoadingScope;
  readonly job_id: ContextReceipt;
  readonly begin_binding: ContextDigest;
  readonly profile_digest: ContextDigest;
}
export interface ObserveRequest {
  readonly binding: ObservationBinding;
  readonly challenge: ContextReceipt;
}
export interface ContinuationIdentity {
  readonly attempt_id: ContextId;
  readonly continuation_id: ContextId;
  readonly input_id: ContextId;
  readonly input_sequence: number;
  readonly input_digest: ContextDigest;
  readonly input_bytes: number;
  readonly authorization_binding: ContextDigest;
}
export interface ContinueRequest {
  readonly scope: LoadingScope;
  readonly continuation: ContinuationIdentity;
  readonly text: string;
  readonly receipt: ContextReceipt;
}
export interface NativeObservation {
  readonly instance_id: ContextReceipt;
  readonly bytes: Uint8Array;
}
export interface NativeCandidate {
  readonly format: ContextFormat;
  readonly coverage: ContextCoverage;
  readonly bytes: Uint8Array;
}
export interface CaptureHooks {
  prepare(request: CaptureBeginRequest, signal: AbortSignal): Promise<void>;
  export(request: ExportRequest, signal: AbortSignal): Promise<NativeCandidate>;
}
export interface PreparedContinuation {
  run(context: RunContext): Promise<Completion>;
}
export interface ContinuationHooks {
  prepare(
    request: ContinueRequest,
    signal: AbortSignal,
  ): Promise<PreparedContinuation>;
}
export interface RestoreHooks {
  begin(request: RestoreBeginRequest, signal: AbortSignal): Promise<void>;
  load(request: RestorePayloadRequest, signal: AbortSignal): Promise<void>;
  observe(
    request: ObserveRequest,
    signal: AbortSignal,
  ): Promise<NativeObservation>;
  readonly continuation?: ContinuationHooks;
}
export interface ContextLifecycle {
  readonly capture?: CaptureHooks;
  readonly restore?: RestoreHooks;
}
/** All hooks share this facade's actual retained owner. No transport or authority is injected. */
export interface ContextFacade extends Facade {
  readonly lifecycle: ContextLifecycle;
  cancel(mode: CancelMode): Promise<void>;
  dispose(): Promise<void>;
}
export type ContextCommand =
  | {
      readonly type: "context_capture_begin";
      readonly request: CaptureBeginRequest;
    }
  | { readonly type: "context_export"; readonly request: ExportRequest }
  | {
      readonly type: "context_release";
      readonly request: {
        readonly attempt_id: string;
        readonly operation_id: string;
      };
    }
  | { readonly type: "restore_begin_v2"; readonly request: RestoreBeginRequest }
  | {
      readonly type: "restore_payload_v2";
      readonly request: RestorePayloadRequest;
    }
  | { readonly type: "context_observe_v1"; readonly request: ObserveRequest }
  | { readonly type: "continue_context_v2"; readonly request: ContinueRequest }
  | {
      readonly type: "context_failed";
      readonly request: {
        readonly attempt_id: string;
        readonly operation_id: string;
        readonly code: string;
      };
    };
