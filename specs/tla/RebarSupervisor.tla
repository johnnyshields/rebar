-------------------------- MODULE RebarSupervisor --------------------------
\* Supervisor restart strategy races for Rebar
\* Source: crates/rebar-core/src/supervisor/engine.rs
\*
\* Models the three restart strategies (OneForOne, OneForAll, RestForOne)
\* and the stale-PID check that prevents double-processing of exit events.
\*
\* Run: tlc RebarSupervisor -config RebarSupervisor.cfg -workers 4
\*
\* Key invariants:
\*   NoDoubleLiveChild    -- OneForAll race: wrong PID check → two tasks per child
\*   NoPIDSharing         -- no two children share the same PID
\*   StaleExitSafety      -- stale PID exit must be a no-op (action property)
\*   RestartLimitRespected -- off-by-one: <= max vs < max
\*   PIDMonotonicity       -- no PID reuse (next_pid is strictly increasing)
\*   NoRestartAfterShutdown -- missing guard in shutdown path

EXTENDS Integers, FiniteSets, Sequences

CONSTANT Children       \* Set of child indices (e.g., {0, 1, 2} -- integers for ordering)
CONSTANT MaxPid         \* Maximum PID value (e.g., 10)
CONSTANT MaxRestarts    \* Max restarts before escalation (e.g., 2)
CONSTANT Strategy       \* "OneForOne" | "OneForAll" | "RestForOne"
CONSTANT None           \* Sentinel for "no PID" (e.g., 0)

ASSUME Strategy \in {"OneForOne", "OneForAll", "RestForOne"}
ASSUME MaxRestarts >= 0

VARIABLES
  child_pid,      \* [i \in Children] -> 1..MaxPid \union {None}
  child_status,   \* [i \in Children] -> {"running", "stopping", "stopped"}
  sup_queue,      \* Seq of exit records {index, pid, reason}
  sup_state,      \* "running" | "shutdown"
  restart_count,  \* Nat -- total restarts performed
  next_pid,       \* Nat -- next PID to assign (monotonically increasing)
  pending_exits   \* SET of {index, pid, reason} not yet in sup_queue

vars == <<child_pid, child_status, sup_queue, sup_state,
          restart_count, next_pid, pending_exits>>

----
(****************************************************************************)
(* Type definitions                                                         *)
(****************************************************************************)

Reasons == {"normal", "error"}

TypeInvariant ==
    /\ \A i \in Children :
           child_pid[i] = None \/ child_pid[i] \in 1..MaxPid
    /\ \A i \in Children :
           child_status[i] \in {"running", "stopping", "stopped"}
    /\ \A k \in 1..Len(sup_queue) :
           sup_queue[k].index \in Children /\
           (sup_queue[k].pid = None \/ sup_queue[k].pid \in 1..MaxPid) /\
           sup_queue[k].reason \in Reasons
    /\ sup_state \in {"running", "shutdown"}
    /\ restart_count \in 0..(MaxRestarts + 1)
    /\ next_pid \in 1..MaxPid

----
(****************************************************************************)
(* Helper operators                                                         *)
(****************************************************************************)

\* True if we are under the restart limit.
\* Rust checks AFTER pushing: (restart_times.len() as u32) <= self.max_restarts
\* TLA+ checks BEFORE incrementing, so we use strict < to match:
\*   Rust: push, len=N+1, N+1 <= max  <==>  TLA+: count=N, N < max, then count'=N+1
UnderRestartLimit == restart_count < MaxRestarts

\* Children with index strictly greater than i (for RestForOne successors)
SuccessorsOf(i) ==
    {j \in Children : j > i}

\* Number of children to restart in RestForOne (failed + successors)
RestForOneCount(i) == 1 + Cardinality(SuccessorsOf(i))

----
(****************************************************************************)
(* Initial state: supervisor starts all children in order                  *)
(****************************************************************************)

