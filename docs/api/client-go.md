# Go Client — API Reference

The `rebar` package provides an idiomatic Go interface to the Rebar actor runtime. It wraps the `librebar_ffi` C shared library via cgo, translating C-level error codes and opaque pointers into Go structs, interfaces, and `error` values.

The package is organized around four concepts: a **Runtime** that manages the actor system, **Pid** values that identify processes, **Msg** values that carry data between processes, and an **Actor** interface that defines process behavior.

---

## Types

### Pid

A process identifier composed of two 64-bit unsigned integers.

```go
type Pid struct {
    NodeID  uint64
    LocalID uint64
}
```

- `NodeID` identifies the runtime node.
- `LocalID` identifies the process within that node.

`Pid` is a plain value type. It is safe to copy, compare, and pass by value.

---

### Msg

A message wrapping a byte slice. Messages are created with `NewMsg` and carry arbitrary payloads between processes.

```go
type Msg struct {
    // unexported fields
}
```

#### `NewMsg`

Create a new message from a byte slice. The data is copied -- the caller retains ownership of the original slice.

```go
func NewMsg(data []byte) *Msg
```

**Parameters:**

| Name   | Type     | Description             |
|--------|----------|-------------------------|
| `data` | `[]byte` | Payload bytes to copy   |

**Returns:** A new `*Msg` containing a copy of `data`.

**Example:**

```go
msg := rebar.NewMsg([]byte("hello"))
```

---

#### `(*Msg) Data`

Returns the message payload as a byte slice.

```go
func (m *Msg) Data() []byte
```

**Returns:** The raw byte payload.

**Example:**

```go
msg := rebar.NewMsg([]byte("hello"))
fmt.Println(string(msg.Data())) // "hello"
```

---

### Actor

The interface that all Rebar actors must implement. It contains a single method, following the Go convention of small, focused interfaces.

```go
type Actor interface {
    HandleMessage(ctx *Context, msg *Msg)
}
```

- `HandleMessage` is called when the process starts (with `msg == nil` as a lifecycle hook) and each time the process receives a message.
- The `ctx` parameter provides the process's identity and messaging capabilities.
- Implementations should not retain references to `ctx` beyond the lifetime of the call.

**Example:**

```go
type Logger struct{}

func (l *Logger) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        fmt.Printf("logger started as %d:%d\n", ctx.Self().NodeID, ctx.Self().LocalID)
        return
    }
    fmt.Printf("log: %s\n", string(msg.Data()))
}
```

---

### Context

Passed to `Actor.HandleMessage` to provide the process's identity and access to the runtime's messaging and registry operations. Context methods mirror the corresponding `Runtime` methods.

```go
type Context struct {
    // unexported fields
}
```

#### `(*Context) Self`

Returns this process's PID.

```go
func (c *Context) Self() Pid
```

**Returns:** The `Pid` of the current process.

---

#### `(*Context) Send`

Sends a message to another process by PID.

```go
func (c *Context) Send(dest Pid, data []byte) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `dest` | `Pid`    | PID of the destination process       |
| `data` | `[]byte` | Payload bytes to send                |

**Returns:** `nil` on success, or an error (see [Errors](#errors)).

**Errors:**
- `*SendError` if the destination process does not exist or its mailbox is closed.

---

#### `(*Context) Register`

Associates a name with a PID in the runtime's registry.

```go
func (c *Context) Register(name string, pid Pid) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Name to register                     |
| `pid`  | `Pid`    | PID to associate with the name       |

**Returns:** `nil` on success, or an error.

**Errors:**
- `*InvalidNameError` if the name contains invalid UTF-8 (unlikely in Go, but possible via unsafe conversions).

**Notes:**
- If the name is already registered, the PID is silently overwritten.

---

#### `(*Context) Whereis`

Looks up a PID by its registered name.

