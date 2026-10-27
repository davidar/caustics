#!/usr/bin/env bash
# The nested gate (docs/testing.md, *The nested gate*): bar/bar-test.sh's steps
# against a nested niri on this machine's real GPU, in minutes. Use this; it is
# the only way the `nested` target starts.
#
#   bar/nested-run.sh             # headless: nothing appears on the desktop
#   CAUSTICS_NESTED_HOST=desktop bar/nested-run.sh   # a window, to watch by eye
#
# 1. Runs are independent and may overlap. Each has its own run directory,
#    runtime dir, session bus, host sway and nested niri; the GPU and the
#    toolbox container are all that two runs share, and the container only
#    needs its first-use creation to tolerate a race. A run still waits while
#    a VM target slot (`~/.cache/caustics-vm*.lock`) is held: a lavapipe guest
#    loads every core, and the steps' time bounds assume a machine that is
#    not rendering a second desktop in software.
# 2. The run directory (`$CAUSTICS_NESTED_LOG`'s, or bar/test-runs/), the log
#    whose path is printed first, and the build: the workspace, the examples
#    the steps run, the oracle's test binary. The "set" is `target/debug/`,
#    where the bar finds its sidecars beside its own exe; nothing is installed.
# 3. Under `dbus-run-session`, so the session bus is the run's alone (the
#    system bus stays the machine's): the nested niri's **host** — by default
#    a headless sway in the `caustics-headless` toolbox, created on first use
#    (docs/testing.md says why headless) — then the nested niri from
#    bar/harness/niri.kdl, both under a private `$XDG_RUNTIME_DIR`, their
#    sockets read from their own stderr; then bar-test.sh with
#    `CAUSTICS_TARGET=nested`, capped at ten minutes.
# 4. On every exit path, by pid and in order: bar-test.sh's own cleanup
#    (clients, fixtures, the bar under its supervisor), anything in the run's
#    pid file that outlived it, the nested niri, the host compositor. Never by
#    name: the developer's own terminal, niri and bar share these binaries.

set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$REPO/bar/nested-run.sh"
HOST_KIND="${CAUSTICS_NESTED_HOST:-headless}"
CAP_S="${CAUSTICS_NESTED_CAP_S:-600}"
# The host compositor, the nested niri and their teardown, shared with
# bar/lock-try.sh; the toolbox the headless host runs in is named there.
NESTED_WHO=nested-run
# shellcheck source=bar/harness/nested-niri.sh
source "$REPO/bar/harness/nested-niri.sh"
# The GPU the headless sway renders on (first_render_node says how it is
# chosen). Exported, because the nested target pins the bar's Vulkan driver
# to the same GPU (bar/targets/nested.sh).
RENDER_NODE="${CAUSTICS_NESTED_RENDER_NODE:-$(first_render_node)}"
[[ -n "$RENDER_NODE" ]] || { echo "nested-run: no /dev/dri/renderD* node; the gate needs a GPU"; exit 2; }
export CAUSTICS_NESTED_RENDER_NODE="$RENDER_NODE"

case "$HOST_KIND" in
    headless | desktop) ;;
    *) echo "nested-run: CAUSTICS_NESTED_HOST is headless or desktop, not '$HOST_KIND'"; exit 2 ;;
esac

# --- 1. the VM wait ------------------------------------------------------------

vm_slots_busy() {
    local l
    for l in "$HOME"/.cache/caustics-vm.lock "$HOME"/.cache/caustics-vm.[0-9]*.lock; do
        [[ -e "$l" ]] || continue
        flock -n "$l" true 2>/dev/null || { printf '%s' "$l"; return 0; }
    done
    return 1
}

# --- 3. inside the private bus ------------------------------------------------

