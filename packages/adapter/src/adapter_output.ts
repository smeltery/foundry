import { ProtocolError, MAX_FRAME_BYTES } from "./protocol.ts";
export class AdapterOutput {
  private tail = Promise.resolve();
  private queued = 0;
  constructor(private readonly write: (frame: Uint8Array) => Promise<void>) {}
  send(frame: Uint8Array): Promise<void> {
    if (this.queued >= 32)
      return Promise.reject(new ProtocolError("output_backpressure"));
    if (frame.length > MAX_FRAME_BYTES + 1)
      return Promise.reject(new ProtocolError("frame_too_large"));
    this.queued++;
    const next = this.tail.then(() => this.write(frame));
    this.tail = next.catch(() => {});
    return next.finally(() => {
      this.queued--;
    });
  }
  drain(): Promise<void> {
    return this.tail;
  }
}
