--------------------------- MODULE RebarRegistry ---------------------------
\* OR-Set CRDT registry for Rebar
\* Source: crates/rebar-cluster/src/registry/orset.rs
\*
\* Models the merge_delta logic where:
\*   Add: rejected if tag is tombstoned (tombstone check FIRST)
\*   Remove: tombstones the tag and removes the entry
\*
\* Key bug hunted: if tombstone check occurs AFTER inserting the entry,
\* a Remove-then-Add sequence resurrects a deleted registration.

EXTENDS Integers, FiniteSets

CONSTANT Nodes         \* Set of node IDs (e.g., {1, 2})
CONSTANT Names         \* Set of registry names (e.g., {"a", "b", "c"})
CONSTANT Tags          \* Abstract unique tags (e.g., {t1, t2, t3})
CONSTANT MaxTimestamp  \* Upper bound on timestamps (e.g., 2)

VARIABLES
  reg_entries,   \* [n \in Nodes] -> [name \in Names] -> SUBSET of entry records
  tombstones,    \* [n \in Nodes] -> SUBSET Tags
  delta_pool     \* SET of undelivered delta records (lossy-free broadcast model)

vars == <<reg_entries, tombstones, delta_pool>>

----
(****************************************************************************)
(* Type definitions                                                         *)
(****************************************************************************)

\* An entry record mirrors RegistryEntry in the source
EntryOf(tg, ts, nid) == [tag |-> tg, ts |-> ts, node_id |-> nid]

AddDelta(name, tg, ts, nid) ==
    [type |-> "Add", name |-> name, tag |-> tg, ts |-> ts, node_id |-> nid]

RemoveDelta(name, tg) ==
    [type |-> "Remove", name |-> name, tag |-> tg]

TypeInvariant ==
    /\ \A n \in Nodes : \A name \in Names :
           \A e \in reg_entries[n][name] :
               /\ e.tag \in Tags
               /\ e.ts \in 1..MaxTimestamp
               /\ e.node_id \in Nodes
    /\ \A n \in Nodes : tombstones[n] \subseteq Tags

----
(****************************************************************************)
(* LWW winner: highest ts wins; ties broken by highest node_id             *)
(* Mirrors Registry::lookup max_by logic                                   *)
(****************************************************************************)

\* Non-strict >= on node_id handles the edge case where delta merging
\* creates two entries with identical (ts, node_id) but different tags
\* (e.g., register at ts=1, unregister, re-register at ts=1 with new tag).
\* CHOOSE resolves ties deterministically, matching Rust's max_by behavior.
Beats(a, b) ==
    a.ts > b.ts \/ (a.ts = b.ts /\ a.node_id >= b.node_id)

Winner(entries) ==
    CHOOSE e \in entries :
        \A other \in entries : e = other \/ Beats(e, other)

----
(****************************************************************************)
(* Initial state                                                            *)
(****************************************************************************)

Init ==
    /\ reg_entries = [n \in Nodes |-> [name \in Names |-> {}]]
    /\ tombstones  = [n \in Nodes |-> {}]
    /\ delta_pool  = {}

----
(****************************************************************************)
(* Actions                                                                  *)
(****************************************************************************)

\* [ACTION] Node n locally registers name with a new tag at timestamp ts.
\* Generates an Add delta for the pool.
\* Preconditions:
\*   - tag globally unused (models UUID v4 uniqueness -- no two registrations
\*     across any node may share a tag, including tombstoned tags)
\*   - no existing entry with same (ts, node_id) to ensure Winner is well-defined
Register(n, name, tg, ts) ==
    /\ \A m \in Nodes : \A nm \in Names : \A e \in reg_entries[m][nm] : e.tag # tg
    /\ \A m \in Nodes : tg \notin tombstones[m]
    /\ \A e \in reg_entries[n][name] : ~(e.ts = ts /\ e.node_id = n)
    /\ reg_entries' = [reg_entries EXCEPT
            ![n][name] = reg_entries[n][name] \union {EntryOf(tg, ts, n)}]
    /\ delta_pool'  = delta_pool \union {AddDelta(name, tg, ts, n)}
    /\ UNCHANGED tombstones

