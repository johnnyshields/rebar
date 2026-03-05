------------------------------ MODULE RebarSwim ------------------------------
\* SWIM failure detector incarnation-number protocol for Rebar
\* Sources: crates/rebar-cluster/src/swim/member.rs
\*          crates/rebar-cluster/src/swim/detector.rs
\*
\* Key asymmetry in the Rust implementation:
\*   Member::suspect(inc)  fires if inc >= self.incarnation  (non-strict, >=)
\*   Member::alive(inc)    fires if inc >  self.incarnation  (strict,     >)
\*   FailureDetector::record_ack() does member.incarnation += 1 directly,
\*     bypassing gossip (no Alive message is broadcast).
\*
\* Bug scenario modeled:
\*   1. N updates M's incarnation to K+1 via record_ack (local only, no gossip)
\*   2. Q, which learned M.inc=K+1 via normal Alive gossip, suspects M → gossips Suspect(K+1)
\*   3. Suspect(K+1) arrives at N: K+1 >= K+1 → TRUE → re-suspects M
\*   This traps a live node in Suspect state despite a confirmed ACK.

EXTENDS Integers, FiniteSets

CONSTANT Nodes    \* Set of node IDs (e.g., {1, 2, 3})
CONSTANT MaxInc   \* Upper bound on incarnation numbers (e.g., 3)