if [[ -n "${CAUSTICS_NESTED_INNER:-}" ]]; then
    RUN="$NESTED_RUN_DIR"
    case "${DBUS_SESSION_BUS_ADDRESS:-}" in
        "" | *"/run/user/$(id -u)/bus"*)
            echo "nested-run: not inside dbus-run-session ($DBUS_SESSION_BUS_ADDRESS); refusing"
            exit 9
            ;;
    esac
    PIDS_FILE="$RUN/pids"
    : >"$PIDS_FILE"
    RT="$XDG_RUNTIME_DIR/caustics-nested.$$"
    mkdir -m 700 "$RT"
    TEST_PID=""

    teardown() {
        local role pid
        trap - EXIT INT TERM
        # The steps' own cleanup runs in bar-test.sh's EXIT trap; a TERM asks for it.
        if alive "$TEST_PID"; then
            kill -TERM "$TEST_PID" 2>/dev/null
            for _ in $(seq 300); do alive "$TEST_PID" || break; sleep 0.1; done
            alive "$TEST_PID" && kill -KILL "$TEST_PID" 2>/dev/null
        fi
        # Whatever outlived it, from the pid file, in order: clients, fixtures,
        # the supervisor, then the bar's whole process group.
        for role in client fixture supervisor; do
            while read -r r pid; do
                [[ "$r" == "$role" ]] && stop_pid "$pid" "$role"
            done <"$PIDS_FILE"
        done
        while read -r r pid; do
            [[ "$r" == bar ]] || continue
            kill -0 -- "-$pid" 2>/dev/null || continue
            echo "nested-run: the bar's group $pid outlived the run; TERM"
            kill -TERM -- "-$pid" 2>/dev/null
            sleep 1
            kill -KILL -- "-$pid" 2>/dev/null
        done <"$PIDS_FILE"
        nested_down
        rm -rf "$RT"
        echo "nested-run: torn down (niri ${NIRI_PID:-none}, host ${HOST_PID:-none})"
    }
    trap teardown EXIT
    trap 'exit 130' INT TERM

    nested_host_up "$HOST_KIND" "$RUN" "$RT" "$RENDER_NODE" || exit 9
    nested_niri_up "$HOST_KIND" "$RUN" "$RT"
    niri_rc=$?
    echo "niri $NIRI_PID" >>"$PIDS_FILE"
    ((niri_rc == 0)) || exit 9
    mode="$NESTED_MODE"
    echo "nested output: $mode"
    [[ "$mode" == "winit 1280x800" ]] || echo "nested-run: the output is not 1280x800; the steps assume it"

    export NESTED_RUN_DIR="$RUN" NESTED_RT="$RT" NESTED_WAYLAND NESTED_NIRI_SOCKET
    left=$((CAP_S - SECONDS))
    CAUSTICS_TARGET=nested timeout --signal=TERM --kill-after=30 "$left" \
        bash "$REPO/bar/bar-test.sh" </dev/null &
    TEST_PID=$!
    wait "$TEST_PID"
    rc=$?
    TEST_PID=""
    if [[ "$rc" == 124 ]]; then
        echo "nested-run: the run hit its ${CAP_S} s cap"
    fi
    # What the nested compositors drew with, for the record: a fall back to a
    # software renderer would make the run's timings meaningless.
    echo "nested niri renderer: $(strip "$RUN/niri.log" | grep -iE 'renderer|GL_RENDERER|egl.*device|llvmpipe|softpipe' | head -3 | tr '\n' ' ')"
    exit "$rc"
fi

# --- 2. the run directory, the build, the host ----------------------------------

waited=0
while busy=$(vm_slots_busy); do
    ((waited == 0)) && echo "nested-run: $busy is held — a VM guest is running; waiting for it ($(date +%T))"
    waited=1
    sleep 15
done

# The pid keeps two runs started in the same second apart.
stamp=$(date +%Y%m%d-%H%M%S)-$$
LOG="${CAUSTICS_NESTED_LOG:-$REPO/bar/test-runs/nested-run-$stamp.log}"
RUN="$(dirname "$LOG")/nested-$stamp"
mkdir -p "$RUN"
echo "nested-run: $HOST_KIND host at $(date +%T); log $LOG; run directory $RUN"

t0=$SECONDS
build() {
    (cd "$REPO" &&
        cargo build --offline --workspace &&
        cargo build --offline -p caustics-power --example fake_upower &&
        cargo build --offline -p caustics-bar-abi --example rows_demo &&
        cargo build --offline -p caustics-lock --example fake_fprintd &&
        cargo test --offline -p caustics-bar --no-run)
}
if ! build >"$RUN/build.log" 2>&1; then
    echo "nested-run: the build failed; $RUN/build.log"
    tail -20 "$RUN/build.log"
    exit 1
fi
echo "nested-run: built in $((SECONDS - t0)) s"

if [[ "$HOST_KIND" == headless ]]; then
    nested_toolbox_ready "$RUN" || exit 9
fi

t1=$SECONDS
NESTED_RUN_DIR="$RUN" CAUSTICS_NESTED_INNER=1 CAUSTICS_NESTED_HOST="$HOST_KIND" \
    dbus-run-session -- bash "$SELF" >"$LOG" 2>&1 </dev/null
rc=$?
echo "nested-run: bar-test.sh exit $rc; $((SECONDS - t1)) s under the cap, $((SECONDS - t0)) s with the build"
grep -E '^([A-Za-z]+ run:|=== timing:|host compositor:|nested output:|nested niri renderer:|nested-run: )' "$LOG"
exit "$rc"
