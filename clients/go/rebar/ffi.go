package rebar

/*
#cgo LDFLAGS: -lrebar_ffi
#include <stdlib.h>
#include "rebar_ffi.h"

// Go cannot pass Go function pointers directly to C.
// C trampoline that calls back into Go.
extern void goRebarProcessCallback(rebar_pid_t pid);
static inline int32_t rebar_spawn_go(rebar_runtime_t *rt, rebar_pid_t *pid_out) {
    return rebar_spawn(rt, goRebarProcessCallback, pid_out);
}
*/
import "C"