```go
func (c *Context) Whereis(name string) (Pid, error)
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Name to look up                      |

**Returns:** The associated `Pid` on success, or a zero `Pid` and an error.

**Errors:**
- `*NotFoundError` if no PID is registered under this name.
- `*InvalidNameError` if the name is not valid UTF-8.

---

#### `(*Context) SendNamed`

Sends a message to a process by its registered name. Combines a registry lookup and a send in a single call.

```go
func (c *Context) SendNamed(name string, data []byte) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Registered name of the target        |
| `data` | `[]byte` | Payload bytes to send                |

**Returns:** `nil` on success, or an error.

**Errors:**
- `*NotFoundError` if no PID is registered under this name.
- `*InvalidNameError` if the name is not valid UTF-8.
- `*SendError` if the resolved process is unreachable.

---

## Runtime

The central type that manages the Rebar actor system. A `Runtime` wraps an opaque C pointer to the `rebar_runtime_t` structure, which contains a Tokio async runtime, the Rebar process scheduler, and a local name registry.

```go
type Runtime struct {
    // unexported fields
}
```

---

### `NewRuntime`

Creates a new Rebar runtime for the given node ID.

```go
func NewRuntime(nodeID uint64) (*Runtime, error)
```

**Parameters:**

| Name     | Type     | Description                              |
|----------|----------|------------------------------------------|
| `nodeID` | `uint64` | Unique identifier for this runtime node  |

**Returns:** A `*Runtime` on success, or `nil` and a `*RebarError` if the internal Tokio runtime fails to initialize.

**Notes:**
- The caller must call `Close()` when the runtime is no longer needed.
- Multiple runtimes can coexist with different `nodeID` values.
- Each runtime maintains its own independent name registry.

**Example:**

```go
rt, err := rebar.NewRuntime(1)
if err != nil {
    log.Fatalf("failed to create runtime: %v", err)
}
defer rt.Close()
```

---

### `(*Runtime) Close`

Frees the runtime and stops all spawned processes. Safe to call multiple times -- subsequent calls are no-ops.

```go
func (r *Runtime) Close()
```

**Notes:**
- All spawned processes are stopped when the runtime is freed.
- The name registry is destroyed along with the runtime.
- Idiomatic usage is `defer rt.Close()` immediately after `NewRuntime`.

---

### `(*Runtime) Send`

Sends a message to a process identified by PID.

```go
func (r *Runtime) Send(dest Pid, data []byte) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `dest` | `Pid`    | PID of the destination process       |
| `data` | `[]byte` | Payload bytes to send                |

**Returns:** `nil` on success, or an error.

**Errors:**
- `*SendError` if the destination process does not exist or its mailbox is closed.

**Notes:**
- The data is copied into Rust-owned memory. The caller retains ownership of the byte slice.
- This call blocks until the send completes.

**Example:**

```go
err := rt.Send(targetPid, []byte("ping"))
if err != nil {
    var sendErr *rebar.SendError
    if errors.As(err, &sendErr) {
        log.Printf("process unreachable: %v", sendErr)
    }
}
```

---

### `(*Runtime) Register`

Associates a name with a PID in the runtime's local name registry.

```go
func (r *Runtime) Register(name string, pid Pid) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Name to register                     |
| `pid`  | `Pid`    | PID to associate with the name       |

**Returns:** `nil` on success, or an error.

**Errors:**
- `*InvalidNameError` if the name bytes are not valid UTF-8.

**Notes:**
- If the name is already registered, the PID is silently overwritten.
- The name is borrowed for the duration of the FFI call; Go retains ownership of the string.

**Example:**

```go
pid, _ := rt.SpawnActor(&MyActor{})
err := rt.Register("my_service", pid)
if err != nil {
    log.Fatalf("register failed: %v", err)
}
```

---

### `(*Runtime) Whereis`

Looks up a PID by its registered name.

