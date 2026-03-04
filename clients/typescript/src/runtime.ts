import { checkError } from "./errors.ts";
import { lib } from "./ffi.ts";
import { Pid } from "./types.ts";
import type { Actor } from "./actor.ts";

/** Passed to Actor.handleMessage. Provides messaging capabilities. */
export class Context {
  constructor(
    private readonly _pid: Pid,
    private readonly _runtime: Runtime,
  ) {}

  /** Return this process's PID. */
  self(): Pid {
    return this._pid;
  }

  /** Send a message to another process. */
  send(dest: Pid, data: Uint8Array): void {
    this._runtime.send(dest, data);
  }

  /** Receive a message from this process's mailbox. */
  recv(timeoutMs: number | bigint = -1n): Uint8Array | null {
    return this._runtime.recv(this._pid, timeoutMs);
  }

  /** Register a name for a PID. */
  register(name: string, pid: Pid): void {
    this._runtime.register(name, pid);
  }

  /** Look up a PID by name. */
  whereis(name: string): Pid {
    return this._runtime.whereis(name);
  }

  /** Send a message to a named process. */
  sendNamed(name: string, data: Uint8Array): void {
    this._runtime.sendNamed(name, data);
  }

  /** Unregister a name. */
  unregister(name: string): void {
    this._runtime.unregister(name);
  }
}

// deno-lint-ignore no-explicit-any
type AnyCallback = Deno.UnsafeCallback<any>;

/** Manages a Rebar actor runtime. Implements Disposable for `using`. */
export class Runtime implements Disposable {
  private ptr: Deno.PointerValue;
  private callbacks: AnyCallback[] = [];

  constructor(nodeId: number | bigint = 1n) {
    this.ptr = lib.symbols.rebar_runtime_new(BigInt(nodeId));
    if (this.ptr === null) {
      throw new Error("failed to create Rebar runtime");
    }
  }

  /** Free the runtime. Safe to call multiple times. */
  close(): void {
    if (this.ptr !== null) {
      lib.symbols.rebar_runtime_free(this.ptr);
      this.ptr = null;
      for (const cb of this.callbacks) {
        cb.close();
      }
      this.callbacks = [];
    }
  }

  [Symbol.dispose](): void {
    this.close();
  }

  /** Send a message to a process by PID. */
  send(dest: Pid, data: Uint8Array): void {
    const msg = lib.symbols.rebar_msg_create(
      data as BufferSource,
      BigInt(data.byteLength),
    );
    try {
      const pidBuf = new BigUint64Array([BigInt(dest.nodeId), BigInt(dest.localId)]);
      const rc = lib.symbols.rebar_send(
        this.ptr,
        pidBuf as unknown as BufferSource,
        msg,
      );
      checkError(rc);
    } finally {
      lib.symbols.rebar_msg_free(msg);
    }
  }

  /** Receive a message from a process's mailbox.
   * Returns null if timed out. */
  recv(pid: Pid, timeoutMs: number | bigint = -1n): Uint8Array | null {
    const pidBuf = new BigUint64Array([BigInt(pid.nodeId), BigInt(pid.localId)]);
    const msgOutBuf = new BigUint64Array(1); // pointer-sized buffer
    const rc = lib.symbols.rebar_recv(
      this.ptr,
      pidBuf as unknown as BufferSource,
      msgOutBuf as unknown as BufferSource,
      BigInt(timeoutMs),
    );
    if (rc === -5) {
      return null; // timeout
    }
    checkError(rc);
    const msgPtr = Deno.UnsafePointer.create(msgOutBuf[0]);
    try {
      const dataPtr = lib.symbols.rebar_msg_data(msgPtr);
      const length = lib.symbols.rebar_msg_len(msgPtr);
      if (dataPtr === null || length === 0n) {
        return new Uint8Array(0);
      }
      const view = new Deno.UnsafePointerView(dataPtr);
      return new Uint8Array(view.getArrayBuffer(Number(length)));
    } finally {
      lib.symbols.rebar_msg_free(msgPtr);
    }
  }

