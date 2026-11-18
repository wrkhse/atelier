---- MODULE CommandStopClearLoose ----
\* The same protocol with one path added: a start that treats a folder
\* holding a record it cannot act on as an empty folder and empties it, so a
\* command an earlier atelier left running keeps running with nothing left to
\* name it, and no later poll can signal it.
\*
\* Both checkers must report this variant as a safety violation too. The good
\* module keeps the added action beside the ones the design uses, so this file
\* is the whole of the difference.

EXTENDS CommandStopClear

LooseNext ==
    Next \/ ClearingUnrecorded

LooseSpec == Init /\ [][LooseNext]_vars

====
