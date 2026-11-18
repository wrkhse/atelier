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
\* What is here, and what is not. The daemon's are the folder, the child a
\* start holds unreaped, and the history of which processes atelier started
\* and signalled; the system's are the boot and the table of process
\* numbers. The commands inside a group are not modelled, so a signalled
\* group leaves nothing behind here, and neither does a descendant that
\* outlives its leader. Each action is one call in the code, so reading the
\* record and signalling the group it names are one step; the window that
\* leaves, between the check and /bin/kill, is named as the follow-up in
\* specs/tla/README.md.

EXTENDS Naturals

\* The numbers a command's group leader can have, the boots the model tells
\* apart, and the most incarnations a number gets in one boot. The
\* configuration sets all three, so a larger model needs no edit here.
CONSTANTS Pids, Boots, Stamp

\* 0 stands for no process, so `proc` and `start` stay total functions and
\* no conjunction depends on the order in which it is read.
Numbers == {0} \cup Pids

\* What a number is now: free; a group leader atelier started; a leader
\* that has exited and not been reaped; or a process atelier did not start,
\* which leads a group of its own or belongs to another.
Kinds == {"free", "leader", "zombie", "foreign"}

\* What the step's folder holds: nothing; a record whose identity was never
\* published; a `group` file from an earlier atelier; a record that cannot
\* name a leader; or a published identity.
Records == {"absent", "unpublished", "legacy", "invalid", "published"}

\* A process, as a record names one: its number, the boot it started in, and
\* its start identity in that boot. Two processes can hold the same number,
\* so only all three say which one is meant.
Identities == { <<p, b, s>> : p \in Pids, b \in Boots, s \in 0..Stamp }

VARIABLES
    \* The boot the computer is in now.
    boot,
    \* What each number is now.
    proc,
    \* The start identity of the process now on each number.
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
    \* The processes atelier has started a group for, and the processes it
    \* has signalled. Both hold identities, so a number that a later process
    \* took is a different process here.
    ours,
    signalled

vars ==
    <<boot, proc, start, folder, recPid, recBoot, recStart, exited, spawned,
      ours, signalled>>

\* The identity of the process on a number now.
Now(p) ==
    <<p, boot, start[p]>>

\* Whether the published record names a leader that still leads its group:
\* the same boot, alive and not a zombie, and the same start identity. A
\* number that another process took fails this, which is what keeps a stale
\* record from signalling it.
IsRunning ==
    /\ folder = "published"
    /\ boot = recBoot
    /\ proc[recPid] = "leader"
    /\ start[recPid] = recStart

\* Whether the folder still names the process on `p`: its published record,
\* or a folder that holds `started` and `worktree` and no usable record,
\* which atelier keeps rather than empties.
Named(p) ==
    \/ /\ folder = "published"
       /\ recPid = p
       /\ recBoot = boot
       /\ recStart = start[p]
    \/ folder = "unpublished"

Init ==
    \E f \in {"absent", "legacy", "invalid"} :
        /\ boot = 0
        /\ proc = [q \in Numbers |-> "free"]
        /\ start = [q \in Numbers |-> 0]
        /\ folder = f
        /\ recPid \in Pids
        /\ recBoot = 0
        /\ recStart = 0
        /\ exited = FALSE
        /\ spawned = 0
        /\ ours = {}
        /\ signalled = {}

\* A start finds no folder, writes `started` and `worktree`, and spawns the
\* command's shell in a process group of its own. The identity is not
\* published yet, and the child is unreaped, so its number cannot be reused
\* while this start still holds it.
Spawning(p) ==
    /\ folder = "absent"
    /\ spawned = 0
    /\ p \in Pids
    /\ proc[p] = "free"
    /\ start[p] < Stamp
    /\ proc' = [proc EXCEPT ![p] = "leader"]
    /\ start' = [start EXCEPT ![p] = start[p] + 1]
    /\ folder' = "unpublished"
    /\ spawned' = p
    /\ ours' = ours \cup {<<p, boot, start[p] + 1>>}
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, recPid, recBoot, recStart, signalled>>

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
    /\ UNCHANGED <<boot, proc, start, exited, ours, signalled>>

