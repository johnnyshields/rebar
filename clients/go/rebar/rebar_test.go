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
