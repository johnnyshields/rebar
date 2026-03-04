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
