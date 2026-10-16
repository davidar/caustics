# shellcheck shell=bash
# The nested target of bar/bar-test.sh (docs/testing.md, *The nested gate*): the
# bar against a nested niri on this machine's real GPU, with a private session
# bus.
# Sourced by bar-test.sh when `CAUSTICS_TARGET=nested`; `bar/nested-run.sh` is the
# only thing that sets that, and it has already, before this file is read:
#
#   - taken `~/.cache/caustics-nested.lock` and built the workspace (the "set" is
#     `target/debug/`, where the bar finds its sidecars beside its own exe);
#   - wrapped the run in `dbus-run-session`, so `$DBUS_SESSION_BUS_ADDRESS`
#     is a bus nobody else is on;
#   - started the nested niri (on a headless sway, or in a window on the
#     desktop) under a private `$XDG_RUNTIME_DIR`, and exported
#     `NESTED_RUN_DIR NESTED_RT NESTED_WAYLAND NESTED_NIRI_SOCKET`.
#
# The surface is vm.sh's (its header lists it). What differs is underneath:
# no unit, so a **supervisor** — a bash loop that runs `target/debug/caustics-bar`
# with an environment file per group sourced, restarts it the way the unit's
# `Restart=always RestartSec=1 RestartPreventExitStatus=2 3` would, and counts
# those restarts as `NRestarts` — and no journal, so the bar's stderr (and
# its sidecars', which inherit it) goes to a file, a line count is the cursor,
# and a second file carries each line's arrival time for `-o short-unix`.
#
# **Nothing here may reach the developer's own session.** The bar, its sidecars, every
# client and every fixture see `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
# `XDG_CACHE_HOME` under the run directory, the run's own runtime dir (so the
# bar's edge lock and the Wayland sockets are the run's), the nested niri's
# sockets and the private bus; `HOME` stays the real one so `caustics-apps` finds
# the real desktop entries. `on_target` refuses any command that names
# systemd, the journal, a by-name kill, or a path under the user's real config
# or state.

T_NAME=nested
: "${NESTED_RUN_DIR:?nested.sh needs bar/nested-run.sh}"
: "${NESTED_RT:?}" "${NESTED_WAYLAND:?}" "${NESTED_NIRI_SOCKET:?}"
RUN="$NESTED_RUN_DIR"
SHOTS="$RUN"

OUTPUT=winit
SCREEN_W=1280
SCREEN_H=800
# The bar's Vulkan driver, and the adapter name the start-up step expects in the bar's log.
# `$CAUSTICS_NESTED_VK_ICD` and `$CAUSTICS_NESTED_ADAPTER` set them outright. Otherwise,
# on a machine with more than one GPU, the bar is pinned to the driver of the
# GPU the headless sway renders on (`$CAUSTICS_NESTED_RENDER_NODE`, chosen by
# bar/nested-run.sh to be a non-NVIDIA one), so a test never wakes a discrete
# GPU, which is what the iGPU-only drop-in does for the installed unit. With
# one GPU, or a driver this does not know, nothing is pinned and the bar takes
# whatever Vulkan offers; the start-up step then checks only that an adapter
# was chosen.
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
read -r derived_icd derived_adapter <<<"$(nested_icd "${CAUSTICS_NESTED_RENDER_NODE:-}")"
if [[ -n "${CAUSTICS_NESTED_VK_ICD:-}" ]]; then
    VK_ICD="$CAUSTICS_NESTED_VK_ICD"
    T_ADAPTER="${CAUSTICS_NESTED_ADAPTER:-wgpu adapter}"
else
    VK_ICD="${derived_icd:-}"
    T_ADAPTER="${CAUSTICS_NESTED_ADAPTER:-${derived_adapter:-wgpu adapter}}"
fi
# The pin as an `env` argument, or nothing: an empty VK_DRIVER_FILES is not
# the same as an unset one to every loader.
VK_ENV=()
[[ -n "$VK_ICD" ]] && VK_ENV=(VK_DRIVER_FILES="$VK_ICD")
echo "nested.sh: Vulkan driver ${VK_ICD:-not pinned}; the bar's adapter must be '$T_ADAPTER'" >&2
U_HOME="$RUN/home"
U_SESSION_BUS="$DBUS_SESSION_BUS_ADDRESS"

