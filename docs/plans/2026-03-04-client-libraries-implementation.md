# Client Libraries Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create idiomatic Go, Python, and TypeScript client libraries wrapping the `rebar-ffi` C-ABI shared library, with rich actor abstractions and comprehensive documentation.

**Architecture:** Each library provides two layers: (1) private FFI bindings calling `librebar_ffi.{so,dylib,dll}`, and (2) public idiomatic types — `Runtime`, `Pid`, `Msg`, `Actor`, `Context` — with automatic memory management. Monorepo layout under `clients/`.

**Tech Stack:** Go (cgo), Python (ctypes), TypeScript (Deno FFI). All depend on `librebar_ffi` built from source via `cargo build --release -p rebar-ffi`.

---

### Task 1: Build shared library and create clients/ scaffold

**Files:**
- Create: `clients/README.md`
- Verify: `target/release/librebar_ffi.so` (or `.dylib` on macOS)

**Step 1: Build the shared library**

Run: `cargo build --release -p rebar-ffi`
Expected: `target/release/librebar_ffi.so` (Linux) or `target/release/librebar_ffi.dylib` (macOS) exists

**Step 2: Verify the exports**

Run: `nm -D target/release/librebar_ffi.so 2>/dev/null | grep rebar_ || nm target/release/librebar_ffi.dylib 2>/dev/null | grep rebar_`
Expected: All 11 exported symbols: `rebar_msg_create`, `rebar_msg_data`, `rebar_msg_len`, `rebar_msg_free`, `rebar_runtime_new`, `rebar_runtime_free`, `rebar_spawn`, `rebar_send`, `rebar_register`, `rebar_whereis`, `rebar_send_named`

**Step 3: Create clients/README.md**

```markdown
# Rebar Client Libraries

Idiomatic client libraries for Go, Python, and TypeScript, wrapping the Rebar actor runtime via C-ABI FFI.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable, for building the shared library)
- One of: Go 1.21+, Python 3.10+, or Deno 1.38+

## Building the Shared Library

All client libraries require `librebar_ffi` — the compiled shared library from the `rebar-ffi` crate.

### Step 1: Clone and build

```bash
git clone https://github.com/alexandernicholson/rebar.git
cd rebar
cargo build --release -p rebar-ffi
```

### Step 2: Locate the output

The shared library is built to:

| Platform | Path |
|----------|------|
| Linux    | `target/release/librebar_ffi.so` |
| macOS    | `target/release/librebar_ffi.dylib` |
| Windows  | `target/release/rebar_ffi.dll` |

### Step 3: Make it loadable

**Linux:**
```bash
export LD_LIBRARY_PATH="$(pwd)/target/release:$LD_LIBRARY_PATH"
```

**macOS:**
```bash
export DYLD_LIBRARY_PATH="$(pwd)/target/release:$DYLD_LIBRARY_PATH"
```

**Windows (PowerShell):**
```powershell
$env:PATH = "$(Get-Location)\target\release;$env:PATH"
```

### Step 4: Verify

```bash
# Linux
nm -D target/release/librebar_ffi.so | grep rebar_runtime_new

# macOS
nm target/release/librebar_ffi.dylib | grep rebar_runtime_new
```

You should see `rebar_runtime_new` in the output. If so, the library is ready.

## Client Libraries

| Language | Directory | Package Manager | Runtime |
|----------|-----------|----------------|---------|
| [Go](go/README.md) | `clients/go/` | `go get` | cgo |
| [Python](python/README.md) | `clients/python/` | `pip install` | ctypes |
| [TypeScript](typescript/README.md) | `clients/typescript/` | `deno add` | Deno FFI |

## Architecture

Each library provides:

1. **Private FFI bindings** — direct C-ABI calls with proper memory management
2. **Core types** — `Runtime`, `Pid`, `Msg` with automatic cleanup
3. **Actor abstraction** — idiomatic base class/interface for defining actors
4. **Context** — passed to actor handlers, provides `Self()`, `Send()`, `Register()`, `Whereis()`, `SendNamed()`
5. **Error types** — language-native error handling wrapping FFI error codes
```

**Step 4: Create directory structure**

Run: `mkdir -p clients/go/rebar clients/python/rebar clients/python/tests clients/typescript/src clients/typescript/tests`

**Step 5: Commit**

```bash
git add clients/README.md
git commit -m "docs: add clients/ scaffold with build instructions"
```

---

### Task 2: Go client — FFI bindings and core types

**Files:**
- Create: `clients/go/rebar/ffi.go`
- Create: `clients/go/rebar/types.go`
- Create: `clients/go/rebar/errors.go`
- Create: `clients/go/rebar/runtime.go`
- Create: `clients/go/go.mod`

**Step 1: Create go.mod**

```
module github.com/alexandernicholson/rebar/clients/go

go 1.21
```

**Step 2: Create ffi.go — cgo bindings (private)**

