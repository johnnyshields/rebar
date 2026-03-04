/** Identifies a process within a Rebar runtime. */
export class Pid {
  constructor(
    public readonly nodeId: bigint,
    public readonly localId: bigint,
  ) {}

  toString(): string {
    return `<${this.nodeId}.${this.localId}>`;
  }
}