case "$DBUS_SESSION_BUS_ADDRESS" in
    "" | *"/run/user/$(id -u)/bus"*)
        echo "nested.sh: the session bus is the desktop's own ($DBUS_SESSION_BUS_ADDRESS); refusing" >&2
        exit 9
        ;;
esac

# The scratch tree. `caustics/` is a **copy** of the checkout's, taken at the
# start of the run: the hot-reload step edits `bar_tunables.slang` in place
# and puts it back, and that must never be the checkout's tracked file.
mkdir -p "$RUN/home" "$RUN/xdg/config" "$RUN/xdg/state" "$RUN/xdg/cache" "$RUN/bin" "$RUN/env.d"
if [[ ! -d "$RUN/caustics" ]]; then
    cp -a "$REPO/caustics" "$RUN/caustics"
fi
# The bar's PATH, the unit's shape: the run's bin (rows_demo by bare name, as
# an installed applet would resolve, and slangc), then the system's. Not
# `~/.local/bin`, which may hold an installed set: a sidecar missing from
# `target/debug` must be absent here, never the installed one.
ln -sfn "$REPO/target/debug/examples/rows_demo" "$RUN/bin/rows_demo"
ln -sfn "$(command -v slangc || echo "$HOME/.local/bin/slangc")" "$RUN/bin/slangc"
BAR_PATH="$RUN/bin:/usr/local/bin:/usr/bin:/bin"

BAR_LOG="$RUN/bar.log"
BAR_TS_LOG="$RUN/bar.ts.log"
: >>"$BAR_LOG"
: >>"$BAR_TS_LOG"
PIDS_FILE="$RUN/pids"
POINTER="$REPO/target/debug/caustics-pointer"

# Everything the run starts is written here as "<role> <pid>", so
# bar/nested-run.sh can stop it even if this script was killed outright.
track() { printf '%s %s\n' "$1" "$2" >>"$PIDS_FILE"; }

# The environment everything on the target runs in, as shell text: the
# paths the steps name and the three env functions (each a no-op or nearly:
# the run's environment *is* the session's here).
nested_prelude() {
    printf 'T_HOME=%q; T_CONFIG=%q; T_STATE=%q; T_CACHE=%q; T_BIN=%q; T_CAUSTICS=%q; ' \
        "$RUN/home" "$RUN/xdg/config" "$RUN/xdg/state" "$RUN/xdg/cache" \
        "$REPO/target/debug" "$RUN/caustics"
    printf 'T_SLANGC=%q; T_ORACLE=%q; T_FAKE_UPOWER=%q; ' \
        "$RUN/bin/slangc" "${ORACLE_BIN:-}" "${FAKE_UPOWER_BIN:-}"
    printf 'export WAYLAND_DISPLAY=%q XDG_RUNTIME_DIR=%q NIRI_SOCKET=%q ' \
        "$NESTED_WAYLAND" "$NESTED_RT" "$NESTED_NIRI_SOCKET"
    printf 'XDG_CONFIG_HOME=%q XDG_STATE_HOME=%q XDG_CACHE_HOME=%q CAUSTICS_DIR=%q ' \
        "$RUN/xdg/config" "$RUN/xdg/state" "$RUN/xdg/cache" "$RUN/caustics"
    printf 'DBUS_SESSION_BUS_ADDRESS=%q; unset DISPLAY; ' "$DBUS_SESSION_BUS_ADDRESS"
    if [[ -n "$VK_ICD" ]]; then
        printf 'bus_env() { :; }; gui_env() { :; }; vk_env() { export VK_DRIVER_FILES=%q; }; ' "$VK_ICD"
    else
        printf 'bus_env() { :; }; gui_env() { :; }; vk_env() { :; }; '
    fi
}