```go
package rebar

/*
#cgo LDFLAGS: -lrebar_ffi
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>

// Types
typedef struct {
    uint64_t node_id;
    uint64_t local_id;
} rebar_pid_t;

typedef struct rebar_msg_t rebar_msg_t;
typedef struct rebar_runtime_t rebar_runtime_t;

// Error codes
#define REBAR_OK              0
#define REBAR_ERR_NULL_PTR   -1
#define REBAR_ERR_SEND_FAILED -2
#define REBAR_ERR_NOT_FOUND  -3
#define REBAR_ERR_INVALID_NAME -4

// Message API
rebar_msg_t *rebar_msg_create(const uint8_t *data, size_t len);
const uint8_t *rebar_msg_data(const rebar_msg_t *msg);
size_t rebar_msg_len(const rebar_msg_t *msg);
void rebar_msg_free(rebar_msg_t *msg);

// Runtime API
rebar_runtime_t *rebar_runtime_new(uint64_t node_id);
void rebar_runtime_free(rebar_runtime_t *rt);

// Process API
typedef void (*rebar_process_fn)(rebar_pid_t);
int32_t rebar_spawn(rebar_runtime_t *rt, rebar_process_fn callback,
                    rebar_pid_t *pid_out);
int32_t rebar_send(rebar_runtime_t *rt, rebar_pid_t dest,
                   const rebar_msg_t *msg);

// Registry API
int32_t rebar_register(rebar_runtime_t *rt, const uint8_t *name,
                       size_t name_len, rebar_pid_t pid);
int32_t rebar_whereis(rebar_runtime_t *rt, const uint8_t *name,
                      size_t name_len, rebar_pid_t *pid_out);
int32_t rebar_send_named(rebar_runtime_t *rt, const uint8_t *name,
                         size_t name_len, const rebar_msg_t *msg);

// Go cannot pass Go function pointers directly to C.
// C trampoline that calls back into Go.
extern void goRebarProcessCallback(rebar_pid_t pid);
static inline int32_t rebar_spawn_go(rebar_runtime_t *rt, rebar_pid_t *pid_out) {
    return rebar_spawn(rt, goRebarProcessCallback, pid_out);
}
*/
import "C"
```

**Step 3: Create errors.go**

```go
package rebar

import "fmt"

// Error codes from rebar-ffi.
const (
	errOK          = 0
	errNullPtr     = -1
	errSendFailed  = -2
	errNotFound    = -3
	errInvalidName = -4
)

// RebarError represents an error returned by the Rebar FFI.
type RebarError struct {
	Code    int
	Message string
}

func (e *RebarError) Error() string {
	return fmt.Sprintf("rebar error %d: %s", e.Code, e.Message)
}

// SendError is returned when a message cannot be delivered.
type SendError struct {
	RebarError
}

// NotFoundError is returned when a name is not registered.
type NotFoundError struct {
	RebarError
}

// InvalidNameError is returned when a name is not valid UTF-8.
type InvalidNameError struct {
	RebarError
}

func checkError(rc C.int32_t) error {
	switch int(rc) {
	case errOK:
		return nil
	case errNullPtr:
		panic("rebar: internal error — null pointer passed to FFI")
	case errSendFailed:
		return &SendError{RebarError{Code: errSendFailed, Message: "failed to deliver message"}}
	case errNotFound:
		return &NotFoundError{RebarError{Code: errNotFound, Message: "name not found in registry"}}
	case errInvalidName:
		return &InvalidNameError{RebarError{Code: errInvalidName, Message: "name is not valid UTF-8"}}
	default:
		return &RebarError{Code: int(rc), Message: "unknown error"}
	}
}
```

**Step 4: Create types.go**

```go
package rebar

import "C"
import "unsafe"

// Pid identifies a process within a Rebar runtime.
type Pid struct {
	NodeID  uint64
	LocalID uint64
}

func pidFromC(cp C.rebar_pid_t) Pid {
	return Pid{NodeID: uint64(cp.node_id), LocalID: uint64(cp.local_id)}
}

func pidToC(p Pid) C.rebar_pid_t {
	return C.rebar_pid_t{node_id: C.uint64_t(p.NodeID), local_id: C.uint64_t(p.LocalID)}
}

// Msg wraps a byte slice for sending to processes.
// Messages are automatically freed when no longer referenced.
type Msg struct {
	data []byte
}

// NewMsg creates a message from a byte slice.
func NewMsg(data []byte) *Msg {
	return &Msg{data: append([]byte(nil), data...)}
}

// Data returns the message payload.
func (m *Msg) Data() []byte {
	return m.data
}

// createCMsg creates a C-side message. Caller must free with rebar_msg_free.
func createCMsg(data []byte) *C.rebar_msg_t {
	if len(data) == 0 {
		return C.rebar_msg_create(nil, 0)
	}
	return C.rebar_msg_create((*C.uint8_t)(unsafe.Pointer(&data[0])), C.size_t(len(data)))
}
```

**Step 5: Create runtime.go**

```go
package rebar

import "C"
import "unsafe"

// Runtime manages a Rebar actor runtime. Create with NewRuntime, close with Close.
type Runtime struct {
	ptr *C.rebar_runtime_t
}

// NewRuntime creates a new Rebar runtime for the given node ID.
func NewRuntime(nodeID uint64) (*Runtime, error) {
	ptr := C.rebar_runtime_new(C.uint64_t(nodeID))
	if ptr == nil {
		return nil, &RebarError{Code: -1, Message: "failed to create runtime"}
	}
	return &Runtime{ptr: ptr}, nil
}

// Close frees the runtime and stops all spawned processes.
// Safe to call multiple times.
func (r *Runtime) Close() {
	if r.ptr != nil {
		C.rebar_runtime_free(r.ptr)
		r.ptr = nil
	}
}

// Send sends a message to a process by PID.
func (r *Runtime) Send(dest Pid, data []byte) error {
	msg := createCMsg(data)
	defer C.rebar_msg_free(msg)
	rc := C.rebar_send(r.ptr, pidToC(dest), msg)
	return checkError(rc)
}

// Register associates a name with a PID in the runtime's registry.
func (r *Runtime) Register(name string, pid Pid) error {
	nameBytes := []byte(name)
	rc := C.rebar_register(
		r.ptr,
		(*C.uint8_t)(unsafe.Pointer(&nameBytes[0])),
		C.size_t(len(nameBytes)),
		pidToC(pid),
	)
	return checkError(rc)
}

// Whereis looks up a PID by its registered name.
func (r *Runtime) Whereis(name string) (Pid, error) {
	nameBytes := []byte(name)
	var pidOut C.rebar_pid_t
	rc := C.rebar_whereis(
		r.ptr,
		(*C.uint8_t)(unsafe.Pointer(&nameBytes[0])),
		C.size_t(len(nameBytes)),
		&pidOut,
	)
	if err := checkError(rc); err != nil {
		return Pid{}, err
	}
	return pidFromC(pidOut), nil
}

// SendNamed sends a message to a process by its registered name.
func (r *Runtime) SendNamed(name string, data []byte) error {
	nameBytes := []byte(name)
	msg := createCMsg(data)
	defer C.rebar_msg_free(msg)
	rc := C.rebar_send_named(
		r.ptr,
		(*C.uint8_t)(unsafe.Pointer(&nameBytes[0])),
		C.size_t(len(nameBytes)),
		msg,
	)
	return checkError(rc)
}
```

