import { ProtocolError } from "./protocol.ts";

/** Cancellation and final cleanup share one deadline, including graceful stop. */
export class AdapterShutdown {
  private deadline?: number;
  constructor(private readonly timeout: number) {}

  begin(): void {
    this.deadline ??= performance.now() + this.timeout;
  }

  remaining(): number {
    this.begin();
    return Math.max(0, this.deadline! - performance.now());
  }

  async finish(
    output: () => Promise<void>,
    dispose: () => Promise<void> | undefined,
    cancellation: Promise<void> | undefined,
    outputAfterDispose: boolean,
  ): Promise<void> {
    this.begin();
    const disposing = Promise.resolve().then(async () => {
      const outcomes = await Promise.allSettled([
        cancellation ?? Promise.resolve(),
        Promise.resolve().then(dispose),
      ]);
      if (outcomes.some((value) => value.status === "rejected")) {
        throw new ProtocolError("adapter_shutdown_failed");
      }
    });
    // Convert synchronous encoders and callbacks into owned rejections too.
    const writing = Promise.resolve().then(() =>
      outputAfterDispose ? disposing.then(output) : output(),
    );
    const cleanup = Promise.allSettled([writing, disposing]).then(
      (outcomes) => {
        if (outcomes.some((value) => value.status === "rejected")) {
          throw new ProtocolError("adapter_shutdown_failed");
        }
      },
    );
    await this.wait(cleanup);
  }

  async wait(cleanup: Promise<void>): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_resolve, reject) => {
      timer = setTimeout(
        () => reject(new ProtocolError("shutdown_timeout")),
        this.remaining(),
      );
    });
    try {
      await Promise.race([cleanup, deadline]);
    } finally {
      clearTimeout(timer);
    }
  }
}
