---- MODULE MCRebarSwim ----
\* Model-checking wrapper for RebarSwim.tla
\* Run: tlc MCRebarSwim -config RebarSwim.cfg -workers 4

EXTENDS RebarSwim, TLC

\* Symmetry reduction on nodes (safe for safety-only checking)
NodeSymmetry == Permutations(Nodes)

MCStateConstraint == StateConstraint

====