**Step 6: Commit**

```bash
git add clients/go/
git commit -m "feat(go): add FFI bindings and core types"
```

---

### Task 3: Go client — Actor abstraction

**Files:**
- Create: `clients/go/rebar/actor.go`
- Modify: `clients/go/rebar/runtime.go` (add SpawnActor)

**Step 1: Create actor.go**

```go
package rebar

import "C"
import (
	"sync"
)

// Actor defines the interface that all Rebar actors must implement.
// HandleMessage is called each time the process receives a message.
type Actor interface {
	HandleMessage(ctx *Context, msg *Msg)
}

// Context is passed to Actor.HandleMessage and provides the process's
// identity and messaging capabilities.
type Context struct {
	self    Pid
	runtime *Runtime
}

// Self returns this process's PID.
func (c *Context) Self() Pid {
	return c.self
}

// Send sends a message to another process by PID.
func (c *Context) Send(dest Pid, data []byte) error {
	return c.runtime.Send(dest, data)
}

// Register associates a name with a PID in the registry.
func (c *Context) Register(name string, pid Pid) error {
	return c.runtime.Register(name, pid)
}

// Whereis looks up a PID by name.
func (c *Context) Whereis(name string) (Pid, error) {
	return c.runtime.Whereis(name)
}

// SendNamed sends a message to a named process.
func (c *Context) SendNamed(name string, data []byte) error {
	return c.runtime.SendNamed(name, data)
}

// --- Actor registration for C callback trampoline ---

var (
	actorMu      sync.Mutex
	actorMap     = make(map[uint64]actorEntry)
	actorCounter uint64
)

type actorEntry struct {
	actor   Actor
	runtime *Runtime
}

func registerActor(a Actor, r *Runtime) uint64 {
	actorMu.Lock()
	defer actorMu.Unlock()
	actorCounter++
	id := actorCounter
	actorMap[id] = actorEntry{actor: a, runtime: r}
	return id
}

// The active actor ID is set before spawning so the C callback can find it.
// This is safe because rebar_spawn blocks until the callback is invoked.
var activeActorID uint64

//export goRebarProcessCallback
func goRebarProcessCallback(pid C.rebar_pid_t) {
	actorMu.Lock()
	entry, ok := actorMap[activeActorID]
	actorMu.Unlock()
	if !ok {
		return
	}
	goPid := pidFromC(pid)
	ctx := &Context{self: goPid, runtime: entry.runtime}
	entry.actor.HandleMessage(ctx, nil)
}
```

**Step 2: Add SpawnActor to runtime.go**

Append to `runtime.go`:

```go
// SpawnActor spawns a new process backed by the given Actor.
// The actor's HandleMessage is called with a nil message on startup
// (as a lifecycle hook), and the process PID is returned.
func (r *Runtime) SpawnActor(actor Actor) (Pid, error) {
	id := registerActor(actor, r)

	actorMu.Lock()
	activeActorID = id
	actorMu.Unlock()

	var pidOut C.rebar_pid_t
	rc := C.rebar_spawn_go(r.ptr, &pidOut)
	if err := checkError(rc); err != nil {
		return Pid{}, err
	}
	return pidFromC(pidOut), nil
}
```

**Step 3: Commit**

```bash
git add clients/go/rebar/actor.go clients/go/rebar/runtime.go
git commit -m "feat(go): add Actor interface and SpawnActor"
```

---

### Task 4: Go client — Tests

**Files:**
- Create: `clients/go/rebar/rebar_test.go`

**Step 1: Write tests**

```go
package rebar

import (
	"testing"
)

func TestNewRuntime(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()
}

func TestCloseIdempotent(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	rt.Close()
	rt.Close() // should not panic
}

func TestPid(t *testing.T) {
	p := Pid{NodeID: 7, LocalID: 42}
	if p.NodeID != 7 || p.LocalID != 42 {
		t.Fatalf("unexpected PID: %+v", p)
	}
}

func TestNewMsg(t *testing.T) {
	msg := NewMsg([]byte("hello"))
	if string(msg.Data()) != "hello" {
		t.Fatalf("unexpected data: %s", msg.Data())
	}
}

func TestSendToInvalidPid(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	err = rt.Send(Pid{NodeID: 1, LocalID: 999999}, []byte("nope"))
	if err == nil {
		t.Fatal("expected error sending to invalid PID")
	}
	if _, ok := err.(*SendError); !ok {
		t.Fatalf("expected SendError, got %T: %v", err, err)
	}
}

func TestRegisterAndWhereis(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	pid := Pid{NodeID: 1, LocalID: 42}
	if err := rt.Register("test_service", pid); err != nil {
		t.Fatalf("Register failed: %v", err)
	}

	found, err := rt.Whereis("test_service")
	if err != nil {
		t.Fatalf("Whereis failed: %v", err)
	}
	if found.NodeID != 1 || found.LocalID != 42 {
		t.Fatalf("unexpected PID: %+v", found)
	}
}

func TestWhereisNotFound(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	_, err = rt.Whereis("nonexistent")
	if err == nil {
		t.Fatal("expected error for missing name")
	}
	if _, ok := err.(*NotFoundError); !ok {
		t.Fatalf("expected NotFoundError, got %T: %v", err, err)
	}
}

type testActor struct {
	called bool
}

func (a *testActor) HandleMessage(ctx *Context, msg *Msg) {
	a.called = true
}

func TestSpawnActor(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	actor := &testActor{}
	pid, err := rt.SpawnActor(actor)
	if err != nil {
		t.Fatalf("SpawnActor failed: %v", err)
	}
	if pid.NodeID != 1 || pid.LocalID == 0 {
		t.Fatalf("unexpected PID: %+v", pid)
	}
}
```

