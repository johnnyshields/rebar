package rebar

// #include "rebar_ffi.h"
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