\* Children = {0, 1, 2} get PIDs 1, 2, 3; next_pid starts at 4.
Init ==
    /\ child_pid     = [i \in Children |-> i + 1]
    /\ child_status  = [i \in Children |-> "running"]
    /\ sup_queue     = <<>>
    /\ sup_state     = "running"
    /\ restart_count = 0
    /\ next_pid      = Cardinality(Children) + 1
    /\ pending_exits = {}

----
(****************************************************************************)
(* Actions                                                                  *)
(****************************************************************************)

\* [ACTION] A running child exits (crash or normal completion).
\* Exit record goes to pending_exits (async channel to supervisor).
\* Mirrors the tokio::spawn wrapper that sends ChildExited on task end.
ChildExitsNaturally(i, reason) ==
    /\ sup_state = "running"
    /\ child_status[i] = "running"
    /\ child_pid[i] # None
    /\ LET rec == [index |-> i, pid |-> child_pid[i], reason |-> reason]
       IN  pending_exits' = pending_exits \union {rec}
    /\ child_status' = [child_status EXCEPT ![i] = "stopped"]
    /\ UNCHANGED <<child_pid, sup_queue, sup_state, restart_count, next_pid>>

\* [ACTION] A child in "stopping" state exits (after shutdown signal).
\* Generates a normal exit record.
StoppingChildExits(i) ==
    /\ sup_state = "running"
    /\ child_status[i] = "stopping"
    /\ child_pid[i] # None
    /\ LET rec == [index |-> i, pid |-> child_pid[i], reason |-> "normal"]
       IN  pending_exits' = pending_exits \union {rec}
    /\ child_status' = [child_status EXCEPT ![i] = "stopped"]
    /\ UNCHANGED <<child_pid, sup_queue, sup_state, restart_count, next_pid>>

\* [ACTION] An exit record is moved from pending_exits to sup_queue.
\* Models async message delivery (unbounded channel).
DeliverExitToSupervisor(e) ==
    /\ e \in pending_exits
    /\ sup_queue'     = Append(sup_queue, e)
    /\ pending_exits' = pending_exits \ {e}
    /\ UNCHANGED <<child_pid, child_status, sup_state, restart_count, next_pid>>

\* [ACTION] Supervisor dequeues a STALE exit (PID mismatch → no-op).
\* Mirrors: if state.children[index].pid != Some(pid) { continue; }
\* This is the critical guard that prevents double-restart in OneForAll.
SupervisorProcessesStaleExit ==
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e == Head(sup_queue)
       IN  /\ child_pid[e.index] # e.pid    \* stale: PID mismatch
           /\ sup_queue' = Tail(sup_queue)
           /\ UNCHANGED <<child_pid, child_status, sup_state,
                          restart_count, next_pid, pending_exits>>

\* [ACTION] Supervisor dequeues a normal (non-error) exit: no restart.
SupervisorProcessesNormalExit ==
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e == Head(sup_queue)
       IN  /\ child_pid[e.index] = e.pid    \* valid PID
           /\ e.reason = "normal"
           /\ sup_queue' = Tail(sup_queue)
           /\ child_pid' = [child_pid EXCEPT ![e.index] = None]
           /\ UNCHANGED <<child_status, sup_state, restart_count,
                          next_pid, pending_exits>>

\* [ACTION] Supervisor processes OneForOne restart.
\* Pops queue, verifies PID, restarts only the failed child.
\* Fresh PID = next_pid (then next_pid increments).
SupervisorProcessesOneForOne ==
    /\ Strategy = "OneForOne"
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e == Head(sup_queue)
       IN  /\ child_pid[e.index] = e.pid    \* valid PID — not stale
           /\ e.reason = "error"
           /\ UnderRestartLimit
           /\ next_pid <= MaxPid
           /\ sup_queue'      = Tail(sup_queue)
           /\ restart_count'  = restart_count + 1
           /\ next_pid'       = next_pid + 1
           /\ child_pid'      = [child_pid    EXCEPT ![e.index] = next_pid]
           /\ child_status'   = [child_status EXCEPT ![e.index] = "running"]
           /\ UNCHANGED <<sup_state, pending_exits>>