**Step 2: Run tests**

Run: `cd clients/go && CGO_LDFLAGS="-L../../target/release" LD_LIBRARY_PATH=../../target/release go test ./rebar/ -v`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add clients/go/rebar/rebar_test.go
git commit -m "test(go): add Go client library tests"
```

---

### Task 5: Go client — README and docs

**Files:**
- Create: `clients/go/README.md`
- Create: `docs/api/client-go.md`
- Create: `docs/guides/actors-go.md`

**Step 1: Create clients/go/README.md**

````markdown
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
````

**Step 2: Create docs/api/client-go.md**

Write full API reference following `docs/api/rebar-ffi.md` pattern — types, methods, error types, examples.

**Step 3: Create docs/guides/actors-go.md**

Write pattern guide explaining:
- Why Go uses a single-method `Actor` interface (Go idiom: small interfaces)
- The `Context` pattern (dependency injection, not global state)
- Error handling via `error` return values (not panics)
- Memory management (automatic via deferred `Close()`)
- Example: ping-pong actors, named services

**Step 4: Commit**

```bash
git add clients/go/README.md docs/api/client-go.md docs/guides/actors-go.md
git commit -m "docs(go): add README, API reference, and actor patterns guide"
```

---

### Task 6: Python client — FFI bindings and core types

**Files:**
- Create: `clients/python/rebar/__init__.py`
- Create: `clients/python/rebar/_ffi.py`
- Create: `clients/python/rebar/types.py`
- Create: `clients/python/rebar/errors.py`
- Create: `clients/python/rebar/runtime.py`
- Create: `clients/python/pyproject.toml`

**Step 1: Create pyproject.toml**

```toml
[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[project]
name = "rebar"
version = "0.1.0"
description = "Python client for the Rebar actor runtime"
requires-python = ">=3.10"

[tool.setuptools.packages.find]
where = ["."]
```

**Step 2: Create _ffi.py — ctypes bindings (private)**

```python
"""Private FFI bindings for librebar_ffi. Do not use directly."""

import ctypes
import ctypes.util
import os
import sys
from pathlib import Path


class RebarPid(ctypes.Structure):
    _fields_ = [("node_id", ctypes.c_uint64), ("local_id", ctypes.c_uint64)]


# Callback type: void (*)(rebar_pid_t)
PROCESS_CALLBACK = ctypes.CFUNCTYPE(None, RebarPid)


def _find_library() -> str:
    """Find librebar_ffi, checking common locations."""
    # Check environment variable first
    env_path = os.environ.get("REBAR_LIB_PATH")
    if env_path and os.path.isfile(env_path):
        return env_path

    # Platform-specific names
    if sys.platform == "darwin":
        name = "librebar_ffi.dylib"
    elif sys.platform == "win32":
        name = "rebar_ffi.dll"
    else:
        name = "librebar_ffi.so"

    # Check LD_LIBRARY_PATH / standard paths
    found = ctypes.util.find_library("rebar_ffi")
    if found:
        return found

    # Check relative to this file (for development)
    repo_root = Path(__file__).resolve().parent.parent.parent.parent
    release_path = repo_root / "target" / "release" / name
    if release_path.is_file():
        return str(release_path)

    raise OSError(
        f"Cannot find {name}. Build it with: cargo build --release -p rebar-ffi\n"
        f"Then set LD_LIBRARY_PATH or REBAR_LIB_PATH."
    )


_lib = ctypes.CDLL(_find_library())

# --- Message API ---
_lib.rebar_msg_create.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
_lib.rebar_msg_create.restype = ctypes.c_void_p

_lib.rebar_msg_data.argtypes = [ctypes.c_void_p]
_lib.rebar_msg_data.restype = ctypes.POINTER(ctypes.c_uint8)

_lib.rebar_msg_len.argtypes = [ctypes.c_void_p]
_lib.rebar_msg_len.restype = ctypes.c_size_t

_lib.rebar_msg_free.argtypes = [ctypes.c_void_p]
_lib.rebar_msg_free.restype = None

# --- Runtime API ---
_lib.rebar_runtime_new.argtypes = [ctypes.c_uint64]
_lib.rebar_runtime_new.restype = ctypes.c_void_p

_lib.rebar_runtime_free.argtypes = [ctypes.c_void_p]
_lib.rebar_runtime_free.restype = None

# --- Process API ---
_lib.rebar_spawn.argtypes = [ctypes.c_void_p, PROCESS_CALLBACK, ctypes.POINTER(RebarPid)]
_lib.rebar_spawn.restype = ctypes.c_int32

_lib.rebar_send.argtypes = [ctypes.c_void_p, RebarPid, ctypes.c_void_p]
_lib.rebar_send.restype = ctypes.c_int32

# --- Registry API ---
_lib.rebar_register.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t, RebarPid
]
_lib.rebar_register.restype = ctypes.c_int32

_lib.rebar_whereis.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(RebarPid)
]
_lib.rebar_whereis.restype = ctypes.c_int32

_lib.rebar_send_named.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p
]
_lib.rebar_send_named.restype = ctypes.c_int32
```

**Step 3: Create errors.py**

```python
"""Error types for the Rebar Python client."""


class RebarError(Exception):
    """Base error for all Rebar operations."""

    def __init__(self, code: int, message: str):
        self.code = code
        super().__init__(f"rebar error {code}: {message}")


