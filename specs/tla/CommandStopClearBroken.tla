---- MODULE CommandStopClearBroken ----
\* The same protocol with one line changed: a start over the folder an
\* earlier start left empties it without the fresh check the design
\* requires, so it can orphan a command that is still running, which no
\* later poll can then name, signal or stop.
\*
\* Both checkers must report this variant as a safety violation, with a
\* counterexample that ends where the orphaned leader still runs and its
\* record is gone. The good module keeps the changed action beside the one
\* it replaces, so this file is the whole of the difference.

EXTENDS CommandStopClear

BrokenNext ==
    \/ \E p \in Pids : Spawning(p)
    \/ \E p \in Pids : Ending(p)
    \/ \E p \in Pids : LeaderExiting(p)
    \/ \E p \in Pids : LeaderVanishing(p)
    \/ \E p \in Pids : Reaping(p)
    \/ Publishing
    \/ Abandoning
    \/ Crashing
    \/ Rebooting
    \/ Stopping
    \/ TimingOut
    \/ ClearingUnchecked

BrokenSpec == Init /\ [][BrokenNext]_vars

====
