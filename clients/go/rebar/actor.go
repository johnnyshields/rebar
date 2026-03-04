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

// Recv receives a message from this process's mailbox.
// timeoutMs: 0 = non-blocking, >0 = timeout in ms, <0 = block forever.
func (c *Context) Recv(timeoutMs int64) (*Msg, error) {
	return c.runtime.Recv(c.self, timeoutMs)
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

	// Call init with nil message.
	entry.actor.HandleMessage(ctx, nil)

	// Start a message loop in a goroutine that polls rebar_recv.
	actorID := activeActorID
	go func() {
		defer func() {
			actorMu.Lock()
			delete(actorMap, actorID)
			actorMu.Unlock()
		}()
		for {
			var msgOut *C.rebar_msg_t
			rc := C.rebar_recv(
				entry.runtime.ptr,
				pid,
				&msgOut,
				C.int64_t(100), // 100ms timeout
			)
			if int(rc) == errTimeout {
				continue
			}
			if int(rc) != errOK {
				return // process stopped or error
			}
			data := C.rebar_msg_data((*C.rebar_msg_t)(unsafe.Pointer(msgOut)))
			length := C.rebar_msg_len((*C.rebar_msg_t)(unsafe.Pointer(msgOut)))
			goBytes := C.GoBytes(unsafe.Pointer(data), C.int(length))
			C.rebar_msg_free((*C.rebar_msg_t)(unsafe.Pointer(msgOut)))

			msg := &Msg{Data: goBytes}
			entry.actor.HandleMessage(ctx, msg)
		}
	}()
}
