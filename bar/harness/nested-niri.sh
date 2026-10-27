# shellcheck shell=bash
# A nested niri for the harness: its host compositor (a headless sway in a
# toolbox, or the developer's desktop, where it is a window), the nested niri
# itself from bar/harness/niri.kdl, and their teardown by pid. Sourced by
# bar/nested-run.sh (the nested gate) and bar/lock-try.sh (the lock, by eye),
# which own everything else: the run directory, the buses, what runs inside.
#
# The caller sets `REPO` before sourcing, and `NESTED_WHO`, the word its
# messages start with. The functions set the globals they name; nothing here
# sets a trap or exits, so each caller's teardown stays its own, and calls
# `nested_down` for this file's part of it.

NESTED_WHO="${NESTED_WHO:-nested}"
# The headless host is sway in a toolbox (podman) container, so the gate needs
# no sway on the host itself. The container and its image are the harness's
# own business; both can be named.
TOOLBOX="${CAUSTICS_NESTED_TOOLBOX:-caustics-headless}"
TOOLBOX_IMAGE="${CAUSTICS_NESTED_TOOLBOX_IMAGE:-registry.fedoraproject.org/fedora-toolbox:44}"

# The globals the functions set, none of them set here: the nested target
# sources this file for `nested_icd` with NESTED_WAYLAND and friends already
# exported to it.
#   HOST_PID     sway itself (headless), the process that owns the socket
#   TOOLBOX_PID  the `toolbox run` wrapper around it
#   HOST_DISPLAY NIRI_PID NESTED_WAYLAND NESTED_NIRI_SOCKET NESTED_MODE PREV_FOCUS

# The GPU the headless sway renders on: `$CAUSTICS_NESTED_RENDER_NODE`, else the
# first render node whose kernel driver is not NVIDIA's (a hybrid laptop's
# discrete GPU must never be woken by a test), else the first render node.
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

# The Vulkan driver of a render node's GPU, so a client is pinned to the GPU
# the host renders on and never wakes a discrete one, which is what the
# iGPU-only drop-in does for the installed unit. With one GPU, or a driver
# this does not know, nothing: the client takes whatever Vulkan offers.
nested_icd() { # nested_icd <render node> -> "<icd file> <adapter word>" or nothing
    local node="$1" driver icd word
    (($(find /sys/class/drm -maxdepth 1 -name 'renderD*' | wc -l) > 1)) || return 0
    driver=$(basename "$(readlink -f "/sys/class/drm/$(basename "$node")/device/driver")")
    case "$driver" in
        amdgpu | radeon) icd=radeon_icd; word=RADV ;;
        i915 | xe) icd=intel_icd; word=Intel ;;
        *) return 0 ;;
    esac
    icd="/usr/share/vulkan/icd.d/$icd.$(uname -m).json"
    [[ -f "$icd" ]] && printf '%s %s' "$icd" "$word"
}

alive() { [[ -n "$1" ]] && kill -0 "$1" 2>/dev/null; }
stop_pid() { # stop_pid <pid> <what>: TERM, five seconds, KILL
    local pid="$1"
    alive "$pid" || return 0
    kill -TERM "$pid" 2>/dev/null
    for _ in $(seq 50); do alive "$pid" || return 0; sleep 0.1; done
    echo "$NESTED_WHO: $2 (pid $pid) ignored TERM; KILL"
    kill -KILL "$pid" 2>/dev/null
}

strip() { sed 's/\x1b\[[0-9;]*m//g' "$1"; }

