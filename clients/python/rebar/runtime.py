"""Runtime and Context for the Rebar Python client."""

from __future__ import annotations

import ctypes
import threading
from typing import TYPE_CHECKING, Optional

from . import _ffi
from .errors import TimeoutError, check_error
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

    def recv(self, timeout_ms: int = -1) -> Optional[bytes]:
        """Receive a message from this process's mailbox.

        Args:
            timeout_ms: -1 = block forever, 0 = non-blocking, >0 = ms timeout.
        """
        return self._runtime.recv(self._pid, timeout_ms)

    def unregister(self, name: str) -> None:
        """Unregister a name."""
        self._runtime.unregister(name)


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

    def unregister(self, name: str) -> None:
        """Unregister a name."""
        name_bytes = name.encode("utf-8")
        rc = _ffi._lib.rebar_unregister(
            self._ptr, name_bytes, len(name_bytes),
        )
        check_error(rc)

    def recv(self, pid: Pid, timeout_ms: int = -1) -> Optional[bytes]:
        """Receive a message from an FFI-spawned process's mailbox.

        Args:
            pid: The process to receive from.
            timeout_ms: -1 = block forever, 0 = non-blocking, >0 = ms timeout.

        Returns:
            The message payload as bytes, or None on timeout.

        Raises:
            TimeoutError: If timeout_ms >= 0 and no message arrived in time.
        """
        ffi_pid = _ffi.RebarPid(pid.node_id, pid.local_id)
        msg_out = ctypes.c_void_p()
        rc = _ffi._lib.rebar_recv(self._ptr, ffi_pid, ctypes.byref(msg_out), timeout_ms)
        if rc == -5:  # REBAR_ERR_TIMEOUT
            raise TimeoutError()
        check_error(rc)
        try:
            data_ptr = _ffi._lib.rebar_msg_data(msg_out)
            data_len = _ffi._lib.rebar_msg_len(msg_out)
            if data_len > 0:
                return bytes(ctypes.cast(data_ptr, ctypes.POINTER(ctypes.c_uint8 * data_len)).contents)
            return b""
        finally:
            _ffi._lib.rebar_msg_free(msg_out)

    def stop_process(self, pid: Pid) -> None:
        """Stop an FFI-spawned process."""
        ffi_pid = _ffi.RebarPid(pid.node_id, pid.local_id)
        rc = _ffi._lib.rebar_stop_process(self._ptr, ffi_pid)
        check_error(rc)

    def spawn_actor(self, actor: Actor) -> Pid:
        """Spawn a new process backed by the given Actor.

        The actor's handle_message is called with a None message on startup,
        then a daemon thread polls for messages and dispatches them.
        """
        pid_out = _ffi.RebarPid()
        runtime_ref = self

        @_ffi.PROCESS_CALLBACK
        def callback(ffi_pid: _ffi.RebarPid, _context: int) -> None:
            pid = Pid(node_id=ffi_pid.node_id, local_id=ffi_pid.local_id)
            ctx = Context(pid, runtime_ref)
            actor.handle_message(ctx, None)

            # Start a daemon thread for the message loop
            def message_loop() -> None:
                while True:
                    try:
                        msg_bytes = runtime_ref.recv(pid, timeout_ms=100)
                        actor.handle_message(ctx, msg_bytes)
                    except TimeoutError:
                        continue
                    except Exception:
                        break

            t = threading.Thread(target=message_loop, daemon=True)
            t.start()

        # Keep reference to prevent GC of the callback
        self._callbacks.append(callback)

        rc = _ffi._lib.rebar_spawn(self._ptr, callback, ctypes.byref(pid_out), 0)
        check_error(rc)
        return Pid(node_id=pid_out.node_id, local_id=pid_out.local_id)
