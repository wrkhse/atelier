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
\* outlives its leader.
\*
\* The good specification also takes the record as written: a filesystem that
\* cannot write it makes a start kill the command it spawned and fail, which is
\* not modelled here.
\*
\* One window is left out, and specs/tla/README.md names it as a follow-up:
\* the one between the check and /bin/kill, because reading the record and
\* signalling the group it names are one action here, as they are one call.
\*
\* The crash window is not left out. The leader publishes its own identity as
\* its first act, before it runs anything, so the folder names the group from
\* the moment the process can do anything, and DaemonCrashing is part of the
\* good specification's Next. CommandStopClearCrash.tla keeps the order
\* atelier ran before that, where only the daemon wrote the record, and there
\* the same crash still fails NoOrphanedLeader.

EXTENDS Naturals

\* The numbers a command's group leader can have, the boots the model tells
\* apart, and the most incarnations a number gets in one boot. The
\* configuration sets all three, so a larger model needs no edit here.
CONSTANTS Pids, Boots, Stamp

\* 0 stands for no process, so `proc` and `start` stay total functions and
\* no conjunction depends on the order in which it is read.
Numbers == {0} \cup Pids

\* What a number is now: free; a group leader atelier started; a leader that
\* has exited and not been reaped; or a process atelier did not start, which
\* leads a group of its own or belongs to another.
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
    \* The number the folder's record names: the published identity's
    \* leader, the `group` file's number, or the number a record that cannot
    \* be read still holds. Meaningful for every folder but an empty one.
    recPid,
    \* The boot and start identity the published record names. Read only
    \* while `folder` is "published", and kept in every state so every
    \* function is total.
    recBoot,
    recStart,
    \* Whether the command's top level has ended and written its status.
    exited,
    \* The child a start spawned and still holds unreaped, 0 when it holds
    \* none.
    spawned,
    \* The processes atelier has started a group for, and the processes it has
    \* signalled. Both hold identities, so a number that a later process took
    \* is a different process here.
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

\* Whether the folder still names the process on `p`.
\*
\* Its published record can confirm it. The start that spawned it can still
\* hold it unreaped, which is the identity not yet published. A `group` file
\* or a record that cannot be read names the number an earlier atelier left
\* a command on, and atelier keeps that folder rather than empties it: the
\* command may still be running, and no later poll could signal it.
Named(p) ==
    \/ /\ folder = "published"
       /\ recPid = p
       /\ recBoot = boot
       /\ recStart = start[p]
    \/ /\ folder = "unpublished"
       /\ spawned = p
    \/ /\ folder \in {"legacy", "invalid"}
       /\ recPid = p

\* A folder that holds nothing, or one an earlier atelier left: a `group`
\* file, or a record that cannot be read. The two records atelier inherits
\* come with the command an earlier atelier started on `recPid`, which may
\* still be running.
Init ==
    \E f \in {"absent", "legacy", "invalid"} :
    \E p \in Pids :
        /\ boot = 0
        /\ proc = [q \in Numbers |->
                     IF f = "absent" \/ q /= p THEN "free" ELSE "leader"]
        /\ start = [q \in Numbers |->
                      IF f = "absent" \/ q /= p THEN 0 ELSE 1]
        /\ folder = f
        /\ recPid = p
        /\ recBoot = 0
        /\ recStart = 0
        /\ exited = FALSE
        /\ spawned = 0
        /\ ours = IF f = "absent" THEN {} ELSE {<<p, 0, 1>>}
        /\ signalled = {}

