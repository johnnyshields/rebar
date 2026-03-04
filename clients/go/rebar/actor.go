package rebar

// #include "rebar_ffi.h"
import "C"
import (
	"sync"
	"unsafe"
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

//export goRebarProcessCallback
func goRebarProcessCallback(pid C.rebar_pid_t, context C.uintptr_t) {
	id := uint64(context)
	actorMu.Lock()
	entry, ok := actorMap[id]
	actorMu.Unlock()
	if !ok {
		return
	}
	goPid := pidFromC(pid)
	ctx := &Context{self: goPid, runtime: entry.runtime}

	// Call HandleMessage with nil for the initialization callback.
	entry.actor.HandleMessage(ctx, nil)

	// Start a goroutine that loops calling rebar_recv and dispatches
	// received messages to the actor's HandleMessage.
	go func() {
		for {
			var msgPtr *C.rebar_msg_t
			rc := C.rebar_recv(entry.runtime.ptr, pid, &msgPtr, C.int64_t(100))
			if rc == C.int32_t(errTimeout) {
				continue
			}
			if rc != C.int32_t(errOK) {
				// Process dead or error — exit the loop.
				break
			}
			// Extract message data
			dataPtr := C.rebar_msg_data(msgPtr)
			dataLen := C.rebar_msg_len(msgPtr)
			var data []byte
			if dataLen > 0 {
				data = C.GoBytes(unsafe.Pointer(dataPtr), C.int(dataLen))
			}
			C.rebar_msg_free(msgPtr)

			entry.actor.HandleMessage(ctx, NewMsg(data))
		}
	}()
}
