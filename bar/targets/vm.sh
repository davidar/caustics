# shellcheck shell=bash
# The VM target of bar/bar-test.sh (docs/testing.md, *The steps and their
# targets*): a topaz-os image as a test guest, booted by that project's test
# harness (`$TOPAZ_TESTS/run.sh`; docs/testing.md, *The VM target*), reached over
# ssh, driven with QMP and screendumped through VNC. Sourced by bar-test.sh
# when `CAUSTICS_TARGET=vm`, which `bar/vm-run.sh` sets.
#
# The transport surface — the only names the steps use to reach where the bar
# runs — is the same in every target file:
#
#   on_target <deadline> <cmd>   a shell command where the bar runs, with
#                                `$T_HOME $T_CONFIG $T_STATE $T_CACHE $T_BIN
#                                $T_CAUSTICS $T_ORACLE
#                                $T_FAKE_UPOWER` and `bus_env gui_env vk_env`
#                                defined in it (the target's prelude)
#   to_target <src> <dst>        a file there (dst relative to $T_HOME)
#   target_kill <pid>            a process the run started, by pid
#   niri_msg …                   the compositor under test's IPC
#   shot <name>                  a 1280x800 8-bit RGB PNG (filter 0) in $SHOTS
#   pointer_to x y, button <true|false> [left|right|middle|wheel-down|wheel-up]
#   keys <text>, key <[mod+…]keysym> [press|release|tap]
#                                typed into whatever has keyboard focus
#   journal [-o short-unix], mark_journal ($JOURNAL_CURSOR)
#   bar_env <group> KEY=VALUE…, bar_env_clear <group>, bar_restart,
#   bar_stop, bar_active, bar_restarts, bar_pid
#   fixture_start <name> <cmd>, fixture_stop <name>, fixture_log <name>
#   client_spawn <cmd>           a client of the compositor under test
#   wake_output                  the output awake and unlocked for the run
#   target_cleanup               the target as the run found it
#
# and the values `OUTPUT SCREEN_W SCREEN_H SHOTS T_NAME T_ADAPTER U_HOME
# U_SESSION_BUS`: the output's name and size, where screenshots go, the
# adapter the bar's `gpu:` line must name, and how the bar's environment
# spells its home and the session bus (`%h`, `%t` in a unit drop-in).

TOPAZ_TESTS="${TOPAZ_TESTS:?set TOPAZ_TESTS to the tests directory of the VM harness (docs/testing.md, *The VM target*)}"
# shellcheck disable=SC1091
source "$TOPAZ_TESTS/lib.sh"

# The toolchain the copy step puts in the guest beside the bar: lavapipe, from
# the host (`$CAUSTICS_VM_LVP`), with a hand-written manifest. No compiler:
# `caustics-reload` carries Slang inside its binary.
LVP_HOST="${CAUSTICS_VM_LVP:-/usr/lib64/libvulkan_lvp.so}"
# The lavapipe build's hash, as one line. An image that ships lavapipe records
# the same line (after any older fields, `; `-separated), and the copy step
# compares the two.
vm_toolchain_version() {
    printf 'lvp %s\n' "$(sha256sum "$LVP_HOST" | cut -c1-16)"
}

T_NAME=vm
# Guest N of the harness (`TOPAZ_TEST_ID`, set by `bar/vm-run.sh`) keeps its screenshots and journals apart from guest 0's, so
# two runs from one checkout never write the same file.
TOPAZ_TEST_ID="${TOPAZ_TEST_ID:-0}"
if [[ "$TOPAZ_TEST_ID" == 0 ]]; then
    SHOTS="$REPO/bar/test-runs"
else
    SHOTS="$REPO/bar/test-runs/vm$TOPAZ_TEST_ID"
fi

# The guest output. 1280x800 px; the run puts it at scale 2, so 640x400 logical.
OUTPUT=Virtual-1
SCREEN_W=1280
SCREEN_H=800
# The guest renders the bar with lavapipe (the copy step puts it there).
T_ADAPTER=llvmpipe
# The unit drop-in's specifiers: the guest's home, and its runtime dir's bus.
U_HOME='%h'
U_SESSION_BUS='unix:path=%t/bus'

