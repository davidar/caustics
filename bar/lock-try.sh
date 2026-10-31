#!/usr/bin/env bash
# The canvas's lock, tried by hand without locking the session: a nested niri
# as a window on the desktop, the canvas inside it with `caustics-lock` in
# its set, locked on request, so the developer types the real password (or
# touches the real fingerprint reader) into the window. Whatever misbehaves,
# closing the window ends it, and nothing in the developer's own session is
# locked. The by-eye companion of the nested gate (docs/testing.md).
#
#   bar/lock-try.sh [picture]                  # the real PAM and the real reader
#   bar/lock-try.sh --dry-run <secret>         # no PAM: <secret> unlocks
#   bar/lock-try.sh --fingerprint off          # the password alone
#   bar/lock-try.sh --pam-service <name>       # /etc/pam.d/<name>, not the one found
#   bar/lock-try.sh --headless --dry-run <s>   # no window, for checking the script
#
# 1. Builds the workspace (`target/debug/`, beside this checkout), then starts
#    a nested niri from bar/harness/niri.kdl the way
#    `CAUSTICS_NESTED_HOST=desktop bar/nested-run.sh` does
#    (bar/harness/nested-niri.sh): a floating window of 1280x800 px, focus
#    handed back to the terminal. `--headless` puts it on the gate's headless
#    sway instead.
# 2. Under a **private `$XDG_RUNTIME_DIR`**, so the canvas's, the wallpaper's
#    and the lock's sockets are this run's, never the live set's, and private
#    XDG config, state and cache dirs under the run directory. The buses are
#    the **real** ones: the system bus for logind, fprintd and PAM's helpers,
#    and the session bus (nothing in this set speaks on it). The lock's
#    session is the user's graphical one, logind's `Display`, given to the
#    sidecar as `$XDG_SESSION_ID`. `--headless` points
#    `$DBUS_SYSTEM_BUS_ADDRESS` at a socket that does not exist, so a check
#    never reaches the real logind: no hint, no signals, no reader.
# 3. Starts `caustics-canvas` from the build, from this checkout (so its
#    `caustics/` hot-reloads, the lock's caustic included) with
#    `CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs
#    caustics-lock"`; shows the picture given, else a generated gradient;
#    waits for the host's `show … into slot` line; then
#    `caustics-canvasctl lock --wait` against the run's runtime dir.
# 4. Streams the witness lines as they come: the host's `lock:`, `prompt:`
#    and `seat:` lines, its warnings, and every `caustics-lock:` line, the
#    `fingerprint:` ones among them; nothing else (the whole log is
#    `canvas.log` in the run directory, which holds no secret). Each unlock
#    ends with one verdict line, `unlocked by password after N attempts` (N
#    counts the one that worked) or `unlocked by fingerprint`. Then, on the
#    terminal, `l` and Return locks again and `q` and Return quits.
#    Ctrl-C, or closing the window, ends it at any time.
# 5. On every exit path, by pid and in order: a `caustics-canvasctl` still
#    waiting, the canvas host and its sidecars (its process group), the
#    stream, the nested niri, the host compositor. Never by name: the
#    developer's own terminal and niri share these binaries. Then the
#    private runtime dir is removed.
#
# Two side effects reach the developer's real session, because the sidecar
# is the real session's locker while it runs:
#   - while the nested session is locked, the sidecar sets the real
#     session's `LockedHint` (a hint only: nothing is locked by it) and
#     clears it on unlock. A run that ends while locked leaves it set, as
#     the sidecar does by design (a restarted host re-locks); the teardown
#     clears a hint this run set and did not clear.
#   - a real `loginctl lock-session` (or the idle timer, or the lid) locks
#     the window too, since the sidecar hears the real session's `Lock`
#     signal; the session's own locker locks the session as ever.
# And a `LockedHint` already set when the run starts locks the window at
# once, as it would a restarted canvas.