class SendError(RebarError):
    """Raised when a message cannot be delivered."""

    def __init__(self):
        super().__init__(-2, "failed to deliver message")


class NotFoundError(RebarError):
    """Raised when a name is not found in the registry."""

    def __init__(self):
        super().__init__(-3, "name not found in registry")


class InvalidNameError(RebarError):
    """Raised when a name is not valid UTF-8."""

    def __init__(self):
        super().__init__(-4, "name is not valid UTF-8")


def check_error(rc: int) -> None:
    """Raise an appropriate exception for non-zero FFI return codes."""
    if rc == 0:
        return
    if rc == -1:
        raise RuntimeError("rebar: internal error — null pointer passed to FFI")
    if rc == -2:
        raise SendError()
    if rc == -3:
        raise NotFoundError()
    if rc == -4:
        raise InvalidNameError()
    raise RebarError(rc, "unknown error")
```

**Step 4: Create types.py**

```python
"""Core types for the Rebar Python client."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Pid:
    """Identifies a process within a Rebar runtime."""

    node_id: int
    local_id: int

    def __str__(self) -> str:
        return f"<{self.node_id}.{self.local_id}>"
```

**Step 5: Create runtime.py**

```python
"""Runtime and Context for the Rebar Python client."""

from __future__ import annotations

import ctypes
from typing import TYPE_CHECKING

from . import _ffi
from .errors import check_error
from .types import Pid

if TYPE_CHECKING:
    from .actor import Actor


class Context:
    """Passed to Actor.handle_message. Provides messaging capabilities."""

    def __init__(self, pid: Pid, runtime: Runtime):
        self._pid = pid
        self._runtime = runtime

    def self_pid(self) -> Pid:
        """Return this process's PID."""
        return self._pid

    def send(self, dest: Pid, data: bytes) -> None:
        """Send a message to another process."""
        self._runtime.send(dest, data)

    def register(self, name: str, pid: Pid) -> None:
        """Register a name for a PID."""
        self._runtime.register(name, pid)

    def whereis(self, name: str) -> Pid:
        """Look up a PID by name."""
        return self._runtime.whereis(name)

    def send_named(self, name: str, data: bytes) -> None:
        """Send a message to a named process."""
        self._runtime.send_named(name, data)


class Runtime:
    """Manages a Rebar actor runtime. Use as a context manager.

    Example::

        with Runtime(node_id=1) as rt:
            pid = rt.spawn_actor(MyActor())
            rt.send(pid, b"hello")
    """

    def __init__(self, node_id: int = 1):
        self._ptr = _ffi._lib.rebar_runtime_new(node_id)
        if not self._ptr:
            raise RuntimeError("failed to create Rebar runtime")
        # Keep references to callbacks to prevent GC
        self._callbacks: list[_ffi.PROCESS_CALLBACK] = []

    def close(self) -> None:
        """Free the runtime. Safe to call multiple times."""
        if self._ptr:
            _ffi._lib.rebar_runtime_free(self._ptr)
            self._ptr = None
            self._callbacks.clear()

    def __enter__(self) -> Runtime:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()

    def send(self, dest: Pid, data: bytes) -> None:
        """Send a message to a process by PID."""
        msg = _ffi._lib.rebar_msg_create(data, len(data))
        try:
            rc = _ffi._lib.rebar_send(self._ptr, _ffi.RebarPid(dest.node_id, dest.local_id), msg)
            check_error(rc)
        finally:
            _ffi._lib.rebar_msg_free(msg)

    def register(self, name: str, pid: Pid) -> None:
        """Register a name for a PID."""
        name_bytes = name.encode("utf-8")
        rc = _ffi._lib.rebar_register(
            self._ptr, name_bytes, len(name_bytes),
            _ffi.RebarPid(pid.node_id, pid.local_id),
        )
        check_error(rc)

    def whereis(self, name: str) -> Pid:
        """Look up a PID by name."""
        name_bytes = name.encode("utf-8")
        pid_out = _ffi.RebarPid()
        rc = _ffi._lib.rebar_whereis(
            self._ptr, name_bytes, len(name_bytes), ctypes.byref(pid_out),
        )
        check_error(rc)
        return Pid(node_id=pid_out.node_id, local_id=pid_out.local_id)

    def send_named(self, name: str, data: bytes) -> None:
        """Send a message to a named process."""
        name_bytes = name.encode("utf-8")
        msg = _ffi._lib.rebar_msg_create(data, len(data))
        try:
            rc = _ffi._lib.rebar_send_named(
                self._ptr, name_bytes, len(name_bytes), msg,
            )
            check_error(rc)
        finally:
            _ffi._lib.rebar_msg_free(msg)

    def spawn_actor(self, actor: Actor) -> Pid:
        """Spawn a new process backed by the given Actor.

        The actor's handle_message is called with a None message on startup.
        """
        pid_out = _ffi.RebarPid()
        runtime_ref = self

        @_ffi.PROCESS_CALLBACK
        def callback(ffi_pid: _ffi.RebarPid) -> None:
            pid = Pid(node_id=ffi_pid.node_id, local_id=ffi_pid.local_id)
            ctx = Context(pid, runtime_ref)
            actor.handle_message(ctx, None)

        # Keep reference to prevent GC of the callback
        self._callbacks.append(callback)

        rc = _ffi._lib.rebar_spawn(self._ptr, callback, ctypes.byref(pid_out))
        check_error(rc)
        return Pid(node_id=pid_out.node_id, local_id=pid_out.local_id)
```

**Step 6: Create __init__.py**

```python
"""Rebar Python client — idiomatic wrapper for the Rebar actor runtime."""

from .actor import Actor
from .errors import InvalidNameError, NotFoundError, RebarError, SendError
from .runtime import Context, Runtime
from .types import Pid

