---- MODULE CommandStopClearCrash ----
\* The same protocol with one path added: a daemon crash between the spawn and
\* the publication of the identity, which leaves the command running with a
\* folder nothing can name it by, and no later poll able to signal it.
\*
\* This is a residual of the design rather than of the model: the crash takes
\* the only handle on a group atelier started, and the folder atelier keeps
\* cannot say which group it was. Both checkers must report the violation, on
\* the state alone. The good module keeps the action beside the ones the
\* design uses, so this file is the whole of the difference.

EXTENDS CommandStopClear

CrashNext ==
    Next \/ DaemonCrashing

CrashSpec == Init /\ [][CrashNext]_vars

====
