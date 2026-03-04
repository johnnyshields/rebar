---- MODULE MCRebarSupervisor ----
\* Model-checking wrapper for RebarSupervisor.tla
\* Run: tlc MCRebarSupervisor -config RebarSupervisor.cfg -workers 4

EXTENDS RebarSupervisor, TLC

\* No symmetry on Children (order matters for RestForOne successor indexing)
\* Symmetry on Children would be unsound here.

MCStateConstraint == StateConstraint

====
