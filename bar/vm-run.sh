#!/usr/bin/env bash
# The one VM runner (docs/testing.md, *The VM target*). Use this; do not write your own.
#
#   bar/vm-run.sh [script]        # default: this checkout's bar/bar-test.sh
#
# The steps are target-agnostic (docs/testing.md, *The steps and their
# targets*); this runner is the VM target's and runs them with
# `CAUSTICS_TARGET=vm`. The nested gate, on the host's own GPU, is
# bar/nested-run.sh.
#
# The runner is checked in so that nobody writes their own: its boot probe is
# the harness's own (`wait_ssh`/`tssh` from the harness's lib.sh), key-only in
# BatchMode, so a refused key is a failed probe and never a password prompt on
# the host.
#
# 1. Take a guest slot. The locks are a semaphore of `$CAUSTICS_VM_SLOTS` files
#    (default 2): slot 0 is `~/.cache/caustics-vm.lock` (the unnumbered name, so
#    a runner that knows one lock still excludes slot 0) and slot N is
#    `~/.cache/caustics-vm.N.lock`. Each is tried once without
#    waiting; when all are busy the run waits for slot 0. `flock -o`
#    throughout: the lock fd is closed before the command runs, so a qemu this
#    starts does not inherit it.
# 2. Boot that slot's guest (`TOPAZ_TEST_ID=N`: ssh port 2233+N, sockets under
#    the harness's .artifacts/N/) unless it is already up.
# 3. Run the script with `TOPAZ_TEST_ID` set, its whole output to a log whose
#    path is printed first (`$CAUSTICS_VM_LOG` overrides it).
# 4. Stop the guest if this run started it — never one it found running — and
#    exit with the script's status.
#
# RAM: each guest is `$TOPAZ_TEST_RAM` (default 3G, see below). Read
# docs/testing.md, *The VM target*, before raising `$CAUSTICS_VM_SLOTS`.
#
# The harness: `$TOPAZ_TESTS` names a directory holding the external VM
# harness's `lib.sh` and `run.sh` (docs/testing.md, *The VM target*).

set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOPAZ_TESTS="${TOPAZ_TESTS:?set TOPAZ_TESTS to the tests directory of the VM harness (docs/testing.md, *The VM target*)}"
SLOTS="${CAUSTICS_VM_SLOTS:-2}"
SELF="$REPO/bar/vm-run.sh"
CONFLICT=75

slot_lock() {
    if [[ "$1" == 0 ]]; then
        printf '%s' "$HOME/.cache/caustics-vm.lock"
    else
        printf '%s' "$HOME/.cache/caustics-vm.$1.lock"
    fi
}

if [[ -z "${CAUSTICS_VM_SLOT_HELD:-}" ]]; then
    for ((id = 0; id < SLOTS; id++)); do
        CAUSTICS_VM_SLOT_HELD=$id flock -n -o -E "$CONFLICT" "$(slot_lock "$id")" bash "$SELF" "$@"
        rc=$?
        [[ "$rc" == "$CONFLICT" ]] || exit "$rc"
    done
    echo "vm-run: all $SLOTS slot(s) busy at $(date +%T); waiting for slot 0"
    CAUSTICS_VM_SLOT_HELD=0 exec flock -o -w 7200 "$(slot_lock 0)" bash "$SELF" "$@"
fi

# --- holding a slot ---------------------------------------------------------

export TOPAZ_TEST_ID="$CAUSTICS_VM_SLOT_HELD"
# The guest uses about 1.4 GB; anything more becomes page cache it never
# returns, and qemu keeps every page the guest touches resident on the host,
# so a guest's size is what it costs. 3 GB is the floor: at 2 GB the oracle on
# lavapipe ran past its cap, a slangc reload took over 40 s, and a 5 s toast
# expired before its screendump. bar-test.sh prints the guest's memory at the
# end of the run, so a tight guest is seen rather than guessed at.
# `TOPAZ_TEST_RAM` overrides.
export TOPAZ_TEST_RAM="${TOPAZ_TEST_RAM:-3G}"
SCRIPT="${1:-$REPO/bar/bar-test.sh}"
LOG="${CAUSTICS_VM_LOG:-$REPO/bar/test-runs/vm-run-$TOPAZ_TEST_ID-$(date +%Y%m%d-%H%M%S).log}"
mkdir -p "$(dirname "$LOG")"
# The harness's own variables and helpers for this slot: SSH_PORT, tssh,
# wait_ssh, vm_start/vm_stop through run.sh.
# shellcheck disable=SC1091
source "$TOPAZ_TESTS/lib.sh"
# A harness without TOPAZ_TEST_ID support puts every slot on port 2233.
if [[ "$TOPAZ_TEST_ID" != 0 && -z "${RUN_ART:-}" ]]; then
    echo "vm-run: $TOPAZ_TESTS/lib.sh has no TOPAZ_TEST_ID; run with CAUSTICS_VM_SLOTS=1"
    exit 9
fi

echo "vm-run: slot $TOPAZ_TEST_ID (ssh port $SSH_PORT) at $(date +%T); log $LOG"

t0=$SECONDS
started=0
if tssh true </dev/null 2>/dev/null; then
    echo "vm-run: guest $TOPAZ_TEST_ID already up (not ours; it stays up)"
else
    "$TOPAZ_TESTS/run.sh" start || exit 9
    started=1
    # A runner stopped by pid must not orphan its guest.
    trap '"$TOPAZ_TESTS/run.sh" stop' EXIT
    if ! wait_ssh 300 </dev/null; then
        echo "vm-run: guest $TOPAZ_TEST_ID did not answer ssh within 300 s"
        "$TOPAZ_TESTS/run.sh" stop
        exit 9
    fi
    echo "vm-run: guest $TOPAZ_TEST_ID answers ssh after $((SECONDS - t0)) s"
fi

# An immutable host lacks the unversioned `.so` symlinks the linker wants; a
# Homebrew prefix supplies them. Elsewhere the development packages do.
if [[ -d /var/home/linuxbrew/.linuxbrew/lib ]]; then
    export LIBRARY_PATH="/var/home/linuxbrew/.linuxbrew/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
fi
CAUSTICS_TARGET=vm bash "$SCRIPT" >"$LOG" 2>&1 </dev/null
rc=$?
echo "vm-run: $(basename "$SCRIPT") exit $rc; $((SECONDS - t0)) s in slot $TOPAZ_TEST_ID"
grep -E '^([A-Za-z]+ run:|=== timing:)' "$LOG"
[[ "$started" == 1 ]] && "$TOPAZ_TESTS/run.sh" stop
exit "$rc"