\* [ACTION] Supervisor processes OneForAll restart.
\* Stops all other running children (they become "stopping" and will
\* generate exit records), then assigns fresh PIDs to ALL children.
\*
\* The race modeled here: after assigning fresh PIDs, previously queued
\* exit records for "stopping" children now have stale PIDs. Without the
\* stale-PID guard, those exits would trigger additional restart cycles.
\* With the guard, SupervisorProcessesStaleExit handles them as no-ops.
SupervisorProcessesOneForAll ==
    /\ Strategy = "OneForAll"
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e == Head(sup_queue)
           n == Cardinality(Children)
       IN  /\ child_pid[e.index] = e.pid    \* valid PID
           /\ e.reason = "error"
           /\ UnderRestartLimit
           /\ next_pid + n - 1 <= MaxPid    \* enough PID space for all children
           \* Assign fresh PIDs: child i gets next_pid + i
           /\ sup_queue'     = Tail(sup_queue)
           /\ restart_count' = restart_count + 1
           /\ next_pid'      = next_pid + n
           /\ child_pid'     = [i \in Children |-> next_pid + i]
           /\ child_status'  = [i \in Children |-> "running"]
           /\ UNCHANGED <<sup_state, pending_exits>>

\* [ACTION] Supervisor processes RestForOne restart.
\* Stops children at index > failed child, then restarts failed + successors.
\* Ordering: failed child gets PID next_pid, successors get next_pid + offset.
SupervisorProcessesRestForOne ==
    /\ Strategy = "RestForOne"
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e    == Head(sup_queue)
           k    == RestForOneCount(e.index)
       IN  /\ child_pid[e.index] = e.pid    \* valid PID
           /\ e.reason = "error"
           /\ UnderRestartLimit
           /\ next_pid + k - 1 <= MaxPid    \* enough PID space
           /\ sup_queue'     = Tail(sup_queue)
           /\ restart_count' = restart_count + 1
           /\ next_pid'      = next_pid + k
           \* Failed child gets next_pid; successors get next_pid + (j - e.index)
           /\ child_pid' =
                [i \in Children |->
                    IF i = e.index
                    THEN next_pid
                    ELSE IF i \in SuccessorsOf(e.index)
                         THEN next_pid + (i - e.index)
                         ELSE child_pid[i]]
           /\ child_status' =
                [i \in Children |->
                    IF i = e.index \/ i \in SuccessorsOf(e.index)
                    THEN "running"
                    ELSE child_status[i]]
           /\ UNCHANGED <<sup_state, pending_exits>>

\* [ACTION] Restart limit exceeded: supervisor shuts down all children.
\* Mirrors: if !check_restart_limit() { shutdown_all_children(); break; }
SupervisorEscalates ==
    /\ Len(sup_queue) > 0
    /\ sup_state = "running"
    /\ LET e == Head(sup_queue)
       IN  /\ child_pid[e.index] = e.pid
           /\ e.reason = "error"
           /\ ~UnderRestartLimit
           /\ sup_queue'    = Tail(sup_queue)
           /\ sup_state'    = "shutdown"
           /\ child_pid'    = [i \in Children |-> None]
           /\ child_status' = [i \in Children |-> "stopped"]
           /\ UNCHANGED <<restart_count, next_pid, pending_exits>>

\* [ACTION] External shutdown request (SupervisorHandle::shutdown()).
SupervisorShutdown ==
    /\ sup_state = "running"
    /\ sup_state'    = "shutdown"
    /\ child_pid'    = [i \in Children |-> None]
    /\ child_status' = [i \in Children |-> "stopped"]
    /\ UNCHANGED <<sup_queue, restart_count, next_pid, pending_exits>>

----
(****************************************************************************)
(* Specification                                                            *)
(****************************************************************************)