\* The identity cannot be recorded, so no later poll could find the group:
\* the start signals the group of the child it still holds unreaped, kills
\* and reaps that child, and removes the folder once the signal was
\* delivered. When the guard refuses the group or /bin/kill fails, the
\* child is still killed and reaped, but the folder stays: the group it
\* names may still run, and the folder is then the only trace of it.
Abandoning ==
    /\ folder = "unpublished"
    /\ spawned /= 0
    /\ \/ /\ signalled' = signalled \cup {Now(spawned)}
          /\ folder' = "absent"
       \/ /\ folder' = "unpublished"
          /\ UNCHANGED signalled
    /\ proc' = [proc EXCEPT ![spawned] = "free"]
    /\ spawned' = 0
    /\ UNCHANGED <<boot, start, recPid, recBoot, recStart, exited, ours>>

\* The daemon dies between the spawn and the publication. Its child handle
\* is gone while the command runs on, and the folder holds `started` and
\* `worktree` with no identity: no later poll can name the group, so
\* atelier sends it no signal and keeps the folder.
Crashing ==
    /\ spawned /= 0
    /\ spawned' = 0
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   exited, ours, signalled>>

\* A start that cannot spawn removes the folder it just made, since nothing
\* ran. When that removal fails too, the folder stays, holding `started`
\* and `worktree` with no identity, and atelier then keeps it for good.
SpawnFailing ==
    /\ folder = "absent"
    /\ spawned = 0
    /\ \/ folder' = "unpublished"
       \/ UNCHANGED folder
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled>>

\* The command's top level ends, and the leader's shell writes `exit`.
Ending(p) ==
    /\ exited = FALSE
    /\ proc[p] = "leader"
    /\ \/ (folder = "published" /\ recPid = p)
       \/ (folder = "unpublished" /\ spawned = p)
    /\ exited' = TRUE
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   spawned, ours, signalled>>

\* The leader exits, and only its entry is left until the reaper waits on
\* it; the identity check reads a zombie as gone.
LeaderExiting(p) ==
    /\ proc[p] = "leader"
    /\ proc' = [proc EXCEPT ![p] = "zombie"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled>>

\* Something else ends the leader, so it writes no exit status: the poll
\* that finds it settles the step failed, without sending a signal.
LeaderVanishing(p) ==
    /\ proc[p] = "leader"
    /\ spawned /= p
    /\ proc' = [proc EXCEPT ![p] = "free"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled>>

\* The reaper reaps the leader, so its number can be used again. A start's
\* own child is reaped by that start, not here.
Reaping(p) ==
    /\ proc[p] = "zombie"
    /\ spawned /= p
    /\ proc' = [proc EXCEPT ![p] = "free"]
    /\ UNCHANGED <<boot, start, folder, recPid, recBoot, recStart, exited,
                   spawned, ours, signalled>>

\* A free number is taken by a process atelier did not start: a group
\* leader of its own, or a process in another group. Either way the number
\* is no longer the group a stale record would name, and this is the reuse
\* the identity check exists for.
Foreign(p) ==
    /\ proc[p] = "free"
    /\ start[p] < Stamp
    /\ proc' = [proc EXCEPT ![p] = "foreign"]
    /\ start' = [start EXCEPT ![p] = start[p] + 1]
    /\ UNCHANGED <<boot, folder, recPid, recBoot, recStart, exited, spawned,
                   ours, signalled>>

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
                   signalled>>

