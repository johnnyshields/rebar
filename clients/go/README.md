# Rebar Go Client

Idiomatic Go package for the Rebar actor runtime. Wraps the `librebar_ffi` C shared library via cgo.

## Prerequisites

- Go 1.21+
- Rust toolchain (to build `librebar_ffi`)

## Quick Start

### 1. Build the shared library

```bash
cd /path/to/rebar
cargo build --release -p rebar-ffi
```

### 2. Set library path

```bash
export LD_LIBRARY_PATH="/path/to/rebar/target/release:$LD_LIBRARY_PATH"
# macOS: use DYLD_LIBRARY_PATH instead
```

### 3. Use in your Go project

```bash
go get github.com/alexandernicholson/rebar/clients/go
```

### 4. Hello World

```go
package main

import (
    "fmt"
    "github.com/alexandernicholson/rebar/clients/go/rebar"
)

type Greeter struct{}

func (g *Greeter) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    fmt.Printf("Hello from process %d:%d\n", ctx.Self().NodeID, ctx.Self().LocalID)
}

func main() {
    rt, err := rebar.NewRuntime(1)
    if err != nil {
        panic(err)
    }
    defer rt.Close()

    pid, err := rt.SpawnActor(&Greeter{})
    if err != nil {
        panic(err)
    }
    fmt.Printf("Spawned: %d:%d\n", pid.NodeID, pid.LocalID)
}
```

### 5. Build with cgo

```bash
CGO_LDFLAGS="-L/path/to/rebar/target/release" go build .
```

## API Reference

See [Go Client API Reference](../../docs/api/client-go.md).

## Actor Patterns

See [Go Actor Patterns Guide](../../docs/guides/actors-go.md).