# The tripwire: a step that names a systemd tool, a by-name kill or a path
# under the user's real configuration fails loudly here instead of touching
# the developer's session.
nested_refuses() { # nested_refuses <command> -> 0 when it must not run
    local cmd="$1"
    [[ "$cmd" =~ (systemctl|journalctl|loginctl|systemd-run|pkill|killall|sudo) ]] && return 0
    # A guest path is spelled `~/…`; the steps say `$T_HOME/…` and friends.
    [[ "$cmd" == *"~/"* ]] && return 0
    [[ "$cmd" == *".local/state"* || "$cmd" == *".local/lib/caustics-bar"* ]] && return 0
    [[ "$cmd" == *".config/systemd"* || "$cmd" == *".config/caustics"* ]] && return 0
    [[ "$cmd" == *".cache/caustics"* ]] && return 0
    return 1
}

on_target() { # on_target <deadline> <command>
    local deadline="$1"; shift
    if nested_refuses "$*"; then
        fail "nested target refused a guest-only command: $(printf '%s' "$*" | head -c 160)"
        return 97
    fi
    timeout "$deadline" bash -c "$(nested_prelude) $*"
}

to_target() { # to_target <src> <dst, relative to $T_HOME>
    cp -f "$1" "$RUN/home/$2"
}

# Only a process this run owns: one of the bar's process group (a sidecar, or
# the bar), or a client or fixture it started. Anything else is a stale pid
# that may since be someone else's.
target_kill() { # target_kill <pid>
    local pid="$1" pgid bar
    bar=$(bar_pid)
    pgid=$(awk '{print $5}' "/proc/$pid/stat" 2>/dev/null)
    if [[ -n "$pgid" && "$pgid" == "$bar" && "$bar" != 0 ]] ||
        grep -qE "^(client|fixture) $pid\$" "$PIDS_FILE" 2>/dev/null; then
        kill "$pid"
    else
        note "pid $pid is not this run's (pgid ${pgid:-none}, bar $bar); not killed"
        return 1
    fi
}

niri_msg() {
    NIRI_SOCKET="$NESTED_NIRI_SOCKET" niri msg "$@"
}

# niri's own screenshot of the output, without the pointer (the guest's
# screendump has none either), turned into what the pixel helpers read: 8-bit
# RGB, filter 0 on every row, one IDAT.
shot() {
    local out="$SHOTS/$1" raw="$RUN/.shot-raw.png" i
    rm -f "$raw"
    if ! niri_msg action screenshot-screen --show-pointer false --path "$raw" >/dev/null 2>&1; then
        fail "screenshot-screen for $1"
        return
    fi
    # The PNG is written off niri's loop; wait for it to exist and stop growing.
    for i in $(seq 50); do
        [[ -s "$raw" ]] && sleep 0.05 && break
        sleep 0.05
    done
    if python3 - "$raw" "$out" <<'PY'
import struct, sys, zlib
src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
pos, raw = 8, b""
while pos + 8 <= len(data):
    length, tag = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + length]
    if tag == b"IHDR":
        w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
    elif tag == b"IDAT":
        raw += body
    pos += 12 + length
assert depth == 8 and ctype in (2, 6), (depth, ctype)
bpp = 4 if ctype == 6 else 3
img = zlib.decompress(raw)
stride = w * bpp
prev = bytearray(stride)
rows = []
p = 0
for _ in range(h):
    f = img[p]; line = bytearray(img[p + 1:p + 1 + stride]); p += 1 + stride
    for i in range(stride):
        a = line[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        if f == 1: line[i] = (line[i] + a) & 255
        elif f == 2: line[i] = (line[i] + b) & 255
        elif f == 3: line[i] = (line[i] + (a + b) // 2) & 255
        elif f == 4:
            pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
            line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
    rows.append(line); prev = line
out = bytearray()
for line in rows:
    out.append(0)
    if bpp == 3:
        out += line
    else:
        rgb = bytearray(w * 3)
        rgb[0::3] = line[0::4]; rgb[1::3] = line[1::4]; rgb[2::3] = line[2::4]
        out += rgb
def chunk(tag, body):
    return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(bytes(out), 1)) + chunk(b"IEND", b"")
open(dst, "wb").write(png)
PY
    then
        note "screenshot $out"
    else
        fail "screenshot for $1 could not be read"
    fi
}

pointer() { WAYLAND_DISPLAY="$NESTED_WAYLAND" XDG_RUNTIME_DIR="$NESTED_RT" "$POINTER" --output "$OUTPUT" "$@"; }