__all__ = [
    "Actor",
    "Context",
    "InvalidNameError",
    "NotFoundError",
    "Pid",
    "RebarError",
    "Runtime",
    "SendError",
]
```

Note: `actor.py` is created in the next task. For now create a placeholder:

```python
# clients/python/rebar/actor.py — placeholder, implemented in Task 7
from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .runtime import Context


class Actor(ABC):
    """Base class for Rebar actors. Subclass and implement handle_message."""

    @abstractmethod
    def handle_message(self, ctx: "Context", msg: Optional[bytes]) -> None:
        """Called when this actor receives a message.

        Args:
            ctx: The process context, providing self_pid(), send(), etc.
            msg: The message payload as bytes, or None on startup.
        """
        ...
```

**Step 7: Commit**

```bash
git add clients/python/
git commit -m "feat(python): add FFI bindings, core types, and runtime"
```

---

### Task 7: Python client — Actor abstraction (already created above as placeholder)

The Actor ABC was included in Task 6. This task adds the full implementation if needed and creates tests.

**Files:**
- Verify: `clients/python/rebar/actor.py` (created in Task 6)
- Create: `clients/python/tests/__init__.py`
- Create: `clients/python/tests/test_rebar.py`

**Step 1: Create tests/__init__.py**

Empty file.

**Step 2: Create test_rebar.py**

```python
"""Tests for the Rebar Python client."""

import pytest
from rebar import Actor, Context, NotFoundError, Pid, Runtime, SendError


class TestRuntime:
    def test_create_and_close(self):
        rt = Runtime(node_id=1)
        rt.close()

    def test_context_manager(self):
        with Runtime(node_id=1) as rt:
            pass  # should not raise

    def test_close_idempotent(self):
        rt = Runtime(node_id=1)
        rt.close()
        rt.close()  # should not raise


class TestPid:
    def test_fields(self):
        pid = Pid(node_id=7, local_id=42)
        assert pid.node_id == 7
        assert pid.local_id == 42

    def test_str(self):
        pid = Pid(node_id=1, local_id=5)
        assert str(pid) == "<1.5>"

    def test_frozen(self):
        pid = Pid(node_id=1, local_id=1)
        with pytest.raises(AttributeError):
            pid.node_id = 2  # type: ignore


class TestSend:
    def test_send_invalid_pid(self):
        with Runtime(node_id=1) as rt:
            with pytest.raises(SendError):
                rt.send(Pid(node_id=1, local_id=999999), b"nope")


class TestRegistry:
    def test_register_and_whereis(self):
        with Runtime(node_id=1) as rt:
            pid = Pid(node_id=1, local_id=42)
            rt.register("test_service", pid)
            found = rt.whereis("test_service")
            assert found.node_id == 1
            assert found.local_id == 42

    def test_whereis_not_found(self):
        with Runtime(node_id=1) as rt:
            with pytest.raises(NotFoundError):
                rt.whereis("nonexistent")


class EchoActor(Actor):
    def __init__(self):
        self.started = False

    def handle_message(self, ctx: Context, msg: bytes | None) -> None:
        self.started = True


class TestActor:
    def test_spawn_actor(self):
        with Runtime(node_id=1) as rt:
            actor = EchoActor()
            pid = rt.spawn_actor(actor)
            assert pid.node_id == 1
            assert pid.local_id > 0
```

**Step 3: Run tests**

Run: `cd clients/python && LD_LIBRARY_PATH=../../target/release python -m pytest tests/ -v`
Expected: All tests PASS

**Step 4: Commit**

```bash
git add clients/python/tests/
git commit -m "test(python): add Python client library tests"
```

---

### Task 8: Python client — README and docs

**Files:**
- Create: `clients/python/README.md`
- Create: `docs/api/client-python.md`
- Create: `docs/guides/actors-python.md`

**Step 1: Create clients/python/README.md**

Getting started guide with: prerequisites, build librebar_ffi, install package (`pip install -e clients/python`), hello world example using `with Runtime() as rt:` and ABC actor.

**Step 2: Create docs/api/client-python.md**

Full API reference: `Runtime` (context manager), `Pid` (frozen dataclass), `Actor` (ABC), `Context`, error classes, examples.

**Step 3: Create docs/guides/actors-python.md**

Pattern guide explaining:
- Why Python uses ABC (Pythonic inheritance, `@abstractmethod`)
- Context managers for resource cleanup (`with Runtime() as rt:`)
- `bytes` for payloads (Python's native binary type)
- Exception hierarchy (Python's error handling convention)
- Type hints throughout (modern Python best practice)
- Example: ping-pong actors, named services

**Step 4: Commit**

```bash
git add clients/python/README.md docs/api/client-python.md docs/guides/actors-python.md
git commit -m "docs(python): add README, API reference, and actor patterns guide"
```

---

### Task 9: TypeScript client — FFI bindings and core types

**Files:**
- Create: `clients/typescript/src/ffi.ts`
- Create: `clients/typescript/src/types.ts`
- Create: `clients/typescript/src/errors.ts`
- Create: `clients/typescript/src/runtime.ts`
- Create: `clients/typescript/src/mod.ts`
- Create: `clients/typescript/deno.json`

**Step 1: Create deno.json**

```json
{
  "name": "@rebar/client",
  "version": "0.1.0",
  "exports": "./src/mod.ts",
  "tasks": {
    "test": "deno test --unstable-ffi --allow-ffi tests/"
  }
}
```

**Step 2: Create src/ffi.ts — Deno FFI bindings (private)**

```typescript
/**
 * Private FFI bindings for librebar_ffi. Do not import directly.
 */

const libName = Deno.build.os === "darwin"
  ? "librebar_ffi.dylib"
  : Deno.build.os === "windows"
  ? "rebar_ffi.dll"
  : "librebar_ffi.so";

const libPath = Deno.env.get("REBAR_LIB_PATH") ?? libName;