set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NESTED_WHO=lock-try
# shellcheck source=bar/harness/nested-niri.sh
source "$REPO/bar/harness/nested-niri.sh"

usage() {
    sed -n '9,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,3\}//'
}

HOST_KIND=desktop
DRY_SECRET=""
FINGERPRINT=on
PAM_SERVICE=""
PICTURE=""
while (($#)); do
    case "$1" in
        --headless) HOST_KIND=headless ;;
        --dry-run)
            [[ -n "${2:-}" ]] || { echo "lock-try: --dry-run needs the secret that unlocks"; exit 2; }
            DRY_SECRET="$2"; shift ;;
        --fingerprint)
            case "${2:-}" in
                on | off) FINGERPRINT="$2" ;;
                *) echo "lock-try: --fingerprint is on or off"; exit 2 ;;
            esac
            shift ;;
        --pam-service)
            [[ -n "${2:-}" ]] || { echo "lock-try: --pam-service needs a name"; exit 2; }
            PAM_SERVICE="$2"; shift ;;
        -h | --help) usage; exit 0 ;;
        -*) echo "lock-try: no option '$1'"; usage; exit 2 ;;
        *)
            [[ -z "$PICTURE" ]] || { echo "lock-try: one picture, not '$PICTURE' and '$1'"; exit 2; }
            PICTURE="$1" ;;
    esac
    shift
done
if [[ -n "$PICTURE" ]]; then
    [[ -f "$PICTURE" ]] || { echo "lock-try: no picture at $PICTURE"; exit 2; }
    PICTURE="$(cd "$(dirname "$PICTURE")" && pwd)/$(basename "$PICTURE")"
fi
# The sidecar's own choice, mirrored (lock/README.md, *The PAM service*):
# the first of these with a file is taken; a name with no file would fall to
# PAM's 'other' service, which usually denies, so the check here is only for
# a name given by hand.
if [[ -n "$PAM_SERVICE" ]]; then
    if [[ -z "$DRY_SECRET" && ! -e "/etc/pam.d/$PAM_SERVICE" && ! -e "/usr/lib/pam.d/$PAM_SERVICE" ]]; then
        echo "lock-try: there is no /etc/pam.d/$PAM_SERVICE; PAM would use its 'other' service, which usually denies"
        echo "lock-try: name one that exists with --pam-service, or take a look with --dry-run <secret>"
        exit 2
    fi
elif [[ -z "$DRY_SECRET" ]]; then
    found=""
    for candidate in caustics-lock swaylock hyprlock gtklock waylock login; do
        for dir in /etc/pam.d /usr/lib/pam.d; do
            [[ -f "$dir/$candidate" ]] && { found="$dir/$candidate"; break 2; }
        done
    done
    [[ -n "$found" ]] || echo "lock-try: no PAM service file found for the lock; it will use 'login' unseen (lock/README.md, *The PAM service*)"
fi

RENDER_NODE="${CAUSTICS_NESTED_RENDER_NODE:-$(first_render_node)}"
[[ -n "$RENDER_NODE" ]] || { echo "lock-try: no /dev/dri/renderD* node; the canvas needs a GPU"; exit 2; }
# The canvas's Vulkan driver, pinned to the host's GPU as the nested target
# pins the bar's.
VK_ICD="${CAUSTICS_NESTED_VK_ICD:-}"
[[ -n "$VK_ICD" ]] || read -r VK_ICD _ <<<"$(nested_icd "$RENDER_NODE")"
VK_ENV=()
[[ -n "$VK_ICD" ]] && VK_ENV=(VK_DRIVER_FILES="$VK_ICD")