\* A start finds no folder, writes `starting`, `started` and `worktree`, and
\* spawns the command's shell in a process group of its own. The leader's
\* first act publishes its own identity, before it runs anything, so the
\* folder names the group from the moment the process can do anything; the
\* daemon still holds the child unreaped until it confirms the record.
Spawning(p) ==
    /\ folder = "absent"
    /\ spawned = 0
    /\ p \in Pids
    /\ proc[p] = "free"
    /\ start[p] < Stamp
    /\ proc' = [proc EXCEPT ![p] = "leader"]
    /\ start' = [start EXCEPT ![p] = start[p] + 1]
    /\ folder' = "published"
    /\ recPid' = p
    /\ recBoot' = boot
    /\ recStart' = start[p] + 1
    /\ spawned' = p
    /\ ours' = ours \cup {<<p, boot, start[p] + 1>>}
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, signalled>>

\* The daemon writes its own reading of the record and lets the reaper wait
\* on the leader. The record is the one the leader published, so this changes
\* what the daemon holds, not what the folder says.
Confirming ==
    /\ spawned /= 0
    /\ spawned' = 0
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   exited, ours, signalled>>

\* The start as atelier ran it before the leader published its own identity:
\* the folder names the group only once the daemon writes the record, so a
\* daemon that dies first leaves a group nothing can name. The third broken
\* variant uses this and Publishing.
SpawningUnpublished(p) ==
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

\* The daemon writes the record of the child it still holds: the order
\* CommandStopClearCrash.tla keeps, where the folder names the group only
\* once this has run. In the good specification the leader has published the
\* same record already, so Confirming is this without the folder changing.
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
\* names may still run, and the folder is then the only trace of it. The good
\* specification takes the record as written, so only the third variant reaches
\* this; a start whose writes fail kills the command it spawned and fails.
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

\* The daemon dies, and the child handle it held is gone. The leader published
\* its own identity before it ran anything, so the folder still names the
\* group and a later poll can check it and signal it: nothing is lost. In the
\* order CommandStopClearCrash.tla keeps, where only the daemon wrote the
\* record, the same crash orphans the group.
DaemonCrashing ==
    /\ spawned /= 0
    /\ spawned' = 0
    /\ UNCHANGED <<boot, proc, start, folder, recPid, recBoot, recStart,
                   exited, ours, signalled>>

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
\* running. This is the first broken variant's one change.
ClearingUnchecked ==
    /\ folder = "published"
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled>>

\* A start that also empties a folder holding a record it cannot act on: a
\* `group` file, or one it cannot read, over the command an earlier atelier
\* left running on its number. This is the second broken variant's one
\* change.
ClearingUnrecorded ==
    /\ folder \in {"legacy", "invalid"}
    /\ folder' = "absent"
    /\ exited' = FALSE
    /\ UNCHANGED <<boot, proc, start, recPid, recBoot, recStart, spawned,
                   ours, signalled>>

Next ==
    \/ \E p \in Pids : Spawning(p)
    \/ \E p \in Pids : Ending(p)
    \/ \E p \in Pids : LeaderExiting(p)
    \/ \E p \in Pids : LeaderVanishing(p)
    \/ \E p \in Pids : Reaping(p)
    \/ \E p \in Pids : Foreign(p)
    \/ Confirming
    \/ DaemonCrashing
    \/ Rebooting
    \/ Stopping
    \/ TimingOut
    \/ Clearing
    \/ SpawnFailing

Spec == Init /\ [][Next]_vars

\* The reaper thread waits on every leader, so a leader that exited is
\* eventually reaped, and each of them is fair: that is the one liveness
\* claim the code itself makes.
FairSpec ==
    Spec /\ \A p \in Pids : WF_vars(Reaping(p))

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
\* named by the folder: its published record, the start that holds it, or the
\* record an earlier atelier left that atelier keeps because it cannot say
\* which group to signal. A folder emptied or repurposed while its command
\* runs leaves a process nothing can name.
NoOrphanedLeader ==
    \A p \in Pids :
        (proc[p] = "leader" /\ Now(p) \in ours) => Named(p)

\* Every signal names a process atelier started a group for. A number that
\* another process took is a different identity, so signalling a reused
\* number fails this.
NoUnconfirmedSignal ==
    signalled \subseteq ours

====