pointer_to() { # pointer_to <px x> <px y>
    pointer move "${1%.*}" "${2%.*}" >/dev/null || note "caustics-pointer move $1 $2 failed"
}

button() { # button <true|false> [left|right|middle|wheel-down|wheel-up]
    local name="${2:-left}" state=release
    [[ "$1" == true ]] && state=press
    case "$name" in
        wheel-down | wheel-up)
            # QMP's wheel is a button whose press is the notch; so is ours.
            [[ "$state" == press ]] && pointer wheel "${name#wheel-}" >/dev/null
            ;;
        *) pointer button "$name" "$state" >/dev/null ;;
    esac
}

# The journal: the bar's log since the cursor, a line count.
JOURNAL_CURSOR=""
journal() {
    local file="$BAR_LOG"
    [[ "$*" == *short-unix* ]] && file="$BAR_TS_LOG"
    if [[ -n "$JOURNAL_CURSOR" ]]; then
        tail -n +$((JOURNAL_CURSOR + 1)) "$file"
    else
        cat "$file"
    fi
}
mark_journal() {
    JOURNAL_CURSOR=$(wc -l <"$BAR_LOG")
}

# --- the supervisor ---------------------------------------------------------

SUP_PID=""
sup_state() { cat "$RUN/sup.state" 2>/dev/null || echo inactive; }

# Writes one line into both logs, with its arrival time in the second: the
# stand-in for journald, and the only reader of the bar's stderr.
STAMPER='
import sys, time
plain = open(sys.argv[1], "ab", buffering=0)
stamped = open(sys.argv[2], "ab", buffering=0)
for line in iter(sys.stdin.buffer.readline, b""):
    if not line.endswith(b"\n"):
        line += b"\n"
    plain.write(line)
    stamped.write(b"%.6f " % time.time() + line)
'

# systemd's own words for an exit, so the steps read the same line.
exit_words() { # exit_words <wait status>
    local rc="$1"
    if ((rc > 128)); then
        local sig
        sig=$(kill -l $((rc - 128)) 2>/dev/null)
        printf 'code=killed, status=%d/%s' $((rc - 128)) "${sig:-?}"
    else
        case "$rc" in
            0) printf 'code=exited, status=0/SUCCESS' ;;
            1) printf 'code=exited, status=1/FAILURE' ;;
            2) printf 'code=exited, status=2/INVALIDARGUMENT' ;;
            3) printf 'code=exited, status=3/NOTIMPLEMENTED' ;;
            *) printf 'code=exited, status=%d' "$rc" ;;
        esac
    fi
}

# The bar and every process in its group (its sidecars), the way a unit's
# cgroup is stopped: TERM, a grace, then KILL.
stop_group() { # stop_group <pgid>
    local g="$1" i
    kill -TERM -- "-$g" 2>/dev/null || return 0
    for i in $(seq 50); do
        kill -0 -- "-$g" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -KILL -- "-$g" 2>/dev/null
}

