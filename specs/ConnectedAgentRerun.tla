------------------------- MODULE ConnectedAgentRerun -------------------------
(***************************************************************************)
(* ConnectedAgent with one task ID that gets two turns, one after the      *)
(* other, and two connections. It checks that a cancel, an answer or an    *)
(* end of the first turn never reaches the second turn of the same task    *)
(* (NoStaleCancel, EarlyCancelHonored, LateCancelDropped). ConnectedAgent  *)
(* itself checks two tasks that are open at once, one turn each. The       *)
(* constants are in ConnectedAgentRerun.cfg.                               *)
(***************************************************************************)
EXTENDS ConnectedAgent

=============================================================================