\* A run's cleanup, or a start's clear, cancels the command the folder
\* still names. A command that wrote its exit needs no signal. The record
\* must confirm the leader, and then its group is signalled, so every
\* process it started dies. A record from another boot, a leader that has
\* gone or become a zombie, or a number another process took sends nothing;
\* so does a signal the guard refuses or /bin/kill fails, which leaves the
\* group running and the state as it was.
Stopping ==
    /\ exited = FALSE
    /\ folder = "published"
    /\ \/ /\ IsRunning
          /\ signalled' = signalled \cup {Now(recPid)}
          /\ proc' = [proc EXCEPT ![recPid] = "zombie"]
          /\ start' = start
       \/ /\ UNCHANGED <<proc, start, signalled>>
    /\ UNCHANGED <<boot, folder, recPid, recBoot, recStart, exited, spawned,
                   ours>>

\* A poll that finds the command past its limit kills its group through
\* the same record, and the step fails with what the command printed. The
\* step fails either way; the group may still run when the signal is
\* refused.
TimingOut ==
    /\ exited = FALSE
    /\ IsRunning
    /\ \/ /\ signalled' = signalled \cup {Now(recPid)}
          /\ proc' = [proc EXCEPT ![recPid] = "zombie"]
          /\ start' = start
       \/ /\ UNCHANGED <<proc, start, signalled>>
    /\ UNCHANGED <<boot, folder, recPid, recBoot, recStart, exited, spawned,
                   ours>>

\* A start over the folder an earlier start left: the stop above, then a
\* fresh check of the record, then the folder emptied only once that check
\* finds the leader gone or the record from another boot. A folder whose
\* leader still runs is kept, which is this guard and the transient "still
\* stopping" error that the run retries on its next poll. A record that
\* cannot name a leader is kept too, and the start fails with the
\* unsignalled text, so the folder stays the only trace of a command that
\* may still run.
Clearing ==
    /\ folder = "published"
    /\ ~IsRunning
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled>>

\* The same start with the fresh check dropped: the folder is emptied the
\* moment the stop returns, whether or not the command it names is still
\* running. This is the broken variant's one change.
ClearingUnchecked ==
    /\ folder = "published"
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled>>

\* The reaper's every run, as one action, for the fairness below.
ReapAll ==
    \E p \in Pids : Reaping(p)

Next ==
    \/ \E p \in Pids : Spawning(p)
    \/ \E p \in Pids : Ending(p)
    \/ \E p \in Pids : LeaderExiting(p)
    \/ \E p \in Pids : LeaderVanishing(p)
    \/ \E p \in Pids : Reaping(p)
    \/ \E p \in Pids : Foreign(p)
    \/ Publishing
    \/ Abandoning
    \/ Crashing
    \/ Rebooting
    \/ Stopping
    \/ TimingOut
    \/ Clearing
    \/ SpawnFailing

Spec == Init /\ [][Next]_vars

\* The reaper thread waits on every leader, so a leader that exited is
\* eventually reaped: that is the one liveness claim the code itself makes,
\* and the assumption below is only that the thread runs while it can.
FairSpec ==
    Spec /\ WF_vars(ReapAll)

ReapedEventually ==
    \A p \in Pids :
        []( (proc[p] = "zombie" /\ spawned /= p) => <>(proc[p] = "free") )

\* The variables stay in their domains, so a change that breaks one is a
\* violation and not a wrong answer.
TypeOK ==
    /\ boot \in Boots
    /\ proc \in [Numbers -> Kinds]
    /\ start \in [Numbers -> 0..Stamp]
    /\ folder \in Records
    /\ recPid \in Pids
    /\ recBoot \in Boots
    /\ recStart \in 0..Stamp
    /\ exited \in BOOLEAN
    /\ spawned \in Numbers
    /\ ours \subseteq Identities
    /\ signalled \subseteq Identities

\* A process atelier started, and that is still leading its group, is still
\* named by the folder: its published record, or the folder that atelier
\* keeps because it cannot say which group to signal. A folder emptied
\* while its command runs leaves a process nothing can name.
NoOrphanedLeader ==
    \A p \in Pids :
        (proc[p] = "leader" /\ Now(p) \in ours) => Named(p)

\* Every signal names a process atelier started a group for. A number that
\* another process took is a different identity, so signalling a reused
\* number fails this.
NoUnconfirmedSignal ==
    signalled \subseteq ours

====