  /** Stop a process and remove it from the process table. */
  stopProcess(pid: Pid): void {
    const pidBuf = new BigUint64Array([BigInt(pid.nodeId), BigInt(pid.localId)]);
    const rc = lib.symbols.rebar_stop_process(
      this.ptr,
      pidBuf as unknown as BufferSource,
    );
    checkError(rc);
  }

  /** Register a name for a PID. */
  register(name: string, pid: Pid): void {
    const nameBytes = new TextEncoder().encode(name);
    const pidBuf = new BigUint64Array([BigInt(pid.nodeId), BigInt(pid.localId)]);
    const rc = lib.symbols.rebar_register(
      this.ptr,
      nameBytes as BufferSource,
      BigInt(nameBytes.byteLength),
      pidBuf as unknown as BufferSource,
    );
    checkError(rc);
  }

  /** Look up a PID by name. */
  whereis(name: string): Pid {
    const nameBytes = new TextEncoder().encode(name);
    const pidBuf = new BigUint64Array(2);
    const rc = lib.symbols.rebar_whereis(
      this.ptr,
      nameBytes as BufferSource,
      BigInt(nameBytes.byteLength),
      pidBuf as unknown as BufferSource,
    );
    checkError(rc);
    return new Pid(pidBuf[0], pidBuf[1]);
  }

  /** Send a message to a named process. */
  sendNamed(name: string, data: Uint8Array): void {
    const nameBytes = new TextEncoder().encode(name);
    const msg = lib.symbols.rebar_msg_create(
      data as BufferSource,
      BigInt(data.byteLength),
    );
    try {
      const rc = lib.symbols.rebar_send_named(
        this.ptr,
        nameBytes as BufferSource,
        BigInt(nameBytes.byteLength),
        msg,
      );
      checkError(rc);
    } finally {
      lib.symbols.rebar_msg_free(msg);
    }
  }

  /** Unregister a name. */
  unregister(name: string): void {
    const nameBytes = new TextEncoder().encode(name);
    const rc = lib.symbols.rebar_unregister(
      this.ptr,
      nameBytes as BufferSource,
      BigInt(nameBytes.byteLength),
    );
    checkError(rc);
  }

  /** Spawn a new process backed by the given Actor. */
  spawnActor(actor: Actor): Pid {
    const pidBuf = new BigUint64Array(2);
    const runtimeRef = this;

    const callback = new Deno.UnsafeCallback(
      {
        parameters: [{ struct: ["u64", "u64"] }],
        result: "void",
      } as const,
      // deno-lint-ignore no-explicit-any
      (pidStruct: any) => {
        let nodeId: bigint;
        let localId: bigint;
        if (pidStruct instanceof BigUint64Array) {
          nodeId = pidStruct[0];
          localId = pidStruct[1];
        } else if (pidStruct instanceof Uint8Array) {
          const view = new DataView(pidStruct.buffer);
          nodeId = view.getBigUint64(0, true);
          localId = view.getBigUint64(8, true);
        } else {
          nodeId = BigInt(pidStruct[0]);
          localId = BigInt(pidStruct[1]);
        }
        const pid = new Pid(nodeId, localId);
        const ctx = new Context(pid, runtimeRef);

        // Init callback with null message.
        actor.handleMessage(ctx, null);

        // Start async message polling loop.
        (async () => {
          try {
            while (true) {
              const data = runtimeRef.recv(pid, 100n);
              if (data === null) {
                // Timeout - yield and retry
                await new Promise((r) => setTimeout(r, 0));
                continue;
              }
              actor.handleMessage(ctx, data);
            }
          } catch {
            // Process stopped or error — exit loop.
          }
        })();
      },
    );
    this.callbacks.push(callback);

    const rc = lib.symbols.rebar_spawn(
      this.ptr,
      callback.pointer,
      pidBuf as unknown as BufferSource,
    );
    checkError(rc);
    return new Pid(pidBuf[0], pidBuf[1]);
  }
}
