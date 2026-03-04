---- MODULE MCRebarRegistry ----
\* Model-checking wrapper for RebarRegistry.tla
\* Run: tlc MCRebarRegistry -config MCRebarRegistry.cfg -workers 4

EXTENDS RebarRegistry, TLC

\* Symmetry reduction: nodes and tags are interchangeable
NodeSymmetry == Permutations(Nodes)
TagSymmetry  == Permutations(Tags)

MCStateConstraint == StateConstraint

====
