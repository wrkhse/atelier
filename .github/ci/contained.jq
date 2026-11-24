# The exact settings run.sh gives a container, judged against `docker inspect`:
# $network and, as positional arguments, every TYPE:SOURCE:TARGET mount.
# test_reports.py runs it against inspection.json, one bad setting at a time.
.[0] as $c | $c.HostConfig as $h
| $c.Config.User == "10001:10001"
and $h.CapDrop == ["ALL"] and ($h.CapAdd // [] | length == 0)
and $h.SecurityOpt == ["no-new-privileges"]
and $h.Privileged == false
and ($h.PidMode == "" or $h.PidMode == "private")
and $h.IpcMode == "private" and $h.UsernsMode == "" and $h.UTSMode == ""
and $h.NetworkMode == $network
and ($h.Devices // [] | length == 0) and ($h.VolumesFrom // [] | length == 0)
and ($h.GroupAdd // [] | length == 0) and ($h.Tmpfs // {} | length == 0)
and $h.PidsLimit == 4096 and $h.Memory > 0 and $h.MemorySwap == $h.Memory
and $h.NanoCpus > 0
and ($h.Binds // [] | length == 0)
and ($h.Mounts | all(.ReadOnly == true))
# An image volume or any other anonymous mount shows only in .Mounts.
and ($c.Mounts | length) == ($h.Mounts | length)
and ([$h.Mounts[] | "\(.Type):\(.Source):\(.Target)"] | sort)
  == ($ARGS.positional | sort)