# The real session, or none at all.
SESSION=""
if [[ "$HOST_KIND" == desktop ]]; then
    SESSION=$(loginctl show-user "$(id -un)" -p Display --value 2>/dev/null)
    if [[ -z "$SESSION" ]]; then
        SESSION="${XDG_SESSION_ID:-}"
        echo "lock-try: logind names no graphical session for $(id -un); the sidecar is given ${SESSION:-none (it looks by pid)}"
    elif [[ -n "${XDG_SESSION_ID:-}" && "$XDG_SESSION_ID" != "$SESSION" ]]; then
        echo "lock-try: this terminal is in session $XDG_SESSION_ID; the lock is given the graphical one, $SESSION"
    fi
    if [[ -n "$SESSION" && "$(loginctl show-session "$SESSION" -p LockedHint --value 2>/dev/null)" == yes ]]; then
        echo "lock-try: session $SESSION's LockedHint is already set, so the window locks as soon as the lock starts"
    fi
fi

stamp=$(date +%Y%m%d-%H%M%S)-$$
RUN="$REPO/bar/test-runs/lock-try-$stamp"
mkdir -p "$RUN/home" "$RUN/xdg/config" "$RUN/xdg/state" "$RUN/xdg/cache"
CANVAS_LOG="$RUN/canvas.log"
: >"$CANVAS_LOG"
echo "lock-try: $HOST_KIND host; run directory $RUN"

t0=$SECONDS
if ! (cd "$REPO" && cargo build --offline --workspace) >"$RUN/build.log" 2>&1; then
    echo "lock-try: the build failed; $RUN/build.log"
    tail -20 "$RUN/build.log"
    exit 1
fi
echo "lock-try: built in $((SECONDS - t0)) s"
BIN="$REPO/target/debug"
if [[ "$HOST_KIND" == headless ]]; then
    nested_toolbox_ready "$RUN" || exit 9
fi

RT="$XDG_RUNTIME_DIR/caustics-locktry.$$"
mkdir -m 700 "$RT"
CANVAS_PID=""
CTL_PID=""
TAIL_PID=""

# The hint this run's sidecar left set, if it did: its last word on it.
hint_left_set() {
    [[ "$(sed -n 's/^caustics-lock: LockedHint of session .* is \(true\|false\)$/\1/p' "$CANVAS_LOG" | tail -1)" == true ]]
}

teardown() {
    trap - EXIT INT TERM
    stop_pid "$CTL_PID" "caustics-canvasctl"
    # The host first: its sidecars exit when their stdin closes. Its group
    # (it is a session leader, by setsid) is whatever of them outlived it.
    stop_pid "$CANVAS_PID" "the canvas"
    if [[ -n "$CANVAS_PID" ]] && kill -0 -- "-$CANVAS_PID" 2>/dev/null; then
        echo "lock-try: the canvas's sidecars outlived it; TERM"
        kill -TERM -- "-$CANVAS_PID" 2>/dev/null
        sleep 1
        kill -KILL -- "-$CANVAS_PID" 2>/dev/null
    fi
    stop_pid "$TAIL_PID" "the witness stream"
    nested_down
    if [[ "$HOST_KIND" == desktop && -n "$SESSION" ]] && hint_left_set; then
        path=$(busctl --system call org.freedesktop.login1 /org/freedesktop/login1 \
            org.freedesktop.login1.Manager GetSession s "$SESSION" 2>/dev/null | sed -n 's/^o "\(.*\)"$/\1/p')
        if [[ -n "$path" ]] && busctl --system call org.freedesktop.login1 "$path" \
            org.freedesktop.login1.Session SetLockedHint b false 2>/dev/null; then
            echo "lock-try: the run ended locked; session $SESSION's LockedHint cleared"
        else
            echo "lock-try: the run ended locked and session $SESSION's LockedHint is still set; clear it with"
            echo "  busctl --system call org.freedesktop.login1 ${path:-/org/freedesktop/login1/session/auto} org.freedesktop.login1.Session SetLockedHint b false"
        fi
    fi
    rm -rf "$RT"
    echo "lock-try: torn down (canvas ${CANVAS_PID:-none}, niri ${NIRI_PID:-none}); the log is $CANVAS_LOG"
}
trap teardown EXIT
trap 'exit 130' INT TERM

