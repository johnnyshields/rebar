# Go Actor Patterns Guide

This guide explains the design decisions behind the Rebar Go client and demonstrates idiomatic patterns for building actors in Go. Where Erlang/BEAM uses processes, pattern matching, and OTP behaviors, Go achieves the same goals through interfaces, structs, and explicit error handling.

---

## Table of Contents

1. [Why a Single-Method Interface](#1-why-a-single-method-interface)
2. [The Context Pattern](#2-the-context-pattern)
3. [Error Handling](#3-error-handling)
4. [Memory Management](#4-memory-management)
5. [Pattern: Ping-Pong Actors](#5-pattern-ping-pong-actors)
6. [Pattern: Named Services](#6-pattern-named-services)
7. [Pattern: Message Dispatch](#7-pattern-message-dispatch)

---

## 1. Why a Single-Method Interface

The `Actor` interface has exactly one method:

```go
type Actor interface {
    HandleMessage(ctx *Context, msg *Msg)
}
```

This follows a core Go design principle: **interfaces should be small**. The Go standard library reinforces this throughout -- `io.Reader` has one method, `io.Writer` has one method, `fmt.Stringer` has one method. Small interfaces are easy to implement, easy to compose, and easy to test with mocks.

In Erlang/OTP, a `gen_server` module implements multiple callbacks (`init/1`, `handle_call/3`, `handle_cast/2`, `handle_info/2`, `terminate/2`). Each callback handles a different lifecycle phase. The Go client collapses this into a single entry point because:

- **Go has no pattern matching.** Erlang dispatches messages by pattern matching on the message shape. Go uses type switches, string comparisons, or protocol buffers. The single `HandleMessage` method is the natural place for this dispatch logic.
- **Startup is a message.** When a process starts, `HandleMessage` is called with `msg == nil`. This removes the need for a separate `Init` callback. The actor can register itself, initialize state, or do nothing -- it is the actor's choice.
- **Any struct is an actor.** Adding a single method to a struct makes it an actor. There is no framework inheritance, no required base type, and no mandatory boilerplate.

```go
// Minimal actor -- a struct with one method.
type Pinger struct{}

func (p *Pinger) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg != nil {
        fmt.Printf("pinger got: %s\n", string(msg.Data()))
    }
}
```

---

## 2. The Context Pattern

Every call to `HandleMessage` receives a `*Context` as its first argument. Context provides the process's identity (`Self()`) and access to messaging (`Send`, `SendNamed`) and the registry (`Register`, `Whereis`).

This is **dependency injection, not global state**. The actor never imports a global runtime variable or calls package-level functions to send messages. Everything it needs arrives through the context.

### Why This Matters

- **Testability.** You can construct a `Context` with a mock runtime for unit testing without starting a real Rebar runtime.
- **Isolation.** Each process gets its own context. There is no shared mutable state between actors unless you explicitly introduce it.
- **Clarity.** Reading the `HandleMessage` signature tells you exactly what capabilities the actor has access to.

### Context vs. Runtime

The `Context` methods mirror `Runtime` methods exactly. The distinction is about scope:

| Operation    | Via Runtime                        | Via Context                        |
|--------------|------------------------------------|------------------------------------|
| Send         | `rt.Send(dest, data)`              | `ctx.Send(dest, data)`             |
| Register     | `rt.Register(name, pid)`           | `ctx.Register(name, pid)`          |
| Whereis      | `rt.Whereis(name)`                 | `ctx.Whereis(name)`                |
| SendNamed    | `rt.SendNamed(name, data)`         | `ctx.SendNamed(name, data)`        |
| Self         | --                                 | `ctx.Self()`                       |

Use `Runtime` methods from main or setup code. Use `Context` methods from inside `HandleMessage`. Both route to the same underlying FFI calls.

---

## 3. Error Handling

The Go client follows Go conventions: errors are returned as values, not thrown as exceptions. There are no panics in the public API (the one exception is null pointer errors, which indicate bugs in the bindings themselves and should never occur in correct usage).

### Checking Error Types

All errors implement the `error` interface and can be inspected with `errors.As`:

```go
err := rt.Send(pid, []byte("hello"))
if err != nil {
    var sendErr *rebar.SendError
    var notFound *rebar.NotFoundError
    var invalidName *rebar.InvalidNameError

    switch {
    case errors.As(err, &sendErr):
        // Process does not exist or mailbox is closed.
        log.Printf("delivery failed: %v", sendErr)
    case errors.As(err, &notFound):
        // Name not in registry (only from Whereis/SendNamed).
        log.Printf("name not found: %v", notFound)
    case errors.As(err, &invalidName):
        // Name was not valid UTF-8.
        log.Printf("bad name: %v", invalidName)
    default:
        log.Printf("unexpected error: %v", err)
    }
}
```

### Error Hierarchy

All error types embed `RebarError`, which carries a numeric `Code` and a `Message` string:

```
error (interface)
  RebarError          -- base type, Code + Message
    SendError         -- message delivery failed (code -2)
    NotFoundError     -- name not in registry (code -3)
    InvalidNameError  -- name not valid UTF-8 (code -4)
```

### Inside HandleMessage

Since `HandleMessage` has no return value, actors handle errors inline:

```go
func (a *MyActor) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        return
    }
    err := ctx.Send(a.target, msg.Data())
    if err != nil {
        // Log, retry, drop -- the actor decides.
        log.Printf("forward failed: %v", err)
    }
}
```

This is a deliberate design choice. In Erlang, a process that crashes is restarted by its supervisor. In the Go client, the actor is responsible for its own error handling. Panicking inside `HandleMessage` would crash the entire Go program, not just the actor, so errors should always be handled explicitly.

---

## 4. Memory Management

### Runtime: Use `defer`

The runtime allocates C memory under the hood. The idiomatic cleanup pattern is:

```go
rt, err := rebar.NewRuntime(1)
if err != nil {
    log.Fatal(err)
}
defer rt.Close()
```

`Close` is idempotent. Calling it twice does nothing. This makes it safe to use in deferred calls even if the runtime is closed early for another reason.

### Messages: Automatic via GC

`Msg` values are pure Go objects. They hold a `[]byte` slice and are garbage-collected normally. There is no manual free step.

When you call `Send` or `SendNamed` with a `[]byte`, the Go client internally creates a C-side message, sends it, and frees the C message in the same function call (via `defer`). Your byte slice is never modified.

### Actors: Held by the Runtime

When you call `SpawnActor`, the actor value is stored in an internal Go map. It is not garbage-collected as long as the runtime is alive. This means:

- Actor struct fields persist across message deliveries.
- You can use the actor struct as stateful storage.
- The actor is released when the runtime is closed.

```go
type Counter struct {
    count int
}

func (c *Counter) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        return
    }
    c.count++
    fmt.Printf("count: %d\n", c.count)
}
```

---

## 5. Pattern: Ping-Pong Actors

Two actors exchanging messages through the registry demonstrate named messaging, startup initialization, and cross-actor communication.

```go
package main

import (
    "fmt"
    "log"
    "time"

    "github.com/alexandernicholson/rebar/clients/go/rebar"
)

// Pinger sends a "ping" to the ponger on startup.
type Pinger struct{}

func (p *Pinger) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        // Startup: register ourselves so Ponger can reply.
        ctx.Register("pinger", ctx.Self())
        // Send the first ping.
        err := ctx.SendNamed("ponger", []byte("ping"))
        if err != nil {
            log.Printf("pinger: failed to send: %v", err)
        }
        return
    }
    fmt.Printf("pinger received: %s\n", string(msg.Data()))
}

// Ponger waits for a "ping" and replies with "pong".
type Ponger struct{}

func (p *Ponger) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        // Startup: register ourselves so Pinger can find us.
        ctx.Register("ponger", ctx.Self())
        return
    }
    fmt.Printf("ponger received: %s\n", string(msg.Data()))
    // Reply to the pinger.
    err := ctx.SendNamed("pinger", []byte("pong"))
    if err != nil {
        log.Printf("ponger: failed to reply: %v", err)
    }
}

func main() {
    rt, err := rebar.NewRuntime(1)
    if err != nil {
        log.Fatal(err)
    }
    defer rt.Close()

    // Spawn ponger first so its name is registered before pinger starts.
    if _, err := rt.SpawnActor(&Ponger{}); err != nil {
        log.Fatal(err)
    }
    if _, err := rt.SpawnActor(&Pinger{}); err != nil {
        log.Fatal(err)
    }

    // Let the async processes run.
    time.Sleep(100 * time.Millisecond)
}
```

**Key points:**

- Ponger is spawned first so that its name is registered before Pinger tries to send.
- Both actors register themselves during the startup call (`msg == nil`).
- Communication uses `SendNamed` so actors do not need to know each other's PIDs directly.
- Error handling is explicit -- failed sends are logged, not ignored.

---

## 6. Pattern: Named Services

Named services are actors registered under well-known names that other parts of the system can discover via `Whereis` or send to via `SendNamed`. This is analogous to Erlang's registered processes.

```go
package main

import (
    "fmt"
    "log"
    "time"

    "github.com/alexandernicholson/rebar/clients/go/rebar"
)

// KVStore is a simple in-memory key-value store actor.
type KVStore struct {
    data map[string]string
}

func (kv *KVStore) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        kv.data = make(map[string]string)
        ctx.Register("kv", ctx.Self())
        fmt.Println("kv store ready")
        return
    }
    payload := string(msg.Data())
    fmt.Printf("kv store received: %s\n", payload)
    // In a real implementation, parse the payload as a command
    // (e.g., "SET key value" or "GET key") and respond accordingly.
}

func main() {
    rt, err := rebar.NewRuntime(1)
    if err != nil {
        log.Fatal(err)
    }
    defer rt.Close()

    // Start the service.
    if _, err := rt.SpawnActor(&KVStore{}); err != nil {
        log.Fatal(err)
    }

    // Any part of the system can now discover the service.
    pid, err := rt.Whereis("kv")
    if err != nil {
        log.Fatalf("kv not found: %v", err)
    }
    fmt.Printf("kv store is at %d:%d\n", pid.NodeID, pid.LocalID)

    // Send a command by name -- no PID needed.
    if err := rt.SendNamed("kv", []byte("SET greeting hello")); err != nil {
        log.Printf("send failed: %v", err)
    }

    time.Sleep(100 * time.Millisecond)
}
```

**Key points:**

- The service registers itself during startup. Callers use `Whereis` or `SendNamed` to interact with it.
- State (the `data` map) lives in the actor struct. It is initialized in the startup handler, not in the constructor. This keeps the struct creation simple and separates allocation from initialization.
- `SendNamed` avoids the need to pass PIDs around, making the system loosely coupled.

---

## 7. Pattern: Message Dispatch

Since Go does not have pattern matching, actors that handle multiple message types need an explicit dispatch mechanism. The simplest approach is a prefix or tag byte:

```go
type Router struct{}

func (r *Router) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        ctx.Register("router", ctx.Self())
        return
    }
    data := msg.Data()
    if len(data) == 0 {
        return
    }

    // First byte is the message type tag.
    tag := data[0]
    payload := data[1:]

    switch tag {
    case 0x01:
        r.handleCreate(ctx, payload)
    case 0x02:
        r.handleUpdate(ctx, payload)
    case 0x03:
        r.handleDelete(ctx, payload)
    default:
        log.Printf("unknown message type: 0x%02x", tag)
    }
}

func (r *Router) handleCreate(ctx *rebar.Context, data []byte) {
    fmt.Printf("create: %s\n", string(data))
}

func (r *Router) handleUpdate(ctx *rebar.Context, data []byte) {
    fmt.Printf("update: %s\n", string(data))
}

func (r *Router) handleDelete(ctx *rebar.Context, data []byte) {
    fmt.Printf("delete: %s\n", string(data))
}
```

For richer message formats, consider encoding messages with JSON, Protocol Buffers, or MessagePack. The actor's `HandleMessage` deserializes the payload and dispatches based on the decoded type:

```go
import "encoding/json"

type Command struct {
    Action string `json:"action"`
    Key    string `json:"key"`
    Value  string `json:"value,omitempty"`
}

type JSONActor struct{}

func (a *JSONActor) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        return
    }
    var cmd Command
    if err := json.Unmarshal(msg.Data(), &cmd); err != nil {
        log.Printf("invalid message: %v", err)
        return
    }
    switch cmd.Action {
    case "get":
        fmt.Printf("get %s\n", cmd.Key)
    case "set":
        fmt.Printf("set %s = %s\n", cmd.Key, cmd.Value)
    }
}
```

This explicit dispatch is more verbose than Erlang's pattern matching, but it is straightforward, debuggable, and uses familiar Go patterns.

---

## See Also

- [Go Client API Reference](../api/client-go.md) -- complete type and method documentation
- [rebar-ffi C-ABI Reference](../api/rebar-ffi.md) -- the underlying C API
- [Extending Rebar](../extending.md) -- building FFI bindings for new languages
