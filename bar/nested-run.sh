#!/usr/bin/env bash
# The nested gate (docs/testing.md, *The nested gate*): bar/bar-test.sh's steps
# against a nested niri on this machine's real GPU, in minutes. Use this; it is
# the only way the `nested` target starts.
#
#   bar/nested-run.sh             # headless: nothing appears on the desktop
#   CAUSTICS_NESTED_HOST=desktop bar/nested-run.sh   # a window, to watch by eye
#
# 1. One at a time: `~/.cache/caustics-nested.lock`, and never beside a run of the
#    VM target — while any `~/.cache/caustics-vm*.lock` slot is held, it waits.
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
# The headless host is sway in a toolbox (podman) container, so the gate needs
# no sway on the host itself. The container and its image are the harness's
# own business; both can be named.
TOOLBOX="${CAUSTICS_NESTED_TOOLBOX:-caustics-headless}"
TOOLBOX_IMAGE="${CAUSTICS_NESTED_TOOLBOX_IMAGE:-registry.fedoraproject.org/fedora-toolbox:44}"
# The GPU the headless sway renders on: `$CAUSTICS_NESTED_RENDER_NODE`, else the
# first render node whose kernel driver is not NVIDIA's (a hybrid laptop's
# discrete GPU must never be woken by a test), else the first render node.
# Exported, because the nested target pins the bar's Vulkan driver to the same
# GPU (bar/targets/nested.sh).
first_render_node() {
    local node driver first=""
    for node in /dev/dri/renderD*; do
        [[ -e "$node" ]] || continue
        first="${first:-$node}"
        driver=$(basename "$(readlink -f "/sys/class/drm/$(basename "$node")/device/driver")")
        [[ "$driver" == nvidia ]] && continue
        printf '%s' "$node"
        return
    done
    printf '%s' "$first"
}
RENDER_NODE="${CAUSTICS_NESTED_RENDER_NODE:-$(first_render_node)}"
[[ -n "$RENDER_NODE" ]] || { echo "nested-run: no /dev/dri/renderD* node; the gate needs a GPU"; exit 2; }
export CAUSTICS_NESTED_RENDER_NODE="$RENDER_NODE"

case "$HOST_KIND" in
    headless | desktop) ;;
    *) echo "nested-run: CAUSTICS_NESTED_HOST is headless or desktop, not '$HOST_KIND'"; exit 2 ;;
esac

# --- 1. the lock -------------------------------------------------------------

vm_slots_busy() {
    local l
    for l in "$HOME"/.cache/caustics-vm.lock "$HOME"/.cache/caustics-vm.[0-9]*.lock; do
        [[ -e "$l" ]] || continue
        flock -n "$l" true 2>/dev/null || { printf '%s' "$l"; return 0; }
    done
    return 1
}

if [[ -z "${CAUSTICS_NESTED_LOCK_HELD:-}" && -z "${CAUSTICS_NESTED_INNER:-}" ]]; then
    mkdir -p "$HOME/.cache"
    CAUSTICS_NESTED_LOCK_HELD=1 exec flock -o -w 7200 "$HOME/.cache/caustics-nested.lock" bash "$SELF" "$@"