nested_host_up "$HOST_KIND" "$RUN" "$RT" "$RENDER_NODE" || exit 9
nested_niri_up "$HOST_KIND" "$RUN" "$RT" || exit 9
echo "nested output: $NESTED_MODE"

# The picture: the one given, else a gradient of the gate's kind (the
# sidecar fits it to the output).
if [[ -z "$PICTURE" ]]; then
    PICTURE="$RUN/gradient.png"
    python3 - "$PICTURE" <<'PY'
import struct, sys, zlib
w, h = 320, 200
rows = []
for y in range(h):
    row = bytearray(b"\x00")
    for x in range(w):
        t, u = x / (w - 1), y / (h - 1)
        row += bytes((int(20 + 40 * t), int(60 + 90 * u), int(120 + 110 * (1 - u))))
    rows.append(bytes(row))
def chunk(tag, body):
    return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body) & 0xffffffff)
open(sys.argv[1], "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(b"".join(rows))) + chunk(b"IEND", b""))
PY
fi

# The canvas, its lock and their settings. From this checkout, so
# caustics-reload watches its `caustics/`.
LOCK_ENV=(CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs caustics-lock")
[[ -n "$DRY_SECRET" ]] && LOCK_ENV+=(CAUSTICS_LOCK_DRY_RUN_SECRET="$DRY_SECRET")
[[ "$FINGERPRINT" == off ]] && LOCK_ENV+=(CAUSTICS_LOCK_FINGERPRINT=0)
[[ -n "$PAM_SERVICE" ]] && LOCK_ENV+=(CAUSTICS_LOCK_PAM_SERVICE="$PAM_SERVICE")
if [[ "$HOST_KIND" == headless ]]; then
    LOCK_ENV+=(DBUS_SYSTEM_BUS_ADDRESS="unix:path=$RUN/no-system-bus")
elif [[ -n "$SESSION" ]]; then
    LOCK_ENV+=(XDG_SESSION_ID="$SESSION")
fi
(
    cd "$REPO" || exit 1
    exec env -u DISPLAY -u NIRI_SOCKET -u XDG_SESSION_ID \
        WAYLAND_DISPLAY="$NESTED_WAYLAND" XDG_RUNTIME_DIR="$RT" \
        XDG_CONFIG_HOME="$RUN/xdg/config" XDG_STATE_HOME="$RUN/xdg/state" \
        XDG_CACHE_HOME="$RUN/xdg/cache" CAUSTICS_DIR="$REPO/caustics" \
        "${VK_ENV[@]}" "${LOCK_ENV[@]}" RUST_LOG=info \
        setsid "$BIN/caustics-canvas"
) </dev/null >>"$CANVAS_LOG" 2>&1 &
CANVAS_PID=$!

canvasctl() { XDG_RUNTIME_DIR="$RT" "$BIN/caustics-canvasctl" "$@"; }

KEYS_HELP="on this terminal: l and Return locks again, q and Return quits (or Ctrl-C, or close the window)"
if [[ "$HOST_KIND" == desktop ]]; then
    LOCKED_HELP="the window is locked. Click into it, then type your password and press Return, or touch the reader; Escape starts a fresh attempt. Close the window or press Ctrl-C here to end."
else
    kb="WAYLAND_DISPLAY=$NESTED_WAYLAND XDG_RUNTIME_DIR=$RT $BIN/caustics-keyboard"
    LOCKED_HELP="the nested niri is locked. Type into it with: $kb type '<secret>' && $kb key Return"
fi
[[ "$FINGERPRINT" == off ]] && LOCKED_HELP="${LOCKED_HELP/, or touch the reader/}"

# The witness stream: the lines worth seeing, each with the time it arrived,
# and a verdict at each unlock.
witness_filter() {
    local line msg level failed=0 how=""
    local host_re='^[0-9T:.Z-]+ +(INFO|WARN|ERROR) +[A-Za-z0-9_:]+: (.*)$'
    while IFS= read -r line; do
        if [[ "$line" == "caustics-lock: "* ]]; then
            msg="${line#caustics-lock: }"
            printf '%(%T)T  %s\n' -1 "$line"
            case "$msg" in
                locked)
                    failed=0 how=""
                    printf '\n  >>> %s\n\n' "$LOCKED_HELP"
                    ;;
                "conversation "*": failed ("*)
                    failed="${msg##*failed (}" failed="${failed%% *}"
                    ;;
                "conversation "*": success after "*)
                    how=password failed="${msg##*success after }" failed="${failed%% *}"
                    ;;
                "fingerprint matched; unlocking") how=fingerprint ;;
                unlocked)
                    if [[ "$how" == fingerprint ]]; then
                        verdict="unlocked by fingerprint"
                        ((failed > 0)) && verdict+=" (after $failed failed password attempts)"
                    else
                        n=$((failed + 1))
                        verdict="unlocked by password after $n attempt$( ((n == 1)) || echo s)"
                    fi
                    [[ -n "$DRY_SECRET" ]] && verdict+=" (dry run)"
                    printf '\n  >>> %s\n  >>> %s\n\n' "$verdict" "$KEYS_HELP"
                    ;;
            esac
        elif [[ "$line" =~ $host_re ]]; then
            level="${BASH_REMATCH[1]}" msg="${BASH_REMATCH[2]}"
            case "$msg" in
                "lock: "* | "prompt: "* | "seat: "* | "sidecar caustics-lock: "*) ;;
                *) [[ "$level" != INFO ]] || continue ;;
            esac
            if [[ "$level" == INFO ]]; then level=""; else level+=" "; fi
            printf '%(%T)T  canvas: %s%s\n' -1 "$level" "$msg"
        fi
    done
}
tail -n +1 -F --pid="$CANVAS_PID" "$CANVAS_LOG" 2>/dev/null > >(witness_filter) &
TAIL_PID=$!

