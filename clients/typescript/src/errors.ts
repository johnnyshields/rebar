/** Base error for all Rebar operations. */
export class RebarError extends Error {
  constructor(public readonly code: number, message: string) {
    super(`rebar error ${code}: ${message}`);
    this.name = "RebarError";
  }
}

/** Raised when a message cannot be delivered. */
export class SendError extends RebarError {
  constructor() {
    super(-2, "failed to deliver message");
    this.name = "SendError";
  }
}

/** Raised when a name is not found in the registry. */
export class NotFoundError extends RebarError {
  constructor() {
    super(-3, "name not found in registry");
    this.name = "NotFoundError";
  }
}

/** Raised when a name is not valid UTF-8. */
export class TimeoutError extends RebarError {
  constructor() {
    super(-5, "recv timed out");
    this.name = "TimeoutError";
  }
}

export class InvalidNameError extends RebarError {
  constructor() {
    super(-4, "name is not valid UTF-8");
    this.name = "InvalidNameError";
  }
}

export function checkError(rc: number): void {
  switch (rc) {
    case 0:
      return;
    case -1:
      throw new Error("rebar: internal error — null pointer passed to FFI");
    case -2:
      throw new SendError();
    case -3:
      throw new NotFoundError();
    case -5:
      throw new TimeoutError();
    case -4:
      throw new InvalidNameError();
    default:
      throw new RebarError(rc, "unknown error");
  }
}
