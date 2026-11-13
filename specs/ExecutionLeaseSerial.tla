------------------------- MODULE ExecutionLeaseSerial -------------------------
(***************************************************************************)
(* ExecutionLease with the running-task limit at its default of one: the   *)
(* node as it was before it could run more than one task. `MaxCap` is 1,   *)
(* so `cap` never leaves 1 (see ExecutionLeaseSerial.cfg), and at most one *)
(* execution is active, whatever the scheduler, a cancel, a lost lease or  *)
(* a restart does. Every property of ExecutionLease holds here too, with   *)
(* more runs of the clock and more restarts than the concurrent model can  *)
(* afford.                                                                 *)
(***************************************************************************)
EXTENDS ExecutionLease

\* One execution thread at a time: the scheduler of a node with the
\* default limit.
OneExecution == Cardinality(Busy) <= 1

=============================================================================
