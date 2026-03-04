package rebar

// #include "rebar_ffi.h"
import "C"
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
