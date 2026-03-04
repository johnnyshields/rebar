import type { Context } from "./runtime.ts";

/**
 * Base class for Rebar actors. Extend and implement handleMessage.
 *
 * @example
 * ```typescript
 * class Greeter extends Actor {
 *   handleMessage(ctx: Context, msg: Uint8Array | null): void {
 *     console.log(`Hello from ${ctx.self()}`);
 *   }
 * }
 * ```
 */
export abstract class Actor {
  /**
   * Called when this actor receives a message.
   * @param ctx - The process context
   * @param msg - The message payload, or null on startup
   */
  abstract handleMessage(ctx: Context, msg: Uint8Array | null): void;
}
