#!/bin/bash
# Checks the spec, its broken variant and its liveness property with TLC and
# with everystate, and compares what the two say. It runs only in this
# experiment's container: it reads /specs, writes nothing outside /tmp,
# needs no network, and runs no atelier code. Neither checker is a
# dependency of atelier, and neither is a required gate.
# specs/tla/README.md describes the experiment; the expectations below are
# its acceptance criteria.
set -uo pipefail

specs=/specs
jar=/opt/tla/tla2tools.jar
everystate=/opt/everystate/bin/everystate
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

declare -A exit_code=()
declare -A millis=()
declare -A generated=()
declare -A distinct=()

tlc() {
    ( cd "$specs" && java -Xmx2g -cp "$jar" tlc2.TLC \
        -workers 1 -metadir "$scratch/meta-$2" -config "$2" "$1" )
}

every_state() {
    ( cd "$specs" && "$everystate" -config "$2" "$1" )
}

# check <key> <command> <spec> <cfg>: one checker over one spec and one
# configuration, keeping its exit status, its time, its output, and the
# states it counted.
check() {
    local key=$1 command=$2 spec=$3 cfg=$4
    local log="$scratch/$key.log" started finished line

    started=$(date +%s%N)
    "$command" "$spec" "$cfg" > "$log" 2>&1
    exit_code[$key]=$?
    finished=$(date +%s%N)
    millis[$key]=$(( (finished - started) / 1000000 ))

    # TLC groups thousands with commas, and so does everystate, which prints
    # TLC's text; the counts are compared without them.
    line=$(grep 'states generated, ' "$log" | tail -1)
    if [[ $line =~ ([0-9][0-9,]*)' states generated, '([0-9][0-9,]*)' distinct states found' ]]; then
        generated[$key]=${BASH_REMATCH[1]//,/}
        distinct[$key]=${BASH_REMATCH[2]//,/}
    fi

    printf '=== %s: exit %s, %sms ===\n' "$key" "${exit_code[$key]}" \
        "${millis[$key]}"
    cat "$log"
    printf '\n'
}

verdict() {
    case $1 in
        0) printf 'passed' ;;
        12) printf 'invariant violated' ;;
        13) printf 'liveness violated' ;;
        *) printf 'exit %s' "$1" ;;
    esac
}

# What version of each tool ran, so the log says what was checked.
printf '=== everystate --version ===\n'
"$everystate" --version 2>&1 || printf '(it does not take --version: exit %s)\n' "$?"
printf '\n'

check tlc-safety tlc CommandStopClear.tla CommandStopClear.cfg
check everystate-safety every_state CommandStopClear.tla CommandStopClear.cfg
check tlc-broken tlc CommandStopClearBroken.tla CommandStopClearBroken.cfg
check everystate-broken every_state CommandStopClearBroken.tla CommandStopClearBroken.cfg
check tlc-live tlc CommandStopClear.tla CommandStopClearLive.cfg
check everystate-live every_state CommandStopClear.tla CommandStopClearLive.cfg

failed=0

# expect <key> <the status the experiment expects>
expect() {
    local key=$1 want=$2 got=${exit_code[$1]:-none}

    if [[ $got != "$want" ]]; then
        printf 'tla: **%s** exited %s, and the experiment expects %s.\n' \
            "$key" "$got" "$want"
        failed=1
    fi
}

# The design's claim: the protocol as written holds its invariants.
expect tlc-safety 0
expect everystate-safety 0

# The broken variant is a safety violation on both. TLC reports one as exit
# status 12, and everystate uses TLC's statuses.
expect tlc-broken 12
expect everystate-broken 12

# TLC's answer on the one liveness claim the design makes.
expect tlc-live 0

# everystate on the property. A liveness violation is a disagreement and
# fails; a tool that will not check a property at all is a finding this
# experiment reports rather than a failure, because what it does with
# PROPERTY is one of the things being measured.
case ${exit_code[everystate-live]:-none} in
    0) printf 'tla: **everystate agrees** on the liveness property.\n' ;;
    13) printf 'tla: **everystate reports a liveness violation** where TLC finds none: they disagree.\n'
        failed=1 ;;
    *) printf 'tla: **finding**: everystate did not check the property (exit %s); TLC did, and its answer is what the design relies on. Its output is above and in the artifact.\n' \
        "${exit_code[everystate-live]:-none}" ;;
esac

printf 'tla: | what | checker | verdict | generated | distinct | ms |\n'
printf 'tla: | --- | --- | --- | --- | --- | --- |\n'

# row <key> <checker> <what> <1 when a missing state count fails the job>
row() {
    local key=$1

    if [[ -z ${generated[$key]:-} && "$4" == 1 ]]; then
        printf 'tla: **%s** printed no state count, so nothing can be compared. Its output is above.\n' "$key"
        failed=1
    fi

    printf 'tla: | %s | %s | %s | %s | %s | %s |\n' "$3" "$2" \
        "$(verdict "${exit_code[$key]:-none}")" "${generated[$key]:-?}" \
        "${distinct[$key]:-?}" "${millis[$key]:-?}"
}

row tlc-safety tlc 'invariants' 1
row everystate-safety everystate 'invariants' 1
row tlc-broken tlc 'the broken variant' 1
row everystate-broken everystate 'the broken variant' 1
row tlc-live tlc 'the liveness property' 1
row everystate-live everystate 'the liveness property' 0

agreed=1

# same <key> <key>: the same verdict, and the same states counted.
same() {
    [[ ${exit_code[$1]:-} == "${exit_code[$2]:-}" ]] || agreed=0
    [[ ${generated[$1]:-} == "${generated[$2]:-}" ]] || agreed=0
    [[ ${distinct[$1]:-} == "${distinct[$2]:-}" ]] || agreed=0
}

same tlc-safety everystate-safety
same tlc-broken everystate-broken

if [[ ${exit_code[everystate-live]:-none} == 0 ]]; then
    same tlc-live everystate-live
fi

if [[ $agreed == 1 ]]; then
    printf 'tla: **Both checkers agree**: the same verdict, and the same states generated and distinct, wherever both answered.\n'
else
    printf 'tla: **The two checkers disagree** on the verdict or the states counted. The table says where, and the whole output is in the artifact.\n'
fi

# The counterexamples, so the two can be compared without the artifact.
for key in tlc-broken everystate-broken; do
    printf 'tla: **%s**:\n\n' "$key"
    awk '/violated/ { seen = 1 }
         seen { print "tla:   " $0; if (++n >= 24) exit }' "$scratch/$key.log"
    printf '\n'
done

exit "$failed"