# wait_log <fixed string> <seconds>: 0 once the canvas's log has the line.
wait_log() {
    local deadline=$((SECONDS + $2))
    until grep -qF -- "$1" "$CANVAS_LOG"; do
        alive "$CANVAS_PID" || { echo "lock-try: the canvas exited; the end of its log:"; tail -15 "$CANVAS_LOG"; return 1; }
        ((SECONDS < deadline)) || { echo "lock-try: no '$1' from the canvas within $2 s"; return 1; }
        sleep 0.2
    done
}

wait_log "caustics-lock: listening on" 120 || exit 1
canvasctl show "$PICTURE" >/dev/null || { echo "lock-try: caustics-canvasctl show $PICTURE failed"; exit 1; }
wait_log "into slot" 60 || exit 1

lock_now() {
    XDG_RUNTIME_DIR="$RT" timeout 40 "$BIN/caustics-canvasctl" lock --wait >"$RUN/lock-wait.out" 2>&1 &
    CTL_PID=$!
    wait "$CTL_PID"
    local rc=$?
    CTL_PID=""
    if ((rc != 0)); then
        echo "lock-try: caustics-canvasctl lock --wait said: $(tr '\n' ' ' <"$RUN/lock-wait.out") (exit $rc)"
    fi
}
lock_now

# The terminal's keys, until q, Ctrl-C, or the window goes.
stdin_open=1
while :; do
    if ! alive "$NIRI_PID"; then
        echo "lock-try: the nested niri is gone (its window was closed)"
        break
    fi
    if ! alive "$CANVAS_PID"; then
        echo "lock-try: the canvas exited; the end of its log:"
        tail -10 "$CANVAS_LOG"
        break
    fi
    if ((stdin_open)); then
        if read -r -t 1 cmd; then
            case "$cmd" in
                l | L) lock_now ;;
                q | Q) break ;;
                "") ;;
                *) echo "lock-try: $KEYS_HELP" ;;
            esac
        elif (($? <= 128)); then
            stdin_open=0
        fi
    else
        sleep 1
    fi
done
exit 0
