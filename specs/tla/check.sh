#!/bin/bash
# Checks the spec and its broken variant with TLC and with everystate, and
# compares what the two say. It runs only in this experiment's container:
# it reads /specs, writes nothing outside /tmp, needs no network, and runs
# no atelier code. Neither checker is a dependency of atelier, and neither
# is a required gate. specs/tla/README.md describes the experiment; the
# expectations below are its acceptance criteria.
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
        -workers 1 -metadir "$scratch/meta-$1" -config "$1.cfg" "$1.tla" )
}

every_state() {
    ( cd "$specs" && "$everystate" -config "$1.cfg" "$1.tla" )
}

# check <label> <command> <spec>: one checker over one spec, keeping its
# exit status, its time, its output, and the states it counted.
check() {
    local label=$1 command=$2 spec=$3
    local key="$label-$spec" log="$scratch/$label-$spec.log"
    local started finished line

    started=$(date +%s%N)
    "$command" "$spec" > "$log" 2>&1
    exit_code[$key]=$?
    finished=$(date +%s%N)
    millis[$key]=$(( (finished - started) / 1000000 ))

    # TLC groups thousands with commas, and so does everystate, which
    # prints TLC's text; the counts are compared without them.
    line=$(grep 'states generated, ' "$log" | tail -1)
    if [[ $line =~ ([0-9][0-9,]*)' states generated, '([0-9][0-9,]*)' distinct states found' ]]; then
        generated[$key]=${BASH_REMATCH[1]//,/}
        distinct[$key]=${BASH_REMATCH[2]//,/}
    fi

    printf '=== %s on %s: exit %s, %sms ===\n' \
        "$label" "$spec" "${exit_code[$key]}" "${millis[$key]}"
    cat "$log"
    printf '\n'
}

verdict() {
    case $1 in
        0) printf 'passed' ;;
        12) printf 'invariant violated' ;;
        *) printf 'exit %s' "$1" ;;
    esac
}

for spec in CommandStopClear CommandStopClearBroken; do
    check tlc tlc "$spec"
    check everystate every_state "$spec"
done

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
expect tlc-CommandStopClear 0
expect everystate-CommandStopClear 0

# The broken variant is a safety violation on both. TLC reports one as exit
# status 12, and everystate uses TLC's statuses.
expect tlc-CommandStopClearBroken 12
expect everystate-CommandStopClearBroken 12

printf 'tla: | spec | checker | verdict | generated | distinct | ms |\n'
printf 'tla: | --- | --- | --- | --- | --- | --- |\n'

for spec in CommandStopClear CommandStopClearBroken; do
    for label in tlc everystate; do
        key="$label-$spec"

        if [[ -z ${generated[$key]:-} ]]; then
            printf 'tla: **%s** printed no state count, so nothing can be compared.\n' "$key"
            failed=1
        fi

        printf 'tla: | %s | %s | %s | %s | %s | %s |\n' "$spec" "$label" \
            "$(verdict "${exit_code[$key]:-none}")" \
            "${generated[$key]:-?}" "${distinct[$key]:-?}" "${millis[$key]:-?}"
    done
done

agreed=1

for spec in CommandStopClear CommandStopClearBroken; do
    [[ ${exit_code[tlc-$spec]:-} == "${exit_code[everystate-$spec]:-}" ]] || agreed=0
    [[ ${generated[tlc-$spec]:-} == "${generated[everystate-$spec]:-}" ]] || agreed=0
    [[ ${distinct[tlc-$spec]:-} == "${distinct[everystate-$spec]:-}" ]] || agreed=0
done

if [[ $agreed == 1 ]]; then
    printf 'tla: **Both checkers agree**: the same verdict, and the same states generated and distinct, on both specs.\n'
else
    printf 'tla: **The two checkers disagree** on the verdict or the states counted. The table says where, and the whole output is in the artifact. Agreement is the experiment measurement, so this does not fail the job.\n'
fi

# The counterexamples, so the two can be compared without the artifact.
for label in tlc everystate; do
    printf 'tla: **%s on CommandStopClearBroken**:\n\n' "$label"
    awk '/violated/ { seen = 1 }
         seen { print "tla:   " $0; if (++n >= 24) exit }' \
        "$scratch/$label-CommandStopClearBroken.log"
    printf '\n'
done

exit "$failed"
