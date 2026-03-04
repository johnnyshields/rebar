package rebar

import (
	"sync"
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

// --- Concurrency tests ---

func TestConcurrentClose(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}

	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			rt.Close()
		}()
	}
	wg.Wait()
}

func TestUseAfterClose(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	rt.Close()

	if err := rt.Send(Pid{NodeID: 1, LocalID: 1}, []byte("x")); err != ErrRuntimeClosed {
		t.Fatalf("expected ErrRuntimeClosed, got %v", err)
	}
	if err := rt.Register("foo", Pid{NodeID: 1, LocalID: 1}); err != ErrRuntimeClosed {
		t.Fatalf("expected ErrRuntimeClosed, got %v", err)
	}
	if _, err := rt.Whereis("foo"); err != ErrRuntimeClosed {
		t.Fatalf("expected ErrRuntimeClosed, got %v", err)
	}
	if err := rt.SendNamed("foo", []byte("x")); err != ErrRuntimeClosed {
		t.Fatalf("expected ErrRuntimeClosed, got %v", err)
	}
	if _, err := rt.SpawnActor(&testActor{}); err != ErrRuntimeClosed {
		t.Fatalf("expected ErrRuntimeClosed, got %v", err)
	}
}

func TestConcurrentSpawnActor(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	var wg sync.WaitGroup
	pids := make([]Pid, 10)
	errs := make([]error, 10)
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			pids[idx], errs[idx] = rt.SpawnActor(&testActor{})
		}(i)
	}
	wg.Wait()

	seen := make(map[uint64]bool)
	for i, err := range errs {
		if err != nil {
			t.Fatalf("SpawnActor[%d] failed: %v", i, err)
		}
		if seen[pids[i].LocalID] {
			t.Fatalf("duplicate PID local_id: %d", pids[i].LocalID)
		}
		seen[pids[i].LocalID] = true
	}
}

func TestConcurrentSend(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	// Sending to invalid PIDs concurrently should not race.
	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			_ = rt.Send(Pid{NodeID: 1, LocalID: uint64(999000 + idx)}, []byte("msg"))
		}(i)
	}
	wg.Wait()
}

func TestEmptyNameErrors(t *testing.T) {
	rt, err := NewRuntime(1)
	if err != nil {
		t.Fatalf("NewRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.Register("", Pid{NodeID: 1, LocalID: 1}); err == nil {
		t.Fatal("expected error for empty name in Register")
	} else if _, ok := err.(*InvalidNameError); !ok {
		t.Fatalf("expected InvalidNameError, got %T: %v", err, err)
	}

	if _, err := rt.Whereis(""); err == nil {
		t.Fatal("expected error for empty name in Whereis")
	} else if _, ok := err.(*InvalidNameError); !ok {
		t.Fatalf("expected InvalidNameError, got %T: %v", err, err)
	}

	if err := rt.SendNamed("", []byte("x")); err == nil {
		t.Fatal("expected error for empty name in SendNamed")
	} else if _, ok := err.(*InvalidNameError); !ok {
		t.Fatalf("expected InvalidNameError, got %T: %v", err, err)
	}
}

func TestCheckErrorNullPtr(t *testing.T) {
	err := checkError(-1)
	if err == nil {
		t.Fatal("expected error for null pointer code")
	}
	re, ok := err.(*RebarError)
	if !ok {
		t.Fatalf("expected *RebarError, got %T", err)
	}
	if re.Code != errNullPtr {
		t.Fatalf("expected code %d, got %d", errNullPtr, re.Code)
	}
}