# First use of the headless host. Another run may be doing the same at the
# same moment, so a failed create or install is judged by what is there
# afterwards, not by its exit status.
nested_toolbox_ready() { # nested_toolbox_ready <run dir> -> 0, or 9 with a message
    local run="$1"
    if ! toolbox run -c "$TOOLBOX" true >/dev/null 2>&1; then
        echo "$NESTED_WHO: creating the '$TOOLBOX' toolbox (first use; sway goes in it, nothing else)"
        toolbox create -y -c "$TOOLBOX" --image "$TOOLBOX_IMAGE" >"$run/toolbox-create.log" 2>&1 ||
            echo "$NESTED_WHO: toolbox create said: $(tail -1 "$run/toolbox-create.log")"
        toolbox run -c "$TOOLBOX" true >/dev/null 2>&1 || {
            echo "$NESTED_WHO: the '$TOOLBOX' toolbox is not usable; $run/toolbox-create.log"
            return 9
        }
    fi
    if ! toolbox run -c "$TOOLBOX" sh -c 'command -v sway' >/dev/null 2>&1; then
        echo "$NESTED_WHO: installing sway into '$TOOLBOX' (first use)"
        toolbox run -c "$TOOLBOX" sudo dnf install -y sway >"$run/toolbox-dnf.log" 2>&1 ||
            echo "$NESTED_WHO: dnf install sway said: $(tail -1 "$run/toolbox-dnf.log")"
        toolbox run -c "$TOOLBOX" sh -c 'command -v sway' >/dev/null 2>&1 || {
            echo "$NESTED_WHO: sway is not in '$TOOLBOX'; $run/toolbox-dnf.log"
            return 9
        }
    fi
}

# The host compositor: the Wayland display the nested niri is a window on.
# Sets HOST_DISPLAY, and HOST_PID and TOOLBOX_PID (headless) or PREV_FOCUS
# (desktop).
nested_host_up() { # nested_host_up <headless|desktop> <run dir> <runtime dir> <render node>
    local kind="$1" run="$2" rt="$3" render_node="$4" host_socket p sway_conf
    HOST_PID="" TOOLBOX_PID="" PREV_FOCUS=""
    if [[ "$kind" == headless ]]; then
        # A copy of the config named for this run, so the sway this run started
        # is the one process whose command line names it — that is how its pid
        # is found (the toolbox wraps it, and its environ is not readable from
        # here), and it is never found by its name alone.
        sway_conf="$run/sway-$(basename "$run").conf"
        cp "$REPO/bar/harness/sway.conf" "$sway_conf"
        toolbox run -c "$TOOLBOX" env XDG_RUNTIME_DIR="$rt" \
            WLR_BACKENDS=headless WLR_RENDER_DRM_DEVICE="$render_node" WLR_LIBINPUT_NO_DEVICES=1 \
            sway -V -c "$sway_conf" >"$run/sway.log" 2>&1 </dev/null &
        TOOLBOX_PID=$!
        for _ in $(seq 300); do
            grep -q "Running compositor on wayland display" "$run/sway.log" && break
            alive "$TOOLBOX_PID" || break
            sleep 0.1
        done
        host_socket=$(sed -n "s/.*Running compositor on wayland display '\([^']*\)'.*/\1/p" "$run/sway.log" | tail -1)
        for p in $(pgrep -u "$(id -u)" -x sway); do
            tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -qF -- "$sway_conf" && HOST_PID=$p
        done
        if [[ -z "$host_socket" || -z "$HOST_PID" ]]; then
            echo "$NESTED_WHO: the headless sway did not come up (socket '${host_socket:-}', pid '${HOST_PID:-}')"
            tail -20 "$run/sway.log"
            return 9
        fi
        echo "host compositor: headless sway pid $HOST_PID, socket $host_socket ($(grep -oE 'GL renderer: .*|GL vendor: .*' "$run/sway.log" | head -2 | tr '\n' ' '))"
        HOST_DISPLAY="$rt/$host_socket"
    else
        if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
            echo "$NESTED_WHO: the desktop host needs a Wayland session (WAYLAND_DISPLAY is unset)"
            return 9
        fi
        HOST_DISPLAY="${XDG_RUNTIME_DIR}/${WAYLAND_DISPLAY}"
        PREV_FOCUS=$(niri msg --json focused-window 2>/dev/null |
            python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["id"] if d else "")' 2>/dev/null)
        echo "host compositor: the desktop ($HOST_DISPLAY); a window will appear for the run"
    fi
}