# Where things are in the guest, defined at the head of every remote command.
# `$HOME` is the *guest's*, expanded by the remote shell. The three functions
# set up the session bus, the graphical session, and lavapipe.
T_PRELUDE='T_HOME=$HOME; T_CONFIG=$HOME/.config; T_STATE=$HOME/.local/state; T_CACHE=$HOME/.cache;
T_BIN=$HOME/.local/bin; T_CAUSTICS=$HOME/caustics-bar/caustics;
T_ORACLE=$HOME/caustics-bar-oracle;
T_FAKE_UPOWER=$HOME/caustics-bar/caustics-fake-upower;
bus_env() { export XDG_RUNTIME_DIR=/run/user/$(id -u); export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus; };
gui_env() { export WAYLAND_DISPLAY=$(systemctl --user show-environment | sed -n "s/^WAYLAND_DISPLAY=//p"); export XDG_RUNTIME_DIR=/run/user/$(id -u); };
vk_env() { export VK_DRIVER_FILES=$HOME/lvp_icd.json VK_LOADER_LAYERS_DISABLE="~all~"; };
'

# `tssh` is a shell function, so `timeout tssh ...` cannot work; this is the
# same ssh invocation with a hard outer deadline, which is what an unattended
# run needs when the guest wedges.
gssh() {
    local deadline="$1"; shift
    timeout "$deadline" ssh -q -p "$SSH_PORT" -i "$SSH_KEY" \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o ConnectTimeout=5 -o LogLevel=ERROR tester@127.0.0.1 "$@"
}

gscp() {
    timeout 300 scp -q -P "$SSH_PORT" -i "$SSH_KEY" \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o ConnectTimeout=5 -o LogLevel=ERROR "$@"
}

on_target() { # on_target <deadline> <command>
    local deadline="$1"; shift
    gssh "$deadline" "$T_PRELUDE $*"
}

to_target() { # to_target <src> <dst, relative to the guest's home>
    gscp "$1" "tester@127.0.0.1:$2"
}

target_kill() { # target_kill <pid>
    gssh 20 "kill $1"
}

# niri's control socket is in the session's environment, not ours.
niri_msg() {
    gssh 30 "export NIRI_SOCKET=\$(systemctl --user show-environment | sed -n 's/^NIRI_SOCKET=//p'); niri msg $*"
}

shot() {
    local out="$SHOTS/$1"
    # `$VNC_SOCK` is the guest's own when the harness runs guests side by
    # side; a harness that runs one guest has only the one socket under `$ART`.
    if python3 "$TOPAZ_TESTS/screendump.py" "${VNC_SOCK:-$ART/vnc.sock}" "$out" >/dev/null 2>&1; then
        note "screenshot $out"
    else
        fail "screendump for $1"
    fi
}

# QEMU's absolute pointer axis is 0..32767 across the screen.
abs_x() { python3 -c "print(round($1 * 32767 / $SCREEN_W))"; }
abs_y() { python3 -c "print(round($1 * 32767 / $SCREEN_H))"; }