fi

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
    HOST_PID=""      # sway itself (headless) — the process that owns the socket
    TOOLBOX_PID=""   # the `toolbox run` wrapper around it
    NIRI_PID=""
    TEST_PID=""
    PREV_FOCUS=""

    alive() { [[ -n "$1" ]] && kill -0 "$1" 2>/dev/null; }
    stop_pid() { # stop_pid <pid> <what>: TERM, five seconds, KILL
        local pid="$1" i
        alive "$pid" || return 0
        kill -TERM "$pid" 2>/dev/null
        for i in $(seq 50); do alive "$pid" || return 0; sleep 0.1; done
        echo "nested-run: $2 (pid $pid) ignored TERM; KILL"
        kill -KILL "$pid" 2>/dev/null
    }

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
        stop_pid "$NIRI_PID" "nested niri"
        stop_pid "$HOST_PID" "headless sway"
        stop_pid "$TOOLBOX_PID" "toolbox run"
        if [[ -n "$PREV_FOCUS" ]]; then
            niri msg action focus-window --id "$PREV_FOCUS" >/dev/null 2>&1
        fi
        rm -rf "$RT"
        echo "nested-run: torn down (niri ${NIRI_PID:-none}, host ${HOST_PID:-none})"
    }
    trap teardown EXIT
    trap 'exit 130' INT TERM

    strip() { sed 's/\x1b\[[0-9;]*m//g' "$1"; }

    # The host compositor: the Wayland display the nested niri is a window on.
    if [[ "$HOST_KIND" == headless ]]; then
        # A copy of the config named for this run, so the sway this run started
        # is the one process whose command line names it — that is how its pid
        # is found (the toolbox wraps it, and its environ is not readable from
        # here), and it is never found by its name alone.
        SWAY_CONF="$RUN/sway-$(basename "$RUN").conf"
        cp "$REPO/bar/harness/sway.conf" "$SWAY_CONF"
        toolbox run -c "$TOOLBOX" env XDG_RUNTIME_DIR="$RT" \
            WLR_BACKENDS=headless WLR_RENDER_DRM_DEVICE="$RENDER_NODE" WLR_LIBINPUT_NO_DEVICES=1 \
            sway -V -c "$SWAY_CONF" >"$RUN/sway.log" 2>&1 </dev/null &
        TOOLBOX_PID=$!
        for _ in $(seq 300); do
            grep -q "Running compositor on wayland display" "$RUN/sway.log" && break
            alive "$TOOLBOX_PID" || break
            sleep 0.1
        done
        host_socket=$(sed -n "s/.*Running compositor on wayland display '\([^']*\)'.*/\1/p" "$RUN/sway.log" | tail -1)
        for p in $(pgrep -u "$(id -u)" -x sway); do
            tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -qF -- "$SWAY_CONF" && HOST_PID=$p
        done
        if [[ -z "$host_socket" || -z "$HOST_PID" ]]; then
            echo "nested-run: the headless sway did not come up (socket '${host_socket:-}', pid '${HOST_PID:-}')"
            tail -20 "$RUN/sway.log"
            exit 9
        fi
        echo "host compositor: headless sway pid $HOST_PID, socket $host_socket ($(grep -oE 'GL renderer: .*|GL vendor: .*' "$RUN/sway.log" | head -2 | tr '\n' ' '))"
        HOST_DISPLAY="$RT/$host_socket"
    else
        HOST_DISPLAY="${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY:?desktop needs a Wayland session}"
        PREV_FOCUS=$(niri msg --json focused-window 2>/dev/null |
            python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["id"] if d else "")' 2>/dev/null)
        echo "host compositor: the desktop ($HOST_DISPLAY); a window will appear for the run"
    fi

    # The nested niri. No DISPLAY (winit would try X), no NIRI_SOCKET (its own
    # is read from its log), the private runtime dir for its sockets.
    # No session bus either: niri posts a notification for every screenshot
    # ("Screenshot captured"), and on the run's bus that is caustics-notify's
    # history — the steps count it. Its children (the dock's "New window")
    # inherit the rest: the scratch XDG tree and a plain `sh`.
    mkdir -p "$RUN/home" "$RUN/xdg/config" "$RUN/xdg/state" "$RUN/xdg/cache"
    env -u DISPLAY -u NIRI_SOCKET WAYLAND_DISPLAY="$HOST_DISPLAY" XDG_RUNTIME_DIR="$RT" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=$RUN/no-bus" \
        XDG_CONFIG_HOME="$RUN/xdg/config" XDG_STATE_HOME="$RUN/xdg/state" \
        XDG_CACHE_HOME="$RUN/xdg/cache" SHELL=/bin/sh HISTFILE="$RUN/home/.sh_history" \
        RUST_LOG="niri=debug,smithay::backend::renderer=info,smithay::backend::egl=info" \
        setsid niri --config "$REPO/bar/harness/niri.kdl" >"$RUN/niri.log" 2>&1 </dev/null &
    NIRI_PID=$!
    echo "niri $NIRI_PID" >>"$PIDS_FILE"
    for _ in $(seq 300); do
        strip "$RUN/niri.log" | grep -q "IPC listening on" && break
        alive "$NIRI_PID" || break
        sleep 0.1
    done
    NESTED_WAYLAND=$(strip "$RUN/niri.log" | sed -n 's/.*listening on Wayland socket: //p' | tail -1)
    NESTED_NIRI_SOCKET=$(strip "$RUN/niri.log" | sed -n 's/.*IPC listening on: //p' | tail -1)
    if [[ -z "$NESTED_WAYLAND" || -z "$NESTED_NIRI_SOCKET" ]]; then
        echo "nested-run: the nested niri did not come up"
        strip "$RUN/niri.log" | tail -20
        exit 9
    fi
    echo "nested niri: pid $NIRI_PID, socket $NESTED_WAYLAND, ipc $NESTED_NIRI_SOCKET"

    if [[ "$HOST_KIND" == desktop ]]; then
        # Floating at the VM target's size on the desktop's niri, so it does
        # not push the developer's columns about, and their focus handed back. The size is the host's
        # logical px, so at a fractional scale it is 1280x800 only nearly.
        win=""
        for _ in $(seq 50); do
            win=$(niri msg --json windows | python3 -c "
import json, sys
for w in json.load(sys.stdin):
    if w.get('pid') == $NIRI_PID: print(w['id'])" 2>/dev/null)
            [[ -n "$win" ]] && break
            sleep 0.1
        done
        scale=$(niri msg --json focused-output | python3 -c 'import json,sys; print(json.load(sys.stdin)["logical"]["scale"])' 2>/dev/null)
        if [[ -n "$win" ]]; then
            niri msg action toggle-window-floating --id "$win" >/dev/null
            niri msg action set-window-width --id "$win" "$(python3 -c "print(round(1280 / ${scale:-1}))")" >/dev/null
            niri msg action set-window-height --id "$win" "$(python3 -c "print(round(800 / ${scale:-1}))")" >/dev/null
        fi
        [[ -n "$PREV_FOCUS" ]] && niri msg action focus-window --id "$PREV_FOCUS" >/dev/null 2>&1
        sleep 1
    fi
    mode=$(NIRI_SOCKET="$NESTED_NIRI_SOCKET" niri msg --json outputs | python3 -c '
import json, sys
for name, o in json.load(sys.stdin).items():
    m = o["modes"][o["current_mode"]]
    print("%s %dx%d" % (name, m["width"], m["height"]))' 2>/dev/null)
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

# --- 2. holding the lock: the run directory, the build, the host ----------------

waited=0
while busy=$(vm_slots_busy); do
    ((waited == 0)) && echo "nested-run: $busy is held — a VM guest is running; waiting for it ($(date +%T))"
    waited=1
    sleep 15
done

stamp=$(date +%Y%m%d-%H%M%S)
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
        cargo test --offline -p caustics-bar --no-run)
}
if ! build >"$RUN/build.log" 2>&1; then
    echo "nested-run: the build failed; $RUN/build.log"
    tail -20 "$RUN/build.log"
    exit 1