VARIABLES
  node_state,      \* [n \in Nodes][m \in Nodes] -> {"Alive", "Suspect", "Dead"}
  incarnation,     \* [n \in Nodes][m \in Nodes] -> 0..MaxInc  (n's view of m's inc)
  self_inc,        \* [n \in Nodes] -> 0..MaxInc  (n's own incarnation for refutation)
  gossip_pool,     \* SET of {type, target, inc} gossip messages
  is_alive,        \* [n \in Nodes] -> BOOLEAN  (ground truth: is node n alive?)
  suspect_expired  \* [n \in Nodes][m \in Nodes] -> BOOLEAN  (suspect timer fired)

vars == <<node_state, incarnation, self_inc, gossip_pool, is_alive, suspect_expired>>

----
(****************************************************************************)
(* Type definitions                                                         *)
(****************************************************************************)

States == {"Alive", "Suspect", "Dead"}

GossipMsg(type, target, inc) == [type |-> type, target |-> target, inc |-> inc]

TypeInvariant ==
    /\ \A n \in Nodes : \A m \in Nodes : node_state[n][m] \in States
    /\ \A n \in Nodes : \A m \in Nodes :
           incarnation[n][m] \in 0..MaxInc
    /\ \A n \in Nodes : self_inc[n] \in 0..MaxInc
    /\ \A n \in Nodes : is_alive[n] \in BOOLEAN
    /\ \A n \in Nodes : \A m \in Nodes : suspect_expired[n][m] \in BOOLEAN

----
(****************************************************************************)
(* Initial state                                                            *)
(****************************************************************************)

Init ==
    /\ node_state     = [n \in Nodes |-> [m \in Nodes |->
                            IF n = m THEN "Alive" ELSE "Alive"]]
    /\ incarnation    = [n \in Nodes |-> [m \in Nodes |-> 0]]
    /\ self_inc       = [n \in Nodes |-> 0]
    /\ gossip_pool    = {}
    /\ is_alive       = [n \in Nodes |-> TRUE]
    /\ suspect_expired = [n \in Nodes |-> [m \in Nodes |-> FALSE]]

----
(****************************************************************************)
(* Actions                                                                  *)
(****************************************************************************)

\* [ACTION] Node n fails to get a probe response from m.
\* Mirrors FailureDetector::record_nack -> member.suspect(member.incarnation)
\* Suspect guard on m.incarnation: uses >= (non-strict).
\* Generates Suspect gossip at the current observed incarnation.
NodeSuspectsOther(n, m) ==
    /\ n # m
    /\ is_alive[n]
    /\ node_state[n][m] # "Dead"
    /\ node_state[n][m] = "Alive"      \* only suspect from Alive (nack on alive member)
    \* Mirrors: member.suspect(member.incarnation)
    \* >= guard: incarnation[n][m] >= incarnation[n][m] always TRUE
    /\ node_state'   = [node_state EXCEPT ![n][m] = "Suspect"]
    /\ gossip_pool'  = gossip_pool \union
                       {GossipMsg("Suspect", m, incarnation[n][m])}
    /\ UNCHANGED <<incarnation, self_inc, is_alive, suspect_expired>>

\* [ACTION] Node m (alive) sees a Suspect gossip about itself with inc >= self_inc.
\* Refutes by bumping self_inc and broadcasting Alive(self_inc + 1).
\* Mirrors the refutation path in the SWIM protocol.
NodeRefutesSuspicion(m) ==
    /\ is_alive[m]
    /\ self_inc[m] < MaxInc
    /\ \E msg \in gossip_pool :
           /\ msg.type = "Suspect"
           /\ msg.target = m
           /\ msg.inc >= self_inc[m]    \* suspect guard fires for self
    /\ self_inc'    = [self_inc EXCEPT ![m] = self_inc[m] + 1]
    /\ gossip_pool' = gossip_pool \union
                      {GossipMsg("Alive", m, self_inc[m] + 1)}
    /\ UNCHANGED <<node_state, incarnation, is_alive, suspect_expired>>

\* [ACTION] Node n receives an Alive gossip message.
\* Mirrors Member::alive(inc): fires only if inc > self.incarnation (STRICT >).
NodeReceivesAliveGossip(n, msg) ==
    /\ msg \in gossip_pool
    /\ msg.type = "Alive"
    /\ n # msg.target
    /\ is_alive[n]
    /\ node_state[n][msg.target] # "Dead"
    \* Strict > guard (correct per source)
    /\ msg.inc > incarnation[n][msg.target]
    /\ node_state'   = [node_state EXCEPT ![n][msg.target] = "Alive"]
    /\ incarnation'  = [incarnation EXCEPT ![n][msg.target] = msg.inc]
    /\ UNCHANGED <<self_inc, gossip_pool, is_alive, suspect_expired>>

\* [ACTION] Node n receives a Suspect gossip message.
\* Mirrors Member::suspect(inc): fires if inc >= self.incarnation (NON-STRICT >=).
NodeReceivesSuspectGossip(n, msg) ==
    /\ msg \in gossip_pool
    /\ msg.type = "Suspect"
    /\ n # msg.target
    /\ is_alive[n]
    /\ node_state[n][msg.target] # "Dead"
    \* Non-strict >= guard (key asymmetry vs alive)
    /\ msg.inc >= incarnation[n][msg.target]
    /\ node_state'   = [node_state EXCEPT ![n][msg.target] = "Suspect"]
    /\ incarnation'  = [incarnation EXCEPT ![n][msg.target] = msg.inc]
    /\ UNCHANGED <<self_inc, gossip_pool, is_alive, suspect_expired>>

\* [ACTION] Node n gets a direct ACK from m (probe succeeded).
\* Mirrors FailureDetector::record_ack: bumps incarnation locally by 1,
\* sets state to Alive. NO gossip is generated. This is the critical
\* bypass that creates the re-suspect vulnerability.
NodeRecordAck(n, m) ==
    /\ n # m
    /\ is_alive[n]
    /\ node_state[n][m] # "Dead"
    /\ incarnation[n][m] < MaxInc
    \* record_ack sets Alive and bumps incarnation (only if was Suspect)
    /\ node_state[n][m] = "Suspect"
    /\ node_state'   = [node_state EXCEPT ![n][m] = "Alive"]
    /\ incarnation'  = [incarnation EXCEPT ![n][m] = incarnation[n][m] + 1]
    /\ UNCHANGED <<self_inc, gossip_pool, is_alive, suspect_expired>>

\* [ACTION] Suspect timer fires at n for m: transition Suspect -> Dead.
\* Mirrors FailureDetector::check_suspect_timeouts.
\* A node does not suspect itself (n # m).
\*
\* Two timing guards model the real-world assumption that suspect_timeout is
\* long enough for (a) gossip to propagate to the observer AND (b) the
\* suspected node to refute if it is still alive:
\*   Guard 1 (local):  n has processed all available Alive gossip for m.
\*   Guard 2 (global): m cannot refute — either dead, at MaxInc, or no
\*                      suspect gossip left to refute. Uses ground-truth
\*                      is_alive[m] to model the timing assumption without
\*                      adding explicit clocks.
SuspectTimeoutFires(n, m) ==
    /\ n # m
    /\ is_alive[n]
    /\ node_state[n][m] = "Suspect"
    /\ ~suspect_expired[n][m]
    \* Guard 1: n has consumed all pending Alive gossip for m
    /\ ~\E msg \in gossip_pool :
           msg.type = "Alive" /\ msg.target = m /\ msg.inc > incarnation[n][m]
    \* Guard 2: m has no pending refutation (dead, exhausted, or already refuted)
    /\ ~(is_alive[m] /\ self_inc[m] < MaxInc /\
         \E msg \in gossip_pool :
            msg.type = "Suspect" /\ msg.target = m /\ msg.inc >= self_inc[m])
    /\ suspect_expired' = [suspect_expired EXCEPT ![n][m] = TRUE]
    /\ node_state'      = [node_state EXCEPT ![n][m] = "Dead"]
    /\ UNCHANGED <<incarnation, self_inc, gossip_pool, is_alive>>

\* [ACTION] Node m crashes (ground truth only).
\* No protocol state changes; node simply stops responding.
NodeCrashes(m) ==
    /\ is_alive[m]
    /\ is_alive' = [is_alive EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<node_state, incarnation, self_inc, gossip_pool, suspect_expired>>

----
(****************************************************************************)
(* Specification                                                            *)
(****************************************************************************)

Next ==
    \/ \E n, m \in Nodes : NodeSuspectsOther(n, m)
    \/ \E m \in Nodes    : NodeRefutesSuspicion(m)
    \/ \E n \in Nodes, msg \in gossip_pool : NodeReceivesAliveGossip(n, msg)
    \/ \E n \in Nodes, msg \in gossip_pool : NodeReceivesSuspectGossip(n, msg)
    \/ \E n, m \in Nodes : NodeRecordAck(n, m)
    \/ \E n, m \in Nodes : SuspectTimeoutFires(n, m)
    \/ \E m \in Nodes    : NodeCrashes(m)

Spec == Init /\ [][Next]_vars

----
(****************************************************************************)
(* Safety invariants                                                        *)
(****************************************************************************)

\* A live node with remaining incarnation budget must not be declared Dead
\* at every observer. The >= vs > asymmetry in suspect/alive guards causes
\* repeated re-suspecting that burns through incarnations; once self_inc
\* reaches MaxInc the node can no longer refute and may be permanently killed.
\* This invariant holds while budget remains — exhaustion IS the bug.
AliveNodeNotPermanentlyDead ==
    \A m \in Nodes : is_alive[m] /\ self_inc[m] < MaxInc =>
        \E n \in Nodes : n # m /\ node_state[n][m] # "Dead"

\* Incarnation numbers at a given observer never decrease for a given target.
\* Bug: out-of-order gossip delivery overwrites a higher incarnation with lower.
\* This is an action property (checked over transitions), not a state invariant.
IncarnationMonotonicity ==
    [][\A n \in Nodes : \A m \in Nodes :
        incarnation'[n][m] >= incarnation[n][m]]_vars

\* Once Dead, a node's state never transitions back to Alive or Suspect.
\* Mirrors the Dead guard at the top of suspect() and alive().
DeadIsTerminal ==
    [][
        \A n \in Nodes : \A m \in Nodes :
            node_state[n][m] = "Dead" =>
            node_state'[n][m] = "Dead"
    ]_vars

\* Cross-suspicion at equal incarnation (inc=0) can always be resolved.
\* When nodes mutually suspect each other at the same incarnation,
\* refutation with a higher incarnation should break the deadlock.
\* Checked as: if all nodes see m as Suspect/Alive (not Dead) and m is alive,
\* then m can still refute.
MutualSuspicionCanResolve ==
    \A m \in Nodes :
        (is_alive[m] /\ self_inc[m] < MaxInc) =>
        ~(\A n \in Nodes :
            n # m => node_state[n][m] = "Dead")

----
(****************************************************************************)
(* State constraint                                                         *)
(****************************************************************************)

StateConstraint ==
    /\ \A n \in Nodes : self_inc[n] <= MaxInc
    /\ \A n \in Nodes : \A m \in Nodes : incarnation[n][m] <= MaxInc
    /\ Cardinality(gossip_pool) <= Cardinality(Nodes) * MaxInc * 2

=============================================================================
