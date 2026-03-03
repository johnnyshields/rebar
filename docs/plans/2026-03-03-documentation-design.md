# Documentation Design

## Audience
Both open-source community (adopters + contributors) and internal reference.

## Approach
In-repo `docs/` directory with GitHub-rendered Markdown. Mermaid diagrams (using `<br>` not `\n` for line breaks in labels). No external tooling required.

## Document Structure

```
README.md                              # Rewrite: overview, quick start, benchmarks
docs/
├── architecture.md                    # Full system architecture with diagrams
├── getting-started.md                 # Developer guide: first actor through supervision
├── extending.md                       # Extension guide: transports, supervisors, FFI
├── api/
│   ├── rebar-core.md                  # Process model, Runtime, Supervisor API
│   ├── rebar-cluster.md               # SWIM, transport, registry API
│   └── rebar-ffi.md                   # C-ABI reference for polyglot integration
├── internals/
│   ├── wire-protocol.md               # Frame format, MessagePack encoding
│   ├── swim-protocol.md               # SWIM gossip implementation
│   ├── crdt-registry.md               # OR-Set CRDT conflict resolution
│   └── supervisor-engine.md           # Restart strategies, limiting, shutdown
└── benchmarks.md                      # Methodology, results, analysis
```

## README.md Content
- Hero description + "Why Rebar?"
- Architecture overview diagram (Mermaid)
- Quick start code example
- Benchmark highlights table
- Feature checklist
- Links into docs/

## Architecture Doc
Diagrams for: crate dependency graph, process lifecycle, message flow (local vs remote), SWIM state machine, supervisor tree strategies, OR-Set CRDT flow, wire protocol frame layout.

## Getting Started Guide
Progressive: add dep → create Runtime → spawn process → send messages → request-reply → supervisors → process-per-key pattern.

## Extension Guide
Custom transports (trait impl), custom supervisor strategies, FFI for new languages, custom serialization.

## API Reference
Per-crate, every public type/trait/function, grouped by module, with code examples.

## Internals
Deep dives: wire protocol binary format, SWIM protocol details, CRDT conflict resolution rules, supervisor restart algorithm.

## Benchmarks Doc
Methodology, Docker Compose topology, resource limits, scenarios, full results tables, analysis.