fi
echo "nested-run: built in $((SECONDS - t0)) s"

if [[ "$HOST_KIND" == headless ]]; then
    if ! toolbox run -c "$TOOLBOX" true >/dev/null 2>&1; then
        echo "nested-run: creating the '$TOOLBOX' toolbox (first use; sway goes in it, nothing else)"
        toolbox create -y -c "$TOOLBOX" --image "$TOOLBOX_IMAGE" || exit 9
    fi
    if ! toolbox run -c "$TOOLBOX" sh -c 'command -v sway' >/dev/null 2>&1; then
        echo "nested-run: installing sway into '$TOOLBOX' (first use)"
        toolbox run -c "$TOOLBOX" sudo dnf install -y sway >"$RUN/toolbox-dnf.log" 2>&1 || {
            echo "nested-run: dnf install sway failed; $RUN/toolbox-dnf.log"
            exit 9
        }
    fi
fi

t1=$SECONDS
NESTED_RUN_DIR="$RUN" CAUSTICS_NESTED_INNER=1 CAUSTICS_NESTED_HOST="$HOST_KIND" \
    dbus-run-session -- bash "$SELF" >"$LOG" 2>&1 </dev/null
rc=$?
echo "nested-run: bar-test.sh exit $rc; $((SECONDS - t1)) s under the cap, $((SECONDS - t0)) s with the build"
grep -E '^([A-Za-z]+ run:|=== timing:|host compositor:|nested output:|nested niri renderer:|nested-run: )' "$LOG"
exit "$rc"