\* [ACTION] Node n unregisters name: tombstones all current tags,
\* generates Remove delta for each.
\* Mirrors Registry::unregister.
Unregister(n, name) ==
    /\ reg_entries[n][name] # {}
    /\ LET removed_tags == {e.tag : e \in reg_entries[n][name]}
       IN
           /\ tombstones'  = [tombstones EXCEPT
                   ![n] = tombstones[n] \union removed_tags]
           /\ delta_pool'  = delta_pool \union
                   {RemoveDelta(name, tg) : tg \in removed_tags}
           /\ reg_entries' = [reg_entries EXCEPT ![n][name] = {}]

\* [ACTION] Node n applies an Add delta.
\* Tombstone check FIRST, then idempotency check.
\* Mirrors Registry::merge_delta Add branch.
ApplyAdd(n, d) ==
    /\ d \in delta_pool
    /\ d.type = "Add"
    /\ d.tag \notin tombstones[n]                          \* tombstone guard
    /\ \A e \in reg_entries[n][d.name] : e.tag # d.tag    \* idempotency
    /\ reg_entries' = [reg_entries EXCEPT
            ![n][d.name] = reg_entries[n][d.name] \union
                           {EntryOf(d.tag, d.ts, d.node_id)}]
    /\ UNCHANGED <<tombstones, delta_pool>>

\* [ACTION] Node n applies a Remove delta.
\* Tombstones the tag and removes any matching entry.
\* Mirrors Registry::merge_delta Remove branch.
ApplyRemove(n, d) ==
    /\ d \in delta_pool
    /\ d.type = "Remove"
    /\ tombstones'  = [tombstones EXCEPT ![n] = tombstones[n] \union {d.tag}]
    /\ reg_entries' = [reg_entries EXCEPT
            ![n][d.name] = {e \in reg_entries[n][d.name] : e.tag # d.tag}]
    /\ UNCHANGED delta_pool

----
(****************************************************************************)
(* Specification                                                            *)
(****************************************************************************)

Next ==
    \/ \E n \in Nodes, name \in Names, tg \in Tags, ts \in 1..MaxTimestamp :
           Register(n, name, tg, ts)
    \/ \E n \in Nodes, name \in Names :
           Unregister(n, name)
    \/ \E n \in Nodes, d \in delta_pool :
           \/ (/\ d.type = "Add"    /\ ApplyAdd(n, d))
           \/ (/\ d.type = "Remove" /\ ApplyRemove(n, d))

Spec == Init /\ [][Next]_vars

----
(****************************************************************************)
(* Safety invariants                                                        *)
(****************************************************************************)

\* No live entry at any node has a tag that is tombstoned at that node.
\* Bug: if tombstone check is AFTER insert in merge_delta, a
\* Remove-then-Add sequence creates a live entry with a tombstoned tag.
NoTombstoneResurrection ==
    \A n \in Nodes : \A name \in Names :
        \A e \in reg_entries[n][name] :
            e.tag \notin tombstones[n]

\* LWW winner is deterministic: identical entry sets always yield same winner.
\* Bug: if tiebreaker uses >= instead of >, non-deterministic choice is possible
\* when two entries have equal ts and equal node_id (degenerate case).
LWWDeterminism ==
    \A n1, n2 \in Nodes : \A name \in Names :
        (reg_entries[n1][name] = reg_entries[n2][name] /\
         reg_entries[n1][name] # {}) =>
            Winner(reg_entries[n1][name]) = Winner(reg_entries[n2][name])

\* Once all deltas have been applied by every node, all nodes agree.
\* "Applied" means: for Add, the tag is live or tombstoned at n;
\*                  for Remove, the tag is tombstoned at n.
AllAddsApplied ==
    \A d \in delta_pool : d.type = "Add" =>
        \A n \in Nodes :
            d.tag \in tombstones[n] \/
            \E e \in reg_entries[n][d.name] : e.tag = d.tag

AllRemovesApplied ==
    \A d \in delta_pool : d.type = "Remove" =>
        \A n \in Nodes : d.tag \in tombstones[n]

ConvergenceAfterFullExchange ==
    (AllAddsApplied /\ AllRemovesApplied) =>
    \A n1, n2 \in Nodes : \A name \in Names :
        reg_entries[n1][name] = reg_entries[n2][name]

----
(****************************************************************************)
(* State constraint for finite model checking                               *)
(****************************************************************************)

StateConstraint ==
    Cardinality(delta_pool) <= Cardinality(Tags) * 2 * Cardinality(Names)

=============================================================================