Next ==
    \/ \E i \in Children, r \in Reasons : ChildExitsNaturally(i, r)
    \/ \E i \in Children                : StoppingChildExits(i)
    \/ \E e \in pending_exits           : DeliverExitToSupervisor(e)
    \/ SupervisorProcessesStaleExit
    \/ SupervisorProcessesNormalExit
    \/ SupervisorProcessesOneForOne
    \/ SupervisorProcessesOneForAll
    \/ SupervisorProcessesRestForOne
    \/ SupervisorEscalates
    \/ SupervisorShutdown

Fairness ==
    /\ WF_vars(SupervisorProcessesOneForOne)
    /\ WF_vars(SupervisorProcessesOneForAll)
    /\ WF_vars(SupervisorProcessesRestForOne)
    /\ WF_vars(SupervisorProcessesStaleExit)
    /\ WF_vars(SupervisorProcessesNormalExit)
    /\ WF_vars(SupervisorEscalates)
    /\ WF_vars(\E e \in pending_exits : DeliverExitToSupervisor(e))

Spec == Init /\ [][Next]_vars /\ Fairness

----
(****************************************************************************)
(* Safety invariants                                                        *)
(****************************************************************************)

\* Every running child must have exactly one PID assigned.
\* Bug hunted: OneForAll race with wrong PID check allows two running
\* entries for the same child index (double-task per slot).
NoDoubleLiveChild ==
    \A i \in Children :
        child_status[i] = "running" => child_pid[i] # None

\* When the supervisor dequeues a stale exit (PID mismatch), child_pid and
\* child_status must remain unchanged. The guard `sup_queue' = Tail(sup_queue)`
\* restricts this to steps that actually consume the queue head, so unrelated
\* actions (e.g., a child exiting naturally) don't trigger the property.
\* Bug hunted: without the PID guard, stale exits trigger spurious restarts.
StaleExitSafety ==
    [][Len(sup_queue) > 0 /\ sup_queue' = Tail(sup_queue) /\
       child_pid[Head(sup_queue).index] # Head(sup_queue).pid
       => child_pid' = child_pid /\ child_status' = child_status]_vars

\* No two distinct children share the same non-None PID.
\* Bug hunted: OneForAll race where two children get the same PID due to
\* non-atomic PID counter increment.
NoPIDSharing ==
    \A i, j \in Children :
        i # j /\ child_pid[i] # None /\ child_pid[j] # None =>
        child_pid[i] # child_pid[j]

\* Restart count never exceeds MaxRestarts unless supervisor has shut down.
\* Bug hunted: <= vs < off-by-one in check_restart_limit.
\* The Rust code allows restart_times.len() <= max_restarts, so with
\* MaxRestarts=2 we permit restart_count up to 2 before escalating.
RestartLimitRespected ==
    restart_count <= MaxRestarts \/ sup_state = "shutdown"

\* Every live child PID is strictly less than next_pid.
\* Guarantees no PID was reused from a previous assignment.
\* Bug hunted: relaxed atomics issue causing PID counter reset.
PIDMonotonicity ==
    \A i \in Children :
        child_pid[i] # None => child_pid[i] < next_pid

\* After shutdown, no children remain in running state.
\* Bug hunted: missing guard in shutdown path allows races where a child
\* is restarted after the shutdown decision is made.
NoRestartAfterShutdown ==
    sup_state = "shutdown" =>
        \A i \in Children : child_status[i] # "running"

----
(****************************************************************************)
(* Liveness properties                                                      *)
(****************************************************************************)

\* A stopped child is eventually restarted (or supervisor shuts down).
ChildEventuallyRestarted ==
    \A i \in Children :
        (child_status[i] = "stopped" /\ sup_state = "running") ~>
        (child_status[i] = "running" \/ sup_state = "shutdown")

\* The supervisor queue eventually drains.
QueueEventuallyDrains ==
    [](Len(sup_queue) > 0 ~> Len(sup_queue) = 0)

----
(****************************************************************************)
(* State constraint                                                         *)
(****************************************************************************)

StateConstraint ==
    /\ next_pid <= MaxPid
    /\ restart_count <= MaxRestarts + 1
    /\ Len(sup_queue) <= Cardinality(Children) + 2

=============================================================================