export const lib = Deno.dlopen(libPath, {
  rebar_msg_create: {
    parameters: ["buffer", "usize"],
    result: "pointer",
  },
  rebar_msg_data: {
    parameters: ["pointer"],
    result: "pointer",
  },
  rebar_msg_len: {
    parameters: ["pointer"],
    result: "usize",
  },
  rebar_msg_free: {
    parameters: ["pointer"],
    result: "void",
  },
  rebar_runtime_new: {
    parameters: ["u64"],
    result: "pointer",
  },
  rebar_runtime_free: {
    parameters: ["pointer"],
    result: "void",
  },
  rebar_spawn: {
    parameters: ["pointer", "function", "buffer"],
    result: "i32",
  },
  rebar_send: {
    parameters: ["pointer", { struct: ["u64", "u64"] }, "pointer"],
    result: "i32",
  },
  rebar_register: {
    parameters: ["pointer", "buffer", "usize", { struct: ["u64", "u64"] }],
    result: "i32",
  },
  rebar_whereis: {
    parameters: ["pointer", "buffer", "usize", "buffer"],
    result: "i32",
  },
  rebar_send_named: {
    parameters: ["pointer", "buffer", "usize", "pointer"],
    result: "i32",
  },
});

// Callback definition for rebar_spawn
export const processCallbackDef = {
  parameters: [{ struct: ["u64", "u64"] }],
  result: "void",
} as const;
```

**Step 3: Create src/types.ts**

```typescript
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
```

**Step 4: Create src/errors.ts**

```typescript
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
    case -4:
      throw new InvalidNameError();
    default:
      throw new RebarError(rc, "unknown error");
  }
}
```

**Step 5: Create src/runtime.ts**

```typescript
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
}

/** Manages a Rebar actor runtime. Implements Disposable for `using`. */
export class Runtime implements Disposable {
  private ptr: Deno.PointerValue;
  private callbacks: Deno.UnsafeCallback[] = [];

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
    const msg = lib.symbols.rebar_msg_create(data, data.byteLength);
    try {
      const rc = lib.symbols.rebar_send(
        this.ptr,
        new BigUint64Array([BigInt(dest.nodeId), BigInt(dest.localId)]),
        msg,
      );
      checkError(rc);
    } finally {
      lib.symbols.rebar_msg_free(msg);
    }
  }

  /** Register a name for a PID. */
  register(name: string, pid: Pid): void {
    const nameBytes = new TextEncoder().encode(name);
    const rc = lib.symbols.rebar_register(
      this.ptr,
      nameBytes,
      nameBytes.byteLength,
      new BigUint64Array([BigInt(pid.nodeId), BigInt(pid.localId)]),
    );
    checkError(rc);
  }

  /** Look up a PID by name. */
  whereis(name: string): Pid {
    const nameBytes = new TextEncoder().encode(name);
    const pidBuf = new BigUint64Array(2);
    const rc = lib.symbols.rebar_whereis(
      this.ptr,
      nameBytes,
      nameBytes.byteLength,
      pidBuf,
    );
    checkError(rc);
    return new Pid(pidBuf[0], pidBuf[1]);
  }

  /** Send a message to a named process. */
  sendNamed(name: string, data: Uint8Array): void {
    const nameBytes = new TextEncoder().encode(name);
    const msg = lib.symbols.rebar_msg_create(data, data.byteLength);
    try {
      const rc = lib.symbols.rebar_send_named(
        this.ptr,
        nameBytes,
        nameBytes.byteLength,
        msg,
      );
      checkError(rc);
    } finally {
      lib.symbols.rebar_msg_free(msg);
    }
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
      (pidStruct: BigUint64Array) => {
        const pid = new Pid(pidStruct[0], pidStruct[1]);
        const ctx = new Context(pid, runtimeRef);
        actor.handleMessage(ctx, null);
      },
    );
    this.callbacks.push(callback);

    const rc = lib.symbols.rebar_spawn(this.ptr, callback.pointer, pidBuf);
    checkError(rc);
    return new Pid(pidBuf[0], pidBuf[1]);
  }
}
```

**Step 6: Create src/actor.ts**

```typescript
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
```

**Step 7: Create src/mod.ts**

```typescript
export { Actor } from "./actor.ts";
export { Context, Runtime } from "./runtime.ts";
export { Pid } from "./types.ts";
export {
  InvalidNameError,
  NotFoundError,
  RebarError,
  SendError,
} from "./errors.ts";
```

**Step 8: Commit**

```bash
git add clients/typescript/
git commit -m "feat(typescript): add FFI bindings, core types, and runtime"
```

---

### Task 10: TypeScript client — Tests

**Files:**
- Create: `clients/typescript/tests/rebar_test.ts`

**Step 1: Write tests**

```typescript
import { assertEquals, assertThrows } from "https://deno.land/std/assert/mod.ts";
import { Actor, Context, NotFoundError, Pid, Runtime, SendError } from "../src/mod.ts";

Deno.test("Runtime - create and close", () => {
  const rt = new Runtime(1n);
  rt.close();
});

Deno.test("Runtime - close idempotent", () => {
  const rt = new Runtime(1n);
  rt.close();
  rt.close(); // should not throw
});

Deno.test("Runtime - using disposable", () => {
  using rt = new Runtime(1n);
  // should dispose automatically
});

Deno.test("Pid - fields", () => {
  const pid = new Pid(7n, 42n);
  assertEquals(pid.nodeId, 7n);
  assertEquals(pid.localId, 42n);
});

Deno.test("Pid - toString", () => {
  const pid = new Pid(1n, 5n);
  assertEquals(pid.toString(), "<1.5>");
});

Deno.test("Send - invalid pid", () => {
  using rt = new Runtime(1n);
  assertThrows(
    () => rt.send(new Pid(1n, 999999n), new Uint8Array([1, 2, 3])),
    SendError,
  );
});

