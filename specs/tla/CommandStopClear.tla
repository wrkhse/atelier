---- MODULE CommandStopClear ----
\* One atelier background command's step folder, and the race a start
\* repeated over an existing folder runs into.
\*
\* The code this reads is crates/cli/src/command_step/run.rs on
\* feat/run-requests (PR #57), with the identity check in identity.rs and
\* the signal guard in crates/command/src/lib.rs. This spec is the design's
\* own argument; it is not a proof of the code.
\*
\* A step folder holds `started`, the time it started; `worktree`, where it
\* runs; `identity`, the record of the leader of the command's process
\* group, published by writing identity.tmp, syncing it, renaming it to
\* `identity`, and syncing the folder; `output`, what the command printed;
\* and `exit`, the status the leader's shell writes once the command's top
\* level ends.
\*
\* A start on a folder that already exists first stops the command the
\* folder still names, then checks the record again, and empties the folder
\* only once that fresh check finds the leader gone or the record from
\* another boot. That second look is what keeps a start replayed after a
\* daemon crash from orphaning a group that is still running, and it is the
\* race this module models.
\*
\* Each action is one call in the code, so reading the record and
\* signalling the group it names are one step. The code names the window
\* that leaves: checking and then running /bin/kill leaves a moment in
\* which the leader can exit and its number be reused. That window is not
\* modelled, so this spec does not claim it is closed.

EXTENDS Naturals

\* The boots this spec tells apart: the one before a restart of the
\* computer, and the one after. A third boot adds no interleaving.
Boots == {0, 1}

\* The numbers a command's group leader can have: a process's number is its
\* pid and its group's. Two are enough to reach a number a later process
\* reuses.
Pids == {2, 3}

\* 0 stands for no process, so `proc` and `start` stay total functions and
\* no conjunction depends on the order in which it is read.
Numbers == {0} \cup Pids

\* What a number is now.
Kinds == {"free", "leader", "zombie"}

\* What the step's folder holds: nothing; a record whose identity was never
\* published; a `group` file from an earlier atelier; a record that cannot
\* name a leader; or a published identity.
Records == {"absent", "unpublished", "legacy", "invalid", "published"}

VARIABLES
    \* The boot the computer is in now.
    boot,
    \* What each number is now.
    proc,
    \* Each number's start identity, 0 before it ever had a process.
    start,
    \* What the step's folder holds.
    folder,
    \* What the published record names. Read only while `folder` is
    \* "published", and kept in every state so every function is total.
    recPid,
    recBoot,
    recStart,
    \* Whether the command's top level has ended and written its status.
    exited,
    \* The child a start spawned and still holds unreaped, 0 when it holds
    \* none.
    spawned,
    \* The numbers atelier has started a group on, and the numbers it has
    \* signalled.
    ours,
    signalled,
    \* History: whether a folder was ever emptied while the leader it named
    \* was still running, which no later poll could then name or stop.
    orphaned

vars ==
    <<boot, proc, start, folder, recPid, recBoot, recStart, exited, spawned,
      ours, signalled, orphaned>>

\* Whether the published record names a leader that still leads its group:
\* the same boot, alive and not a zombie, and the same start identity.
IsRunning ==
    /\ folder = "published"
    /\ boot = recBoot
    /\ proc[recPid] = "leader"
    /\ start[recPid] = recStart

Init ==
    /\ boot = 0
    /\ proc = [q \in Numbers |-> "free"]
    /\ start = [q \in Numbers |-> 0]
    /\ folder = "absent"
    /\ recPid = 2
    /\ recBoot = 0
    /\ recStart = 1
    /\ exited = FALSE
    /\ spawned = 0
    /\ ours = {}
    /\ signalled = {}
    /\ orphaned = FALSE

\* A start finds no folder, writes `started` and `worktree`, and spawns the
\* command's shell in a process group of its own. The identity is not
\* published yet, and the child is unreaped, so its number cannot be reused
\* while this start still holds it.
Spawning(p) ==
    /\ folder = "absent"
    /\ spawned = 0
    /\ p \in Pids
    /\ proc[p] = "free"
    /\ start[p] < 2
    /\ proc' = [proc EXCEPT ![p] = "leader"]
    /\ start' = [start EXCEPT ![p] = start[p] + 1]
    /\ folder' = "unpublished"
    /\ spawned' = p
    /\ ours' = ours \cup {p}
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, recPid, recBoot, recStart, signalled, orphaned>>

\* The start records the child's identity before it returns: the child
\* leads its own group, and the record holds the number, the boot and the
\* start identity the system reports for it.
Publishing ==
    /\ folder = "unpublished"
    /\ spawned /= 0
    /\ folder' = "published"
    /\ recPid' = spawned
    /\ recBoot' = boot
    /\ recStart' = start[spawned]
    /\ spawned' = 0
    /\ UNCHANGED <<boot, proc, start, exited, ours, signalled, orphaned>>

\* The identity cannot be recorded, so no later poll could find the group:
\* the start signals the group of the child it still holds unreaped, kills
\* and reaps it, and removes the folder once that signal was delivered. A
\* refused signal would keep the folder, which is then the only trace of a
\* command that may still run; the guard refuses no number here, since the
\* child is a process this start just spawned.
Abandoning ==
    /\ folder = "unpublished"
    /\ spawned /= 0
    /\ signalled' = signalled \cup {spawned}
    /\ proc' = [proc EXCEPT ![spawned] = "free"]
    /\ folder' = "absent"
    /\ spawned' = 0
    /\ UNCHANGED <<boot, start, recPid, recBoot, recStart, exited, ours,
                   orphaned>>

\* The daemon dies between the spawn and the publication. Its child handle
\* is gone while the command runs on, and the folder holds `started` and
\* `worktree` with no identity: no later poll can name the group, so
\* atelier sends it no signal and keeps the folder.
Crashing ==
    /\ spawned /= 0
    /\ spawned' = 0
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   exited, ours, signalled, orphaned>>

\* The command's top level ends, and the leader's shell writes `exit`.
Ending(p) ==
    /\ exited = FALSE
    /\ proc[p] = "leader"
    /\ \/ (folder = "published" /\ recPid = p)
       \/ (folder = "unpublished" /\ spawned = p)
    /\ exited' = TRUE
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   spawned, ours, signalled, orphaned>>

\* The leader exits, and only its entry is left until the reaper waits on
\* it; the identity check reads a zombie as gone.
LeaderExiting(p) ==
    /\ proc[p] = "leader"
    /\ proc' = [proc EXCEPT ![p] = "zombie"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled, orphaned>>

\* Something else ends the leader, so it writes no exit status: the poll
\* that finds it settles the step failed, without sending a signal.
LeaderVanishing(p) ==
    /\ proc[p] = "leader"
    /\ spawned /= p
    /\ proc' = [proc EXCEPT ![p] = "free"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled, orphaned>>

\* The reaper reaps the leader, so its number can be used again. A start's
\* own child is reaped by that start, not here.
Reaping(p) ==
    /\ proc[p] = "zombie"
    /\ spawned /= p
    /\ proc' = [proc EXCEPT ![p] = "free"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled, orphaned>>

\* The computer restarts: no process is left, the daemon is gone, and the
\* folder keeps what it held, including a record from the boot that has
\* gone.
Rebooting ==
    /\ boot < 1
    /\ boot' = boot + 1
    /\ proc' = [q \in Numbers |-> "free"]
    /\ start' = [q \in Numbers |-> 0]
    /\ spawned' = 0
    /\ UNCHANGED <<folder, recPid, recBoot, recStart, exited, ours,
                   signalled, orphaned>>

\* A run's cleanup, or a start's clear, cancels the command the folder
\* still names. A command that wrote its exit needs no signal. The record
\* must confirm the leader, and then its group is signalled, so every
\* process it started dies. A record from another boot, a leader that has
\* gone or become a zombie, or a number that no longer leads that group,
\* sends nothing.
Stopping ==
    /\ exited = FALSE
    /\ folder = "published"
    /\ \/ /\ IsRunning
          /\ signalled' = signalled \cup {recPid}
          /\ proc' = [proc EXCEPT ![recPid] = "zombie"]
          /\ start' = start
       \/ /\ ~IsRunning
          /\ UNCHANGED <<proc, start, signalled>>
    /\ UNCHANGED <<boot, folder, recPid, recBoot, recStart, exited, spawned,
                   ours, orphaned>>

\* A poll that finds the command past its limit kills its group through
\* the same record, and the step fails with what the command printed.
TimingOut ==
    /\ exited = FALSE
    /\ IsRunning
    /\ signalled' = signalled \cup {recPid}
    /\ proc' = [proc EXCEPT ![recPid] = "zombie"]
    /\ start' = start
    /\ UNCHANGED <<boot, folder, recPid, recBoot, recStart, exited, spawned,
                   ours, orphaned>>

\* A start over the folder an earlier start left: the stop above, then a
\* fresh check of the record, then the folder emptied only once that check
\* finds the leader gone or the record from another boot. A record that
\* cannot name a leader is kept, and the start fails transiently with the
\* unsignalled text, so the folder stays the only trace of a command that
\* may still run.
Clearing ==
    /\ folder = "published"
    /\ ~IsRunning
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled, orphaned>>

\* The same start with the fresh check dropped: the folder is emptied the
\* moment the stop returns, whether or not the command it names is still
\* running. This is the broken variant's one change.
ClearingUnchecked ==
    /\ folder = "published"
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ orphaned' = (orphaned \/ IsRunning)
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled>>

Next ==
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
    \/ Clearing

Spec == Init /\ [][Next]_vars

\* The variables stay in their domains, so a change that breaks one is a
\* violation and not a wrong answer.
TypeOK ==
    /\ boot \in Boots
    /\ proc \in [Numbers -> Kinds]
    /\ start \in [Numbers -> 0..2]
    /\ folder \in Records
    /\ recPid \in Pids
    /\ recBoot \in Boots
    /\ recStart \in 1..2
    /\ exited \in BOOLEAN
    /\ spawned \in Numbers
    /\ ours \subseteq Numbers
    /\ signalled \subseteq Numbers
    /\ orphaned \in BOOLEAN

\* No folder is ever emptied while the leader it named was still running:
\* nothing would be left to say which group to signal, so no later poll
\* could stop it.
NoOrphanedLeader ==
    orphaned = FALSE

\* No signal names a number atelier did not start a group on.
NoUnconfirmedSignal ==
    signalled \subseteq ours

====