# Two batches, the first one pixel to the side: a move that changes y alone
# never reached the guest (`park_pointer` from the clock's centre to the same
# x left the pointer on the clock, and the strip that landed under it opened a
# preview a later step then found open), while every move that changed both
# axes did. The nudge makes every move change both.
pointer_to() { # pointer_to <px x> <px y>
    local nudge
    nudge=$(python3 -c "print($1 - 1 if $1 > 0 else $1 + 1)")
    qmp input-send-event "{\"events\":[
        {\"type\":\"abs\",\"data\":{\"axis\":\"x\",\"value\":$(abs_x "$nudge")}},
        {\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":$(abs_y "$2")}}]}" >/dev/null
    qmp input-send-event "{\"events\":[
        {\"type\":\"abs\",\"data\":{\"axis\":\"x\",\"value\":$(abs_x "$1")}},
        {\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":$(abs_y "$2")}}]}" >/dev/null
}

button() { # button <true|false> [left|right|middle|wheel-down|wheel-up]
    qmp input-send-event \
        "{\"events\":[{\"type\":\"btn\",\"data\":{\"down\":$1,\"button\":\"${2:-left}\"}}]}" >/dev/null
}

# --- the keyboard ------------------------------------------------------------
#
# QMP names physical keys (`qcode`s), and the guest's own keymap turns them
# into characters, so what comes out is the US layout's only while the guest's
# layout is US (the image's default). The characters `keys` can type are the
# ones `caustics-keyboard` can on the nested target: printable ASCII, newline
# (Return) and tab. Anything else is refused before a key is sent. A typed
# character is one `send-key` (its keys down in order, then up in reverse),
# queued by QEMU behind the one before it, so a string arrives in order.

# The qcodes that type one character, Shift first when it takes Shift.
vm_char_qcodes() { # vm_char_qcodes <char> -> "shift a", "spc", or nothing
    local c="$1"
    case "$c" in
        [abcdefghijklmnopqrstuvwxyz0123456789]) printf '%s' "$c" ;;
        [ABCDEFGHIJKLMNOPQRSTUVWXYZ]) printf 'shift %s' "${c,,}" ;;
        ' ') printf spc ;;
        $'\n') printf ret ;;
        $'\t') printf tab ;;
        '-') printf minus ;; '_') printf 'shift minus' ;;
        '=') printf equal ;; '+') printf 'shift equal' ;;
        '[') printf bracket_left ;; '{') printf 'shift bracket_left' ;;
        ']') printf bracket_right ;; '}') printf 'shift bracket_right' ;;
        '\') printf backslash ;; '|') printf 'shift backslash' ;;
        ';') printf semicolon ;; ':') printf 'shift semicolon' ;;
        "'") printf apostrophe ;; '"') printf 'shift apostrophe' ;;
        '`') printf grave_accent ;; '~') printf 'shift grave_accent' ;;
        ',') printf comma ;; '<') printf 'shift comma' ;;
        '.') printf dot ;; '>') printf 'shift dot' ;;
        '/') printf slash ;; '?') printf 'shift slash' ;;
        '!') printf 'shift 1' ;; '@') printf 'shift 2' ;; '#') printf 'shift 3' ;;
        '$') printf 'shift 4' ;; '%') printf 'shift 5' ;; '^') printf 'shift 6' ;;
        '&') printf 'shift 7' ;; '*') printf 'shift 8' ;; '(') printf 'shift 9' ;;
        ')') printf 'shift 0' ;;
    esac
}

