package rebar

/*
#include "rebar_ffi.h"

// Go cannot pass Go function pointers directly to C.
// C trampoline that calls back into Go.
extern void goRebarProcessCallback(rebar_pid_t pid);
static inline int32_t rebar_spawn_go(rebar_runtime_t *rt, rebar_pid_t *pid_out) {
    return rebar_spawn(rt, goRebarProcessCallback, pid_out);
}
*/
import "C"
import (
	"sync"
	"unsafe"
)

// ErrRuntimeClosed is returned when an operation is attempted on a closed runtime.
var ErrRuntimeClosed = &RebarError{Code: -100, Message: "runtime is closed"}

// Runtime manages a Rebar actor runtime. Create with NewRuntime, close with Close.
type Runtime struct {
	mu     sync.RWMutex
	ptr    *C.rebar_runtime_t
	closed bool
	once   sync.Once
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
// Safe to call multiple times and concurrently.
func (r *Runtime) Close() {
	r.once.Do(func() {
		r.mu.Lock()
		defer r.mu.Unlock()
		C.rebar_runtime_free(r.ptr)
		r.ptr = nil
		r.closed = true
	})
}

// acquire takes a read lock and checks the runtime is still open.
// Caller must call r.mu.RUnlock() when done.
func (r *Runtime) acquire() error {
	r.mu.RLock()
	if r.closed {
		r.mu.RUnlock()
		return ErrRuntimeClosed
	}
	return nil
}

// Send sends a message to a process by PID.
func (r *Runtime) Send(dest Pid, data []byte) error {
	if err := r.acquire(); err != nil {
		return err
	}
	defer r.mu.RUnlock()
	msg := createCMsg(data)
	defer C.rebar_msg_free(msg)
	rc := C.rebar_send(r.ptr, pidToC(dest), msg)
	return checkError(rc)
}

// Register associates a name with a PID in the runtime's registry.
func (r *Runtime) Register(name string, pid Pid) error {
	if len(name) == 0 {
		return &InvalidNameError{RebarError{Code: errInvalidName, Message: "name must not be empty"}}
	}
	if err := r.acquire(); err != nil {
		return err
	}
	defer r.mu.RUnlock()
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
	if len(name) == 0 {
		return Pid{}, &InvalidNameError{RebarError{Code: errInvalidName, Message: "name must not be empty"}}
	}
	if err := r.acquire(); err != nil {
		return Pid{}, err
	}
	defer r.mu.RUnlock()
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
	if len(name) == 0 {
		return &InvalidNameError{RebarError{Code: errInvalidName, Message: "name must not be empty"}}
	}
	if err := r.acquire(); err != nil {
		return err
	}
	defer r.mu.RUnlock()
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

// SpawnActor spawns a new process backed by the given Actor.
// The actor's HandleMessage is called with a nil message on startup
// (as a lifecycle hook), and the process PID is returned.
func (r *Runtime) SpawnActor(actor Actor) (Pid, error) {
	if err := r.acquire(); err != nil {
		return Pid{}, err
	}
	defer r.mu.RUnlock()

	id := registerActor(actor, r)

	// Hold actorMu across both the activeActorID write and the C call.
	// rebar_spawn_go blocks until the callback completes, so the lock
	// is released promptly and concurrent spawns are serialized correctly.
	actorMu.Lock()
	activeActorID = id
	var pidOut C.rebar_pid_t
	rc := C.rebar_spawn_go(r.ptr, &pidOut)
	actorMu.Unlock()

	if err := checkError(rc); err != nil {
		unregisterActor(id)
		return Pid{}, err
	}
	return pidFromC(pidOut), nil
}
