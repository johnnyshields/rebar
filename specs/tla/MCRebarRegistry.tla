---- MODULE MCRebarRegistry ----
\* Model-checking wrapper for RebarRegistry.tla
\* Run: tlc MCRebarRegistry -config MCRebarRegistry.cfg -workers 4

EXTENDS RebarRegistry, TLC

\* Symmetry reduction: nodes and tags are interchangeable
NodeSymmetry == Permutations(Nodes)
\* TagSymmetry is intentionally NOT used in the cfg. Tag identity matters
\* for tombstone semantics: permuting tags would hide Add-after-Remove bugs
\* where a specific tag is tombstoned then re-added.
TagSymmetry  == Permutations(Tags)

MCStateConstraint == StateConstraint

====