# The qcodes for a keysym name with optional modifiers, the names
# `caustics-keyboard key` takes: `Return`, `A`, `slash`, `ctrl+u`.
vm_key_qcodes() { # vm_key_qcodes <[mod+…]keysym> -> "ctrl u", or nothing
    local spec="$1" name mods="" m q
    name="${spec##*+}"
    if [[ -z "$name" ]]; then
        name=plus
        spec="${spec%+}"
    fi
    [[ "$spec" == *+* ]] && mods="${spec%+*}"
    case "$name" in
        Return) q=ret ;; Escape) q=esc ;; BackSpace) q=backspace ;; Tab) q=tab ;;
        Delete) q=delete ;; Insert) q=insert ;; Home) q=home ;; End) q=end ;;
        Prior) q=pgup ;; Next) q=pgdn ;; Up) q=up ;; Down) q=down ;;
        Left) q=left ;; Right) q=right ;; space) q=spc ;;
        F[1-9] | F1[0-2]) q="${name,,}" ;;
        Shift_L) q=shift ;; Control_L) q=ctrl ;; Alt_L) q=alt ;; Super_L) q=meta_l ;;
        grave) q=grave_accent ;; asciitilde) q='shift grave_accent' ;;
        exclam) q='shift 1' ;; at) q='shift 2' ;; numbersign) q='shift 3' ;;
        dollar) q='shift 4' ;; percent) q='shift 5' ;; asciicircum) q='shift 6' ;;
        ampersand) q='shift 7' ;; asterisk) q='shift 8' ;; parenleft) q='shift 9' ;;
        parenright) q='shift 0' ;; minus) q=minus ;; underscore) q='shift minus' ;;
        equal) q=equal ;; plus) q='shift equal' ;;
        bracketleft) q=bracket_left ;; braceleft) q='shift bracket_left' ;;
        bracketright) q=bracket_right ;; braceright) q='shift bracket_right' ;;
        backslash) q=backslash ;; bar) q='shift backslash' ;;
        semicolon) q=semicolon ;; colon) q='shift semicolon' ;;
        apostrophe) q=apostrophe ;; quotedbl) q='shift apostrophe' ;;
        comma) q=comma ;; less) q='shift comma' ;; period) q=dot ;;
        greater) q='shift dot' ;; slash) q=slash ;; question) q='shift slash' ;;
        ?) q=$(vm_char_qcodes "$name") ;;
        *) q="" ;;
    esac
    [[ -n "$q" ]] || return 1
    local out=()
    if [[ -n "$mods" ]]; then
        IFS=+ read -ra m <<<"$mods"
        for m in "${m[@]}"; do
            case "${m,,}" in
                shift) out+=(shift) ;; ctrl | control) out+=(ctrl) ;;
                alt) out+=(alt) ;; super | logo) out+=(meta_l) ;;
                *) return 1 ;;
            esac
        done
    fi
    # Shift once, first, whether the name or the chord asked for it.
    if [[ "$q" == "shift "* ]]; then
        q="${q#shift }"
        [[ " ${out[*]} " == *" shift "* ]] || out=(shift ${out[@]+"${out[@]}"})
    fi
    printf '%s' "${out[*]} $q" | sed 's/^ //'
}

vm_send_key() { # vm_send_key <qcode…>: down in order, up in reverse
    local q json=""
    for q in "$@"; do json+="${json:+,}{\"type\":\"qcode\",\"data\":\"$q\"}"; done
    qmp send-key "{\"keys\":[$json],\"hold-time\":40}" >/dev/null
}

vm_key_events() { # vm_key_events <true|false> <qcode…>
    local down="$1" q json=""
    shift
    for q in "$@"; do
        json+="${json:+,}{\"type\":\"key\",\"data\":{\"down\":$down,\"key\":{\"type\":\"qcode\",\"data\":\"$q\"}}}"
    done
    qmp input-send-event "{\"events\":[$json]}" >/dev/null
}