# The nested niri. No DISPLAY (winit would try X), no NIRI_SOCKET (its own
# is read from its log), the private runtime dir for its sockets.
# No session bus either: niri posts a notification for every screenshot
# ("Screenshot captured"), and on the gate's bus that is caustics-notify's
# history — the steps count it; on the desktop's it would be the developer's.
# Its children (the dock's "New window") inherit the rest: the scratch XDG
# tree and a plain `sh`. Sets NIRI_PID (as soon as it is started, so a
# teardown finds it even when this fails), NESTED_WAYLAND,
# NESTED_NIRI_SOCKET and NESTED_MODE (`<output> <w>x<h>`).
nested_niri_up() { # nested_niri_up <headless|desktop> <run dir> <runtime dir>
    local kind="$1" run="$2" rt="$3" win scale
    NESTED_WAYLAND="" NESTED_NIRI_SOCKET="" NESTED_MODE=""
    mkdir -p "$run/home" "$run/xdg/config" "$run/xdg/state" "$run/xdg/cache"
    env -u DISPLAY -u NIRI_SOCKET WAYLAND_DISPLAY="$HOST_DISPLAY" XDG_RUNTIME_DIR="$rt" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=$run/no-bus" \
        XDG_CONFIG_HOME="$run/xdg/config" XDG_STATE_HOME="$run/xdg/state" \
        XDG_CACHE_HOME="$run/xdg/cache" SHELL=/bin/sh HISTFILE="$run/home/.sh_history" \
        RUST_LOG="niri=debug,smithay::backend::renderer=info,smithay::backend::egl=info" \
        setsid niri --config "$REPO/bar/harness/niri.kdl" >"$run/niri.log" 2>&1 </dev/null &
    NIRI_PID=$!
    for _ in $(seq 300); do
        strip "$run/niri.log" | grep -q "IPC listening on" && break
        alive "$NIRI_PID" || break
        sleep 0.1
    done
    NESTED_WAYLAND=$(strip "$run/niri.log" | sed -n 's/.*listening on Wayland socket: //p' | tail -1)
    NESTED_NIRI_SOCKET=$(strip "$run/niri.log" | sed -n 's/.*IPC listening on: //p' | tail -1)
    if [[ -z "$NESTED_WAYLAND" || -z "$NESTED_NIRI_SOCKET" ]]; then
        echo "$NESTED_WHO: the nested niri did not come up"
        strip "$run/niri.log" | tail -20
        return 9
    fi
    echo "nested niri: pid $NIRI_PID, socket $NESTED_WAYLAND, ipc $NESTED_NIRI_SOCKET"

    if [[ "$kind" == desktop ]]; then
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
        [[ -n "${PREV_FOCUS:-}" ]] && niri msg action focus-window --id "$PREV_FOCUS" >/dev/null 2>&1
        sleep 1
    fi
    # shellcheck disable=SC2034 # the callers' to read
    NESTED_MODE=$(NIRI_SOCKET="$NESTED_NIRI_SOCKET" niri msg --json outputs | python3 -c '
import json, sys
for name, o in json.load(sys.stdin).items():
    m = o["modes"][o["current_mode"]]
    print("%s %dx%d" % (name, m["width"], m["height"]))' 2>/dev/null)
}

# This file's part of a teardown, in order: the nested niri, the host
# compositor, and on the desktop the developer's focus handed back.
nested_down() {
    stop_pid "${NIRI_PID:-}" "nested niri"
    stop_pid "${HOST_PID:-}" "headless sway"
    stop_pid "${TOOLBOX_PID:-}" "toolbox run"
    if [[ -n "${PREV_FOCUS:-}" ]]; then
        niri msg action focus-window --id "$PREV_FOCUS" >/dev/null 2>&1
    fi
}