```go
func (r *Runtime) Whereis(name string) (Pid, error)
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Name to look up                      |

**Returns:** The associated `Pid` on success, or a zero-value `Pid` and an error.

**Errors:**
- `*NotFoundError` if no PID is registered under this name.
- `*InvalidNameError` if the name is not valid UTF-8.

**Example:**

```go
pid, err := rt.Whereis("my_service")
if err != nil {
    var notFound *rebar.NotFoundError
    if errors.As(err, &notFound) {
        log.Println("service not registered yet")
    }
}
```

---

### `(*Runtime) SendNamed`

Sends a message to a process by its registered name. Combines a registry lookup and a send in a single call.

```go
func (r *Runtime) SendNamed(name string, data []byte) error
```

**Parameters:**

| Name   | Type     | Description                          |
|--------|----------|--------------------------------------|
| `name` | `string` | Registered name of the target        |
| `data` | `[]byte` | Payload bytes to send                |

**Returns:** `nil` on success, or an error.

**Errors:**
- `*NotFoundError` if no PID is registered under this name.
- `*InvalidNameError` if the name is not valid UTF-8.
- `*SendError` if the resolved process is unreachable.

**Notes:**
- The registry lock is held only for the lookup; the send happens after the lock is released.
- The caller retains ownership of `data`.

**Example:**

```go
err := rt.SendNamed("worker", []byte("work-item"))
if err != nil {
    log.Printf("failed to send: %v", err)
}
```

---

### `(*Runtime) SpawnActor`

Spawns a new process backed by the given `Actor`. The actor's `HandleMessage` method is called with a `nil` message on startup as a lifecycle hook, and the process PID is returned.

```go
func (r *Runtime) SpawnActor(actor Actor) (Pid, error)
```

**Parameters:**

| Name    | Type    | Description                          |
|---------|---------|--------------------------------------|
| `actor` | `Actor` | Actor implementation for the process |

**Returns:** The `Pid` of the newly spawned process, or a zero-value `Pid` and an error.

**Notes:**
- This call blocks until the process is spawned.
- The actor is registered internally and looked up by the C callback trampoline.
- The startup call (`msg == nil`) executes synchronously during spawn.

**Example:**

```go
type Counter struct {
    count int
}

func (c *Counter) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        // Startup: register ourselves
        ctx.Register("counter", ctx.Self())
        return
    }
    c.count++
    fmt.Printf("count: %d\n", c.count)
}

pid, err := rt.SpawnActor(&Counter{})
if err != nil {
    log.Fatalf("spawn failed: %v", err)
}
fmt.Printf("spawned counter at %d:%d\n", pid.NodeID, pid.LocalID)
```

---

## Errors

All errors returned by the Go client are concrete types that implement the `error` interface. They can be inspected with `errors.As` or type assertions.

### RebarError

The base error type for all Rebar FFI errors.

```go
type RebarError struct {
    Code    int
    Message string
}