supervise() {
    exec 3> >(exec python3 -c "$STAMPER" "$BAR_LOG" "$BAR_TS_LOG")
    local cmd="" rc pid restarts=0 starts=() now
    echo 0 >"$RUN/sup.pid"
    echo 0 >"$RUN/sup.nrestarts"
    echo inactive >"$RUN/sup.state"
    take_cmd() { cmd=""; [[ -e "$RUN/sup.cmd" ]] && cmd=$(<"$RUN/sup.cmd") && rm -f "$RUN/sup.cmd"; }
    take_cmd
    while :; do
        case "$cmd" in
            quit) break ;;
            start | restart) ;;
            *)
                # Idle (stopped, or failed): wait to be asked.
                sleep 0.1
                take_cmd
                continue
                ;;
        esac
        # Start limit, as the unit's: ten starts in sixty seconds and it fails.
        now=$SECONDS
        starts+=("$now")
        while ((${#starts[@]} > 0 && now - starts[0] > 60)); do starts=("${starts[@]:1}"); done
        (
            # The unit's working directory is the home dir, which has no
            # `caustics/`: caustics-reload looks for a cwd-relative `caustics/`
            # *before* `$CAUSTICS_DIR`, and from the checkout's root it
            # would watch the checkout's rather than the run's copy.
            cd "$RUN/home" || exit 1
            set -a
            for f in "$RUN"/env.d/*.env; do [[ -e "$f" ]] && source "$f"; done
            set +a
            exec env -u DISPLAY \
                WAYLAND_DISPLAY="$NESTED_WAYLAND" XDG_RUNTIME_DIR="$NESTED_RT" \
                NIRI_SOCKET="$NESTED_NIRI_SOCKET" \
                XDG_CONFIG_HOME="$RUN/xdg/config" XDG_STATE_HOME="$RUN/xdg/state" \
                XDG_CACHE_HOME="$RUN/xdg/cache" CAUSTICS_DIR="$RUN/caustics" \
                "${VK_ENV[@]}" \
                RUST_LOG="${RUST_LOG_BAR:-info,caustics_bar::layout=debug,caustics_bar::shape=debug}" \
                PATH="$BAR_PATH" \
                setsid "$REPO/target/debug/caustics-bar"
        ) </dev/null >&3 2>&3 &
        pid=$!
        echo "$pid" >"$RUN/sup.pid"
        echo active >"$RUN/sup.state"
        echo "bar $pid" >>"$PIDS_FILE"
        echo "supervisor: Started caustics-bar (pid $pid)." >&3
        wait "$pid"
        rc=$?
        stop_group "$pid"
        echo 0 >"$RUN/sup.pid"
        echo "caustics-bar.service: Main process exited, $(exit_words "$rc")" >&3
        take_cmd
        case "$cmd" in
            quit) break ;;
            stop) echo inactive >"$RUN/sup.state"; cmd=""; continue ;;
            start | restart) continue ;;
        esac
        if [[ "$rc" == 2 || "$rc" == 3 ]]; then
            # RestartPreventExitStatus=2 3: final until asked again.
            echo failed >"$RUN/sup.state"
            echo "caustics-bar.service: Failed with result 'exit-code'." >&3
            continue
        fi
        if ((${#starts[@]} >= 10)); then
            echo failed >"$RUN/sup.state"
            echo "caustics-bar.service: Start request repeated too quickly." >&3
            starts=()
            continue
        fi
        echo activating >"$RUN/sup.state"
        sleep 1
        take_cmd
        [[ "$cmd" == quit ]] && break
        [[ "$cmd" == stop ]] && { echo inactive >"$RUN/sup.state"; cmd=""; continue; }
        restarts=$((restarts + 1))
        echo "$restarts" >"$RUN/sup.nrestarts"
        echo "caustics-bar.service: Scheduled restart job, restart counter is at $restarts." >&3
        cmd=restart
    done
    echo inactive >"$RUN/sup.state"
    exec 3>&-
}

sup_command() { # sup_command <restart|stop|quit>
    printf '%s\n' "$1" >"$RUN/sup.cmd.tmp" && mv -f "$RUN/sup.cmd.tmp" "$RUN/sup.cmd"
}

sup_ensure() {
    if [[ -z "$SUP_PID" ]] || ! kill -0 "$SUP_PID" 2>/dev/null; then
        supervise &
        SUP_PID=$!
        track supervisor "$SUP_PID"
    fi
}

bar_env() { # bar_env <group> KEY=VALUE…
    local group="$1" kv
    shift
    : >"$RUN/env.d/$group.env"
    for kv in "$@"; do
        printf '%s=%q\n' "${kv%%=*}" "${kv#*=}" >>"$RUN/env.d/$group.env"
    done
}

bar_env_clear() { rm -f "$RUN/env.d/$1.env"; }

bar_restart() {
    local old i pid line
    sup_ensure
    old=$(cat "$RUN/sup.pid" 2>/dev/null || echo 0)
    sup_command restart
    [[ "$old" != 0 ]] && kill -TERM "$old" 2>/dev/null
    for i in $(seq 150); do
        pid=$(cat "$RUN/sup.pid" 2>/dev/null || echo 0)
        [[ "$pid" != 0 && "$pid" != "$old" ]] && break
        sleep 0.1
    done
    if [[ "$pid" == 0 || "$pid" == "$old" ]]; then
        note "the supervisor did not start a bar within 15 s"
        return 0
    fi
    # The old bar writes until it dies — a last layout, when a sidecar
    # republishes in the gap — and those lines land after a mark the caller
    # took before calling here. A caller's journal is the new instance's
    # from its first line: the supervisor's own, which the stamper may still
    # be writing when `sup.pid` is read.
    [[ -n "$JOURNAL_CURSOR" ]] || return 0
    for i in $(seq 50); do
        line=$(grep -n -F "supervisor: Started caustics-bar (pid $pid)." "$BAR_LOG" |
            tail -1 | cut -d: -f1)
        if [[ -n "$line" ]]; then
            ((line > JOURNAL_CURSOR + 1)) && JOURNAL_CURSOR=$((line - 1))
            return 0
        fi
        sleep 0.1
    done
    note "no 'supervisor: Started' line for pid $pid within 5 s; the journal keeps the caller's mark"
}

bar_stop() {
    local pid i
    pid=$(cat "$RUN/sup.pid" 2>/dev/null || echo 0)
    sup_command stop
    [[ "$pid" != 0 ]] && kill -TERM "$pid" 2>/dev/null
    for i in $(seq 100); do
        [[ "$(sup_state)" == inactive ]] && return 0
        sleep 0.1
    done
}

bar_active() {
    local pid
    pid=$(cat "$RUN/sup.pid" 2>/dev/null || echo 0)
    if [[ "$(sup_state)" == active && "$pid" != 0 ]] && kill -0 "$pid" 2>/dev/null; then
        echo active
    else
        sup_state
    fi
}
bar_restarts() { cat "$RUN/sup.nrestarts" 2>/dev/null || echo 0; }
bar_pid() { cat "$RUN/sup.pid" 2>/dev/null || echo 0; }

declare -A FIXTURES=()
fixture_start() { # fixture_start <name> <command>
    setsid bash -c "$(nested_prelude) exec $2" >"$RUN/fixture-$1.log" 2>&1 </dev/null &
    FIXTURES[$1]=$!
    track fixture "$!"
}
fixture_stop() {
    local pid="${FIXTURES[$1]:-}"
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
    unset "FIXTURES[$1]"
    return 0
}
fixture_log() { tail -3 "$RUN/fixture-$1.log" 2>/dev/null; }

# A client is a window the steps look at, and nothing more. It gets no
# session bus (a terminal's notifications and the portals it would activate
# would land in caustics-notify's history, which the steps count), and its shell
# is a plain `sh` with its history in the scratch tree, not the developer's
# login shell reading their rc files and writing their history.
CLIENTS=()
client_spawn() { # client_spawn <command>
    setsid bash -c "$(nested_prelude) cd \"\$T_HOME\";
        export DBUS_SESSION_BUS_ADDRESS=unix:path=$RUN/no-bus SHELL=/bin/sh HISTFILE=\$T_HOME/.sh_history ENV=
        exec $1" >>"$RUN/clients.log" 2>&1 </dev/null &
    CLIENTS+=("$!")
    track client "$!"
}

# A nested output is never parked and never locked.
wake_output() { :; }
before_bar_start() { sup_ensure; }

# Clients, fixtures, then the bar with its supervisor told first so it does
# not restart what it is about to lose (bar/nested-run.sh stops niri after).
target_cleanup() {
    local pid
    for pid in ${CLIENTS[@]+"${CLIENTS[@]}"}; do kill "$pid" 2>/dev/null; done
    for pid in ${FIXTURES[@]+"${FIXTURES[@]}"}; do kill "$pid" 2>/dev/null; done
    if [[ -n "$SUP_PID" ]] && kill -0 "$SUP_PID" 2>/dev/null; then
        pid=$(bar_pid)
        sup_command quit
        [[ "$pid" != 0 ]] && kill -TERM "$pid" 2>/dev/null
        wait "$SUP_PID" 2>/dev/null
    fi
    note "clients, fixtures and the bar stopped; the scratch tree is $RUN"
}
target_cleanup_last() { :; }
