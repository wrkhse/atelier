---- MODULE CommandStopClearCrash ----
\* The protocol as atelier ran it before the leader published its own
\* identity: the daemon wrote the record after the spawn, so a crash between
\* the two left a group nothing could name. The third broken variant, and the
\* order the good module no longer uses.
\*
\* Both checkers must report the violation, on the state alone. The good module
\* keeps every action this uses, so this file is the whole of the difference.

EXTENDS CommandStopClear

CrashNext ==
    \/ \E p \in Pids : SpawningUnpublished(p)
    \/ \E p \in Pids : Ending(p)
    \/ \E p \in Pids : LeaderExiting(p)
    \/ \E p \in Pids : LeaderVanishing(p)
    \/ \E p \in Pids : Reaping(p)
    \/ \E p \in Pids : Foreign(p)
    \/ Publishing
    \/ DaemonCrashing
    \/ Abandoning
    \/ Rebooting
    \/ Stopping
    \/ TimingOut
    \/ Clearing
    \/ SpawnFailing

CrashSpec == Init /\ [][CrashNext]_vars

====