func (e *RebarError) Error() string
```

- `Code` is the integer error code from the FFI layer.
- `Message` is a human-readable description.

---

### SendError

Returned when a message cannot be delivered to a process. Embeds `RebarError`.

```go
type SendError struct {
    RebarError
}
```

- Returned by `Send`, `SendNamed`, `Context.Send`, and `Context.SendNamed`.
- Typically means the destination process does not exist or its mailbox is closed.
- FFI error code: `-2` (`REBAR_ERR_SEND_FAILED`).

**Example:**

```go
err := rt.Send(pid, []byte("hello"))
if err != nil {
    var sendErr *rebar.SendError
    if errors.As(err, &sendErr) {
        fmt.Println("message delivery failed")
    }
}
```

---

### NotFoundError

Returned when a name is not found in the registry. Embeds `RebarError`.

```go
type NotFoundError struct {
    RebarError
}
```

- Returned by `Whereis`, `SendNamed`, `Context.Whereis`, and `Context.SendNamed`.
- FFI error code: `-3` (`REBAR_ERR_NOT_FOUND`).

**Example:**

```go
_, err := rt.Whereis("nonexistent")
if err != nil {
    var notFound *rebar.NotFoundError
    if errors.As(err, &notFound) {
        fmt.Println("name not registered")
    }
}
```

---

### InvalidNameError

Returned when a name passed to a registry function is not valid UTF-8. Embeds `RebarError`.

```go
type InvalidNameError struct {
    RebarError
}
```

- Returned by `Register`, `Whereis`, `SendNamed`, and their `Context` equivalents.
- In practice this is rare in Go since Go strings are conventionally UTF-8, but it can occur through unsafe conversions.
- FFI error code: `-4` (`REBAR_ERR_INVALID_NAME`).

---

### Null Pointer Panics

A null pointer error (`REBAR_ERR_NULL_PTR`, code `-1`) indicates a bug in the Go bindings rather than a user error. The Go client promotes this to a **panic** rather than returning it as an error, since it should never occur under correct usage.

---

## Memory Management

### Runtime Lifecycle

The caller who calls `NewRuntime` owns the runtime and must free it with `Close()`. The idiomatic pattern is:

```go
rt, err := rebar.NewRuntime(1)
if err != nil {
    log.Fatal(err)
}
defer rt.Close()
```

`Close` is idempotent -- calling it multiple times is safe.

### Messages

`Msg` values are pure Go objects backed by byte slices. They are garbage-collected normally and require no manual cleanup.

When sending via `Runtime.Send` or `Context.Send`, the data is copied into a C-side message, sent, and the C message is freed automatically via `defer`. The caller's byte slice is never modified.

### Actors

Actor values are stored in an internal Go map indexed by ID. The C callback trampoline looks up the actor by ID when the spawned process starts. Actor references are held for the lifetime of the runtime.

### Thread Safety

The `Runtime` is internally synchronized -- it is safe to call any method from multiple goroutines concurrently. The underlying C runtime uses a Tokio runtime (thread-safe) and the name registry is protected by a mutex.

`Msg` values are plain Go structs and follow normal Go concurrency rules.

---

## Complete Example

```go
package main

import (
    "errors"
    "fmt"
    "log"
    "time"

    "github.com/alexandernicholson/rebar/clients/go/rebar"
)

// Greeter is an actor that prints a greeting when it receives a message.
type Greeter struct{}

func (g *Greeter) HandleMessage(ctx *rebar.Context, msg *rebar.Msg) {
    if msg == nil {
        // Startup: register ourselves under a well-known name.
        ctx.Register("greeter", ctx.Self())
        fmt.Printf("greeter started at %d:%d\n", ctx.Self().NodeID, ctx.Self().LocalID)
        return
    }
    fmt.Printf("greeter received: %s\n", string(msg.Data()))
}

func main() {
    // Create a runtime on node 1.
    rt, err := rebar.NewRuntime(1)
    if err != nil {
        log.Fatalf("failed to create runtime: %v", err)
    }
    defer rt.Close()

    // Spawn the greeter actor.
    pid, err := rt.SpawnActor(&Greeter{})
    if err != nil {
        log.Fatalf("failed to spawn: %v", err)
    }

    // Send a message by PID.
    if err := rt.Send(pid, []byte("hello by PID")); err != nil {
        log.Printf("send failed: %v", err)
    }

    // Send a message by name.
    if err := rt.SendNamed("greeter", []byte("hello by name")); err != nil {
        log.Printf("send named failed: %v", err)
    }

    // Look up by name.
    found, err := rt.Whereis("greeter")
    if err != nil {
        var notFound *rebar.NotFoundError
        if errors.As(err, &notFound) {
            log.Println("greeter not registered")
        }
    } else {
        fmt.Printf("greeter is at %d:%d\n", found.NodeID, found.LocalID)
    }

    // Give async processes a moment to execute.
    time.Sleep(100 * time.Millisecond)
}
```

---

## See Also

- [rebar-ffi C-ABI Reference](rebar-ffi.md) -- the underlying C API that this package wraps
- [Go Actor Patterns Guide](../guides/actors-go.md) -- idiomatic patterns for building actors in Go
- [Architecture](../architecture.md) -- how the FFI layer fits into the overall crate structure