Deno.test("Registry - register and whereis", () => {
  using rt = new Runtime(1n);
  const pid = new Pid(1n, 42n);
  rt.register("test_service", pid);
  const found = rt.whereis("test_service");
  assertEquals(found.nodeId, 1n);
  assertEquals(found.localId, 42n);
});

Deno.test("Registry - whereis not found", () => {
  using rt = new Runtime(1n);
  assertThrows(() => rt.whereis("nonexistent"), NotFoundError);
});

class TestActor extends Actor {
  started = false;
  handleMessage(ctx: Context, msg: Uint8Array | null): void {
    this.started = true;
  }
}

Deno.test("Actor - spawn", () => {
  using rt = new Runtime(1n);
  const actor = new TestActor();
  const pid = rt.spawnActor(actor);
  assertEquals(pid.nodeId, 1n);
});
```

**Step 2: Run tests**

Run: `cd clients/typescript && LD_LIBRARY_PATH=../../target/release deno test --unstable-ffi --allow-ffi tests/`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add clients/typescript/tests/
git commit -m "test(typescript): add TypeScript client library tests"
```

---

### Task 11: TypeScript client — README and docs

**Files:**
- Create: `clients/typescript/README.md`
- Create: `docs/api/client-typescript.md`
- Create: `docs/guides/actors-typescript.md`

**Step 1: Create clients/typescript/README.md**

Getting started guide with: prerequisites (Deno 1.38+), build librebar_ffi, set `REBAR_LIB_PATH` or `LD_LIBRARY_PATH`, hello world example using `using runtime = new Runtime(1n)`.

**Step 2: Create docs/api/client-typescript.md**

Full API reference: `Runtime` (Disposable), `Pid`, `Actor` (abstract class), `Context`, error classes, examples.

**Step 3: Create docs/guides/actors-typescript.md**

Pattern guide explaining:
- Why TypeScript uses abstract class (class-based OOP, `abstract` keyword)
- `Disposable` protocol and `using` keyword (modern JS/TS resource management)
- `Uint8Array` for payloads (JS native binary type)
- `bigint` for IDs (Deno FFI requires BigInt for u64)
- Error class hierarchy (TypeScript's error convention)
- Example: ping-pong actors, named services

**Step 4: Commit**

```bash
git add clients/typescript/README.md docs/api/client-typescript.md docs/guides/actors-typescript.md
git commit -m "docs(typescript): add README, API reference, and actor patterns guide"
```

---

### Task 12: Update main docs with client library references

**Files:**
- Modify: `docs/getting-started.md` (add client libraries to Next Steps)
- Modify: `README.md` (mention client libraries)

**Step 1: Update getting-started.md Next Steps**

Add to the Next Steps section:

```markdown
- Client libraries: [Go](clients/go/README.md) | [Python](clients/python/README.md) | [TypeScript](clients/typescript/README.md) for using Rebar from other languages.
```

**Step 2: Update README.md**

Add a "Client Libraries" section after the existing feature list:

```markdown
### Client Libraries

Idiomatic wrappers for embedding Rebar in Go, Python, and TypeScript applications:

| Language | Package | Actor Pattern |
|----------|---------|--------------|
| Go | `clients/go/` | `Actor` interface with `HandleMessage(ctx, msg)` |
| Python | `clients/python/` | `Actor` ABC with `handle_message(ctx, msg)` |
| TypeScript | `clients/typescript/` | `Actor` abstract class with `handleMessage(ctx, msg)` |

See [Client Libraries](clients/README.md) for build instructions.
```

**Step 3: Commit**

```bash
git add docs/getting-started.md README.md
git commit -m "docs: add client library references to main docs"
```

---

### Task 13: Final verification and push

**Step 1: Run all Rust tests**

Run: `cargo test --workspace`
Expected: All tests pass (no regressions)

**Step 2: Run Go tests**

Run: `cd clients/go && CGO_LDFLAGS="-L../../target/release" LD_LIBRARY_PATH=../../target/release go test ./rebar/ -v`
Expected: All tests pass

**Step 3: Run Python tests**

Run: `cd clients/python && LD_LIBRARY_PATH=../../target/release python -m pytest tests/ -v`
Expected: All tests pass

**Step 4: Run TypeScript tests**

Run: `cd clients/typescript && LD_LIBRARY_PATH=../../target/release deno test --unstable-ffi --allow-ffi tests/`
Expected: All tests pass

**Step 5: Push**

```bash
git push origin main
```

---

## Summary

| Task | Files | Action |
|------|-------|--------|
| 1 | `clients/README.md` | Scaffold + build instructions |
| 2 | `clients/go/rebar/{ffi,types,errors,runtime}.go` | Go FFI bindings + core types |
| 3 | `clients/go/rebar/actor.go` | Go Actor interface + Context |
| 4 | `clients/go/rebar/rebar_test.go` | Go tests |
| 5 | `clients/go/README.md`, `docs/api/client-go.md`, `docs/guides/actors-go.md` | Go docs |
| 6 | `clients/python/rebar/{_ffi,types,errors,runtime,actor,__init__}.py` | Python FFI + core types |
| 7 | `clients/python/tests/test_rebar.py` | Python tests |
| 8 | `clients/python/README.md`, `docs/api/client-python.md`, `docs/guides/actors-python.md` | Python docs |
| 9 | `clients/typescript/src/{ffi,types,errors,runtime,actor,mod}.ts` | TypeScript FFI + core types |
| 10 | `clients/typescript/tests/rebar_test.ts` | TypeScript tests |
| 11 | `clients/typescript/README.md`, `docs/api/client-typescript.md`, `docs/guides/actors-typescript.md` | TypeScript docs |
| 12 | `docs/getting-started.md`, `README.md` | Cross-references |
| 13 | All | Final verification + push |
