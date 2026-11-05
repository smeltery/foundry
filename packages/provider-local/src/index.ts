export { createLocalFacade, type LocalFacadeOptions } from "./facade.ts";
export { LocalEndpoint } from "./endpoint.ts";
export type { Completion, CompletionRequest, Message, ToolCall, ToolSpec } from "./chat.ts";
export {
  EndpointError,
  type ErrorClass,
  type FailureCategory,
  type InvalidField,
} from "./errors.ts";
export { MAX_REPLY_BYTES } from "./reply.ts";
export { classify, type NetworkClass, type OptIn, type Resolve } from "./network.ts";
export { DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, type EndpointSettings } from "./settings.ts";