keys() { # keys <text>: typed as is; a newline in it is Return
    local text="$1" i c q missing="" strokes=()
    for ((i = 0; i < ${#text}; i++)); do
        c="${text:i:1}"
        q=$(vm_char_qcodes "$c")
        if [[ -z "$q" ]]; then
            missing+="$(printf '%q ' "$c")"
        else
            strokes+=("$q")
        fi
    done
    if [[ -n "$missing" ]]; then
        note "keys: the VM path cannot type $missing"
        return 1
    fi
    for ((i = 0; i < ${#strokes[@]}; i++)); do
        # shellcheck disable=SC2086
        vm_send_key ${strokes[i]} || { note "QMP send-key failed at character $i"; return 1; }
    done
}

key() { # key <[mod+…]keysym> [press|release|tap]: `Return`, `ctrl+u`, `Escape release`
    local q phase="${2:-tap}" rev=() i
    q=$(vm_key_qcodes "$1") || { note "key: the VM path has no key for '$1'"; return 1; }
    read -ra q <<<"$q"
    case "$phase" in
        tap) vm_send_key "${q[@]}" ;;
        press) vm_key_events true "${q[@]}" ;;
        release)
            for ((i = ${#q[@]} - 1; i >= 0; i--)); do rev+=("${q[i]}"); done
            vm_key_events false "${rev[@]}"
            ;;
        *) note "key: the phase is press, release or tap, not '$phase'"; return 1 ;;
    esac
}

# Only this run's lines: the guest's journal keeps every earlier start since
# boot (and a WAYLAND_DEBUG session is hundreds of thousands of lines), and a
# full dump over ssh both blows the 30 s deadline and answers "popup mapped"
# with last run's position. The cursor is taken just before the bar starts.
JOURNAL_CURSOR=""
journal() {
    local after=()
    # The cursor carries ';' and '=', so it is single-quoted for the remote
    # shell; the caller's own arguments are appended unquoted.
    [[ -n "$JOURNAL_CURSOR" ]] && after=("--after-cursor='$JOURNAL_CURSOR'")
    gssh 30 "journalctl --user -u caustics-bar --no-pager -o cat ${after[*]} $*"
}
mark_journal() {
    JOURNAL_CURSOR=$(gssh 30 'journalctl --user -u caustics-bar --no-pager -n 0 --show-cursor 2>/dev/null' |
        sed -n 's/^-- cursor: //p')
    [[ -n "$JOURNAL_CURSOR" ]] || note "no journal cursor; reading the whole journal"
}

# --- the unit ----------------------------------------------------------------
#
# The bar is `caustics-bar.service`, and a step's environment is a drop-in named
# for its group: `bar_env power CAUSTICS_POWER_DRY_RUN=1` is `power.conf`. Every
# value is written quoted, because systemd splits an unquoted `Environment=`
# on whitespace (a two-command `$CAUSTICS_SIDECARS` would be split).

DROPINS='~/.config/systemd/user/caustics-bar.service.d'

bar_env() { # bar_env <group> KEY=VALUE…
    local group="$1" kv
    shift
    {
        printf '[Service]\n'
        for kv in "$@"; do printf 'Environment="%s"\n' "$kv"; done
    } | gssh 20 "cat > $DROPINS/$group.conf"
}

bar_env_clear() { # bar_env_clear <group>
    gssh 20 "rm -f $DROPINS/$1.conf"
}

# A stop, then a start, rather than one `restart`: the old bar writes until
# it dies — a last layout, when a sidecar republishes in the gap — and those
# lines would land after a mark the caller took before calling here. With the
# unit stopped (`stop` waits for the process), a caller's mark is retaken, so
# its journal is the new instance's from its first line.
bar_restart() {
    gssh 30 'systemctl --user daemon-reload; systemctl --user reset-failed caustics-bar 2>/dev/null
        systemctl --user stop caustics-bar' >/dev/null
    [[ -n "$JOURNAL_CURSOR" ]] && mark_journal
    gssh 30 'systemctl --user start caustics-bar' >/dev/null
}

bar_stop() {
    gssh 20 'systemctl --user stop caustics-bar' >/dev/null
}

bar_active() { gssh 20 'systemctl --user is-active caustics-bar'; }
bar_restarts() { gssh 20 'systemctl --user show -p NRestarts --value caustics-bar'; }
bar_pid() { gssh 20 'systemctl --user show -p MainPID --value caustics-bar' | tr -d '\r'; }

fixture_start() { # fixture_start <name> <command>
    gssh 30 "systemctl --user reset-failed caustics-$1 2>/dev/null
        $T_PRELUDE systemd-run --user --quiet --unit=caustics-$1 $2" >/dev/null
}
fixture_stop() { gssh 20 "systemctl --user stop caustics-$1 2>/dev/null" >/dev/null; }
fixture_log() { gssh 20 "journalctl --user -u caustics-$1 --no-pager -o cat | tail -3"; }

# Start one client in the guest's graphical session.
client_spawn() { # client_spawn <command>
    # With the prelude, as `on_target` and `fixture_start` have it: without
    # it a client command that names `$T_HOME` runs at an empty path.
    gssh 25 "$T_PRELUDE export WAYLAND_DISPLAY=\$(systemctl --user show-environment | sed -n 's/^WAYLAND_DISPLAY=//p')
        export XDG_RUNTIME_DIR=/run/user/\$(id -u)
        (setsid $1 >/dev/null 2>&1 </dev/null &) ; true" >/dev/null
}

# The guest parks its monitor and locks after ~30 minutes idle, and a
# screendump of a parked monitor is a black rectangle with "Display output is
# not active" on it. Unlock, power the output back on, and hold an inhibitor
# for the length of the run.
wake_output() {
    gssh 40 'sudo -n loginctl unlock-sessions 2>/dev/null; true' >/dev/null
    niri_msg action power-on-monitors >/dev/null 2>&1
    niri_msg output "$OUTPUT" on >/dev/null 2>&1
    # A real input event is what actually un-parks it; the IPC action alone did not
    # bring this virtio-gpu back.
    qmp input-send-event '{"events":[{"type":"abs","data":{"axis":"x","value":16000}},{"type":"abs","data":{"axis":"y","value":16000}}]}' >/dev/null 2>&1
    qmp input-send-event '{"events":[{"type":"abs","data":{"axis":"x","value":16400}},{"type":"abs","data":{"axis":"y","value":15600}}]}' >/dev/null 2>&1
    sleep 2
    gssh 30 'pkill -f caustics-bar-test-inhibit; nohup setsid systemd-inhibit --what=idle:sleep --who=caustics-bar-test-inhibit --why="caustics-bar test run" sleep 7200 >/dev/null 2>&1 </dev/null & disown; true' >/dev/null 2>&1
    sleep 3
}

# Before the bar starts: niri parks the display after about 30 minutes idle
# and, on this QEMU virtio-gpu, does not always bring it back, so an inhibitor
# holds it on for the length of the run.
before_bar_start() {
    gssh 20 'pkill -f caustics-bar-test-inhibit; (setsid systemd-inhibit --what=idle:sleep --who=caustics-bar-test-inhibit --why="caustics-bar test run" sleep 7200 >/dev/null 2>&1 </dev/null &) ; true' >/dev/null
}

target_cleanup() {
    # The guest's memory at the end of the run, so a guest sized down (`$TOPAZ_TEST_RAM`
    # in bar/vm-run.sh) is seen swapping or OOM-killing rather than
    # guessed at from flaky waits: used/available in MiB, swap in use, and any
    # kernel OOM line.
    note "guest memory at the end: $(gssh 20 'free -m | awk "/^Mem:/ {printf \"used %s MiB, available %s MiB\", \$3, \$7} /^Swap:/ {printf \", swap used %s MiB\", \$3}"; n=$(sudo -n journalctl -k --no-pager 2>/dev/null | grep -ci -E "out of memory|oom-kill"); echo ", oom lines: ${n:-?}"' 2>/dev/null | tr -d '\n')"
    gssh 30 'rm -f ~/.config/systemd/user/caustics-bar.service.d/fault.conf \
        ~/.config/systemd/user/caustics-bar.service.d/sysmon.conf \
        ~/.config/systemd/user/caustics-bar.service.d/sidecars.conf \
        ~/.config/systemd/user/caustics-bar.service.d/power.conf \
        ~/.config/systemd/user/caustics-bar.service.d/charts.conf
        if [ -f ~/bar_tunables.slang.orig ]; then
            mv -f ~/bar_tunables.slang.orig ~/caustics-bar/caustics/bar_tunables.slang
        fi
        systemctl --user daemon-reload
        systemctl --user stop caustics-bar
        systemctl --user reset-failed caustics-bar
        systemctl --user stop caustics-fake-upower 2>/dev/null
        systemctl --user reset-failed caustics-fake-upower 2>/dev/null' >/dev/null 2>&1
}

# After the windows are closed: the output back to the scale it booted at.
target_cleanup_last() {
    niri_msg output "$OUTPUT" scale 1 >/dev/null 2>&1
    note "bar stopped, windows closed, $OUTPUT back to scale 1"
}
