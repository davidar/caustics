#!/usr/bin/env bash
# The bar's steps: one bar in one session, driven with pointer events,
# screenshots and the bar's own journal; one verdict line per claim
# (docs/testing.md describes the run and its output). The steps reach the bar
# only through the transport surface of `bar/targets/$CAUSTICS_TARGET.sh`:
# `nested` (a nested niri on the machine's GPU, run by `bar/nested-run.sh`)
# or `vm` (a topaz-os guest booted by its test harness, run by `bar/vm-run.sh`;
# docs/testing.md, *The VM target*). Steps that exercise the target image
# rather than the bar say `vm_only "<why>"`. The trap
# below always stops the bar and puts the target back.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "${CAUSTICS_TARGET:-}" in
    vm | nested) ;;
    *)
        echo "bar-test.sh: CAUSTICS_TARGET must be vm or nested (run bar/vm-run.sh or bar/nested-run.sh)" >&2
        exit 2
        ;;
esac
# shellcheck disable=SC1090
source "$REPO/bar/targets/$CAUSTICS_TARGET.sh"
BINARY="$REPO/target/debug/caustics-bar"
SIDECAR="$REPO/target/debug/caustics-sysmon"
# The bar starts every sidecar from beside its own binary.
TRAY_SIDECAR="$REPO/target/debug/caustics-tray"
POWER_SIDECAR="$REPO/target/debug/caustics-power"
AUDIO_SIDECAR="$REPO/target/debug/caustics-audio"
NETWORK_SIDECAR="$REPO/target/debug/caustics-network"
BLUETOOTH_SIDECAR="$REPO/target/debug/caustics-bluetooth"
# The notification daemon.
NOTIFY_SIDECAR="$REPO/target/debug/caustics-notify"
# Desktop entries and colour icons, answering `Request` frames.
APPS_SIDECAR="$REPO/target/debug/caustics-apps"
# The font, the OpenType arena and the symbolic icons: the bar loads no font
# itself, so **without this one there is no text on the bar at all**.
GLYPHS_SIDECAR="$REPO/target/debug/caustics-glyphs"
# The compiler: watches `caustics/`, the tunables among it, compiles it with
# Slang in process and writes the caustic to the bar as `Module` frames (every step that edits the
# tunables waits on one of its swaps).
RELOAD_SIDECAR="$REPO/target/debug/caustics-reload"
# The rows-frame worked example (docs/abi.md, *Writing a sidecar*), an extra sidecar.
ROWS_DEMO="$REPO/target/debug/examples/rows_demo"
# The output (`$OUTPUT`, 1280x800 px) is the target's, at scale 2: 640x400 logical.

# The dock's favourites, expanded by the target's shell (a `~` would expand
# here). `cleanup` removes it, so every run starts from the built-in seed.
guest_favourites='$T_STATE/caustics/bar/favourites'

# The bar's line when a reloaded generation reached the GPU (`reload: failed`
# is caustics-reload's for a compile that did not build).
SWAPPED="modules, swapped"

FAILURES=0
PASSES=0
SKIPS=0
mkdir -p "$SHOTS"

# --- the clock -------------------------------------------------------------
# Every line carries the time since the run started; the run ends with the ten
# slowest steps and how many bounded waits ran their whole bound (a failure,
# or a negative proved by waiting it out: a passing run should have none).
# `$EPOCHREALTIME` stripped to its digits is microseconds in any locale.
now_us() { local t=${EPOCHREALTIME//[!0-9]/}; printf '%s' "$((10#$t))"; }
RUN_T0_US=$(now_us)
fmt_us() { # fmt_us <microseconds> -> +H:MM:SS.mmm
    local us=$1
    printf '+%d:%02d:%02d.%03d' $((us / 3600000000)) $((us / 60000000 % 60)) \
        $((us / 1000000 % 60)) $((us / 1000 % 1000))
}
secs_us() { # secs_us <microseconds> -> seconds with one decimal
    printf '%d.%d' $(($1 / 1000000)) $(($1 / 100000 % 10))
}
stamp() { fmt_us $(($(now_us) - RUN_T0_US)); }

STEP_NAMES=()
STEP_TOOK=()
STEP_WAITED=()
STEP_BOUNDS=()
STEP_CUR=-1
STEP_T0_US=0
# Close the step that is running: its duration goes into the table.
step_close() {
    ((STEP_CUR >= 0)) || return 0
    STEP_TOOK[STEP_CUR]=$(($(now_us) - STEP_T0_US))
}
# A bounded wait's cost, charged to the step it ran in. `reached` is 1 when
# the wait ran its whole bound without seeing its line.
wait_charge() { # wait_charge <started_us> <reached 0|1> <pattern> <bound s>
    ((STEP_CUR >= 0)) || return 0
    STEP_WAITED[STEP_CUR]=$((STEP_WAITED[STEP_CUR] + $(now_us) - $1))
    if [[ "$2" == 1 ]]; then
        STEP_BOUNDS[STEP_CUR]=$((STEP_BOUNDS[STEP_CUR] + 1))
        printf '  ....  %s  wait for "%s" ran its whole %s s bound\n' "$(stamp)" "$3" "$4"
    fi
}

say() {
    step_close
    STEP_CUR=${#STEP_NAMES[@]}
    STEP_NAMES+=("$*")
    STEP_TOOK+=(0)
    STEP_WAITED+=(0)
    STEP_BOUNDS+=(0)
    STEP_T0_US=$(now_us)
    printf '\n=== %s  %s\n' "$(stamp)" "$*"
}
pass() { printf '  PASS  %s  %s\n' "$(stamp)" "$*"; PASSES=$((PASSES + 1)); }
fail() { printf '  FAIL  %s  %s\n' "$(stamp)" "$*"; FAILURES=$((FAILURES + 1)); }
note() { printf '  ....  %s  %s\n' "$(stamp)" "$*"; }

# A part that exercises the VM image rather than the bar: runs on the VM,
# elsewhere is one SKIP line. `if vm_only "<why>"; then …; fi`.
vm_only() { # vm_only <why> -> 0 on the VM target
    [[ "$T_NAME" == vm ]] && return 0
    printf '  SKIP  %s  %s\n' "$(stamp)" "$*"
    SKIPS=$((SKIPS + 1))
    return 1
}

# The table, from the EXIT trap, so an early `exit 1` still prints it.
timing_summary() {
    step_close
    STEP_CUR=-1
    local total=$(($(now_us) - RUN_T0_US)) i bounds=0 waited=0
    for i in "${!STEP_NAMES[@]}"; do
        bounds=$((bounds + STEP_BOUNDS[i]))
        waited=$((waited + STEP_WAITED[i]))
    done
    printf '\n=== timing: %s total, %d steps, %s s in bounded waits, %d wait(s) ran their bound\n' \
        "$(fmt_us "$total")" "${#STEP_NAMES[@]}" "$(secs_us "$waited")" "$bounds"
    printf '  %8s  %8s  %6s  %s\n' "took s" "waited s" "bounds" "step (the ten slowest)"
    for i in "${!STEP_NAMES[@]}"; do
        printf '%s\t%s\n' "${STEP_TOOK[i]}" "$i"
    done | sort -rn | head -10 | while IFS=$'\t' read -r _ i; do
        printf '  %8s  %8s  %6d  %s\n' "$(secs_us "${STEP_TOOK[i]}")" \
            "$(secs_us "${STEP_WAITED[i]}")" "${STEP_BOUNDS[i]}" "${STEP_NAMES[i]}"
    done
}

# assert <description> <haystack> <needle>
assert_contains() {
    # A pattern match, not `printf | grep -q`: under `pipefail` the writer's
    # SIGPIPE fails the pipeline despite a match (see `jgrep`). Every `grep -q`
    # in this file takes the journal as a here-string.
    if [[ "$2" == *"$3"* ]]; then pass "$1"; else fail "$1 (no '$3')"; fi
}

# `shot <name>` is the target's: a 1280x800 RGB PNG in `$SHOTS`.

# How many pixels of one rectangle differ between two screenshots. Not a mean:
# Adwaita's sun and moon have nearly the same ink, so the chip's mean RGB moves
# by 1 while 668 of its 3360 pixels move by more than 8.
png_pixel_diff() { # png_pixel_diff <a.png> <b.png> <x> <y> <w> <h>
    python3 - "$@" <<'PY'
import struct, sys, zlib

def load(path):
    data = open(path, "rb").read()
    pos, width, height, raw = 8, 0, 0, b""
    while pos + 8 <= len(data):
        length, tag = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if tag == b"IHDR":
            width, height = struct.unpack(">II", body[:8])
        elif tag == b"IDAT":
            raw += body
        pos += length + 12
    return width, height, zlib.decompress(raw)

x, y, w, h = (int(float(v)) for v in sys.argv[3:7])
wa, ha, pa = load(sys.argv[1])
wb, hb, pb = load(sys.argv[2])
if (wa, ha) != (wb, hb):
    print("-1")
    raise SystemExit
stride = wa * 3 + 1
moved = 0
for row in range(y, min(y + h, ha)):
    base = row * stride + 1
    for col in range(x, min(x + w, wa)):
        i = base + col * 3
        if max(abs(pa[i + c] - pb[i + c]) for c in range(3)) > 8:
            moved += 1
print(moved)
PY
}

# Mean RGB of a screenshot rectangle, as "R G B". `shot` writes 8-bit RGB PNGs
# with filter 0 on every row, so zlib and arithmetic are the whole decoder.
# True when every pixel of the PNG is the same colour: a parked output or a
# lock screen, never a desktop with a bar on it.
png_is_uniform() { # png_is_uniform <file>
    python3 - "$1" <<'PY'
import struct, sys, zlib
data = open(sys.argv[1], "rb").read()
pos, width, height, raw = 8, 0, 0, b""
while pos + 8 <= len(data):
    length, tag = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + length]
    if tag == b"IHDR":
        width, height = struct.unpack(">II", body[:8])
    elif tag == b"IDAT":
        raw += body
    pos += 12 + length
img = zlib.decompress(raw)
stride = width * 3 + 1
first = img[1:stride]
if first != first[:3] * width:
    sys.exit(1)
for row in range(1, height):
    if img[row * stride] != 0 or img[row * stride + 1:(row + 1) * stride] != first:
        sys.exit(1)
sys.exit(0)
PY
}

png_mean() { # png_mean <file> <x> <y> <w> <h>
    python3 - "$@" <<'PY'
import struct, sys, zlib
path = sys.argv[1]
# The rect comes off a `layout:` line, whose values are floats.
x, y, w, h = (int(float(v)) for v in sys.argv[2:6])
data = open(path, "rb").read()
pos, width, height, raw = 8, 0, 0, b""
while pos + 8 <= len(data):
    length, tag = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + length]
    if tag == b"IHDR":
        width, height = struct.unpack(">II", body[:8])
    elif tag == b"IDAT":
        raw += body
    pos += 12 + length
img = zlib.decompress(raw)
stride = width * 3 + 1
r = g = b = n = 0
for row in range(max(y, 0), min(y + h, height)):
    base = row * stride + 1
    for col in range(max(x, 0), min(x + w, width)):
        i = base + col * 3
        r += img[i]
        g += img[i + 1]
        b += img[i + 2]
        n += 1
print(f"{r // max(n, 1)} {g // max(n, 1)} {b // max(n, 1)}")
PY
}

# One field of the first window in a `niri msg --json` reply, whether that
# reply is a list (`windows`) or a single object (`focused-window`).
WINDOWS_PY='
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(1)
if isinstance(data, list):
    data = data[-1] if data else None
if isinstance(data, dict) and sys.argv[1] in data:
    print(data[sys.argv[1]])
'

# Windows this run opened: cleanup closes those by id, never by binary name.
OPENED_WINDOWS=()

# Start one client in the target's graphical session and remember its window id.
guest_spawn() { # guest_spawn <command>
    local before after
    before=$(niri_msg --json windows | python3 -c '
import json, sys
print(" ".join(str(w["id"]) for w in json.load(sys.stdin)))' 2>/dev/null)
    client_spawn "$1"
    sleep 3
    after=$(niri_msg --json windows | python3 -c '
import json, sys
print(" ".join(str(w["id"]) for w in json.load(sys.stdin)))' 2>/dev/null)
    for id in $after; do
        case " $before " in
            *" $id "*) ;;
            *) OPENED_WINDOWS+=("$id"); note "opened window $id" ;;
        esac
    done
}

# `pointer_to <x> <y>` (screen px) and `button <true|false> [which]` are the
# target's: QMP on the target, a virtual pointer on the nested niri.

click_at() { pointer_to "$1" "$2"; sleep 0.4; button true; sleep 0.3; button false; sleep 0.6; }

# A secondary click. The 0.4 s dwell lets the hover preview come up first, so
# the menu has to replace it.
right_click_at() { # right_click_at <px x> <px y>
    pointer_to "$1" "$2"; sleep 0.4
    button true right; sleep 0.3; button false right; sleep 0.8
}

# The pointer off the bar, long enough for the hover spring to settle. A
# before/after texel comparison needs this first, or it measures the hover.
park_pointer() {
    pointer_to $((SCREEN_W / 2)) $((SCREEN_H / 3))
    sleep 1.5
}

# `journal` and `mark_journal` are the target's: the bar's log from
# `$JOURNAL_CURSOR` on. `journal | grep -q` quits at the first match, and
# under `pipefail` the writer's SIGPIPE (141) fails the pipeline *despite* the
# match once the journal is past a pipe buffer; `grep -c` reads it all.
jgrep() { grep -c "$@" >/dev/null; }

# The newest `layout:` rect for one element, as "x y w h" in that target's px.
layout_rect() { # layout_rect <ELEM_NAME>
    journal | grep -F "layout: $1 " | grep -v 'rect=gone' | tail -1 |
        sed -n 's/.*rect=\([0-9.-]*\),\([0-9.-]*\),\([0-9.-]*\),\([0-9.-]*\).*/\1 \2 \3 \4/p'
}

# Like wait_for_journal, but only the newest line may satisfy it.
wait_for_journal_tail() { # wait_for_journal_tail <pattern> <seconds>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until journal | tail -n 1 | grep -qF -- "$1"; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "$1" "$2"; return 1; }
        sleep 1
    done
    wait_charge "$t0" 0
}

wait_for_journal() { # wait_for_journal <pattern> <seconds>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until journal | jgrep -F -- "$1"; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "$1" "$2"; return 1; }
        sleep 1
    done
    wait_charge "$t0" 0
}

# Like wait_for_journal, for a condition that is not a journal line: a
# command run here (which may itself be an `on_target`) until it succeeds.
wait_until() { # wait_until <what, for the timing table> <seconds> <command>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until eval "$3" >/dev/null 2>&1; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "$1" "$2"; return 1; }
        sleep 0.5
    done
    wait_charge "$t0" 0
}

# How many lines of the run so far carry `pattern`.
journal_count() { # journal_count <pattern>
    journal | grep -cF -- "$1"
}

# Like wait_for_journal, but only for a line beyond the count `<before>`:
# steps share witnesses, and a stale match would read a frame too early.
wait_for_journal_more() { # wait_for_journal_more <pattern> <seconds> <before>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until [[ "$(journal_count "$1")" -gt "$3" ]]; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "$1" "$2"; return 1; }
        sleep 1
    done
    wait_charge "$t0" 0
}

# Where the bar's top edge is on screen: hover the clock, and compare the y we
# asked for with the y the bar saw. The walk starts at the screen edge the bar
# is anchored to (`top`, the shipped `EDGE`, unless told otherwise) and moves
# inwards. The hover witness is one line per *change*, so park the pointer
# mid-screen first and accept only a new line.
probe_bar_origin() { # probe_bar_origin <clock centre x> [top|bottom]
    local before after screen_y seen_y probe edge="${2:-top}"
    pointer_to $((SCREEN_W / 2)) $((SCREEN_H / 2))
    sleep 1
    for probe in $(seq 1 8 300); do
        if [[ "$edge" == bottom ]]; then
            screen_y=$((SCREEN_H - probe))
        else
            screen_y=$((probe - 1))
        fi
        before=$(journal_count "hover: clock at=")
        pointer_to "$1" "$screen_y"
        sleep 0.5
        after=$(journal_count "hover: clock at=")
        [[ "$after" -gt "$before" ]] || continue
        seen_y=$(journal | grep -F "hover: clock at=" | tail -1 |
            sed -n 's/.*at=\([0-9.-]*\),\([0-9.-]*\).*/\2/p')
        [[ -n "$seen_y" ]] || continue
        python3 -c "print(round($screen_y - $seen_y))"
        return 0
    done
    return 1
}

cleanup() {
    say "cleanup"
    target_cleanup
    on_target 20 "rm -f $guest_favourites" >/dev/null 2>&1
    # The windows this run opened, and only those, by niri window id.
    for id in ${OPENED_WINDOWS[@]+"${OPENED_WINDOWS[@]}"}; do
        niri_msg action close-window --id "$id" >/dev/null 2>&1
    done
    target_cleanup_last
    timing_summary
}
trap cleanup EXIT
# A signal (bar/nested-run.sh's cap sends TERM) ends the run through cleanup.
trap 'exit 130' INT TERM

# --- step 1: build ----------------------------------------------------------

say "1. build"
# All sixteen binaries (the bar spawns each sidecar from beside itself, and
# the canvas its own), the rows demo, and the sink sidecars' fake host.
if (cd "$REPO" &&
    cargo build --offline -p caustics-bar -p caustics-sysmon -p caustics-tray -p caustics-power \
        -p caustics-audio -p caustics-network -p caustics-bluetooth -p caustics-notify -p caustics-apps -p caustics-glyphs \
        -p caustics-reload -p caustics-canvas -p caustics-wallpaper -p caustics-polkit \
        -p caustics-lock &&
    cargo build --offline -p caustics-bar-abi --example rows_demo --example fake_host); then
    pass "cargo build of the bar, its ten sidecars, the canvas, its sidecar and client, the polkit and lock sinks, the rows_demo example and the fake host"
else
    fail "cargo build of the set, the rows_demo example or the fake host"
    exit 1
fi

# ...and the crate's test binary, which carries the oracle.
ORACLE_BIN=$( (cd "$REPO" &&
    cargo test --offline -p caustics-bar --no-run --message-format=json 2>/dev/null) |
    /usr/bin/sed -n 's/.*"executable":"\([^"]*caustics_bar-[^"]*\)".*/\1/p' | head -1)
if [[ -n "$ORACLE_BIN" && -x "$ORACLE_BIN" ]]; then
    pass "the oracle's test binary ($(basename "$ORACLE_BIN"))"
else
    fail "the oracle's test binary did not build"
fi

# A battery for a target that has none (the battery-menu step).
FAKE_UPOWER_BIN="$REPO/target/debug/examples/fake_upower"
(cd "$REPO" && cargo build --offline -p caustics-power --example fake_upower) >/dev/null 2>&1 || true
if [[ -x "$FAKE_UPOWER_BIN" ]]; then
    pass "the fake UPower ($(basename "$FAKE_UPOWER_BIN"))"
else
    fail "the fake UPower did not build"
fi

# --- step 2: copy into the target -------------------------------------------

say "2. the set and lavapipe are on the target"
# The whole of this step is the VM's: a nested run's set is `target/debug/`
# where cargo left it. Nothing compiles the caustic but the set itself:
# `caustics-reload` carries Slang inside its binary.
if vm_only "the set upload, the unit and lavapipe are the guest image's (nested: the set is target/debug)"; then
if ! gssh 20 true; then
    fail "the VM is not reachable over ssh on port $SSH_PORT — start it with tests/run.sh start"
    exit 1
fi
# A running bar holds its own binary open (ETXTBSY): stop it before the copy.
gssh 30 'systemctl --user stop caustics-bar 2>/dev/null
    mkdir -p ~/.local/bin ~/caustics-bar/caustics ~/.config/systemd/user/caustics-bar.service.d
    rm -f ~/.local/bin/caustics-bar ~/.local/bin/caustics-sysmon ~/.local/bin/caustics-tray \
        ~/.local/bin/caustics-power ~/.local/bin/caustics-audio ~/.local/bin/caustics-network \
        ~/.local/bin/caustics-bluetooth ~/.local/bin/caustics-notify ~/.local/bin/caustics-apps \
        ~/.local/bin/caustics-glyphs ~/.local/bin/caustics-reload \
        ~/.local/bin/caustics-canvas ~/.local/bin/caustics-wallpaper ~/.local/bin/caustics-canvasctl \
        ~/.local/bin/caustics-polkit ~/.local/bin/caustics-lock \
        ~/.local/bin/rows_demo'

gscp "$BINARY" tester@127.0.0.1:.local/bin/caustics-bar && pass "binary" || fail "binary"
# Beside the bar, which is where the bar looks first.
gscp "$SIDECAR" tester@127.0.0.1:.local/bin/caustics-sysmon && pass "sidecar" || fail "sidecar"
gscp "$TRAY_SIDECAR" tester@127.0.0.1:.local/bin/caustics-tray && pass "tray sidecar" ||
    fail "tray sidecar"
gscp "$POWER_SIDECAR" tester@127.0.0.1:.local/bin/caustics-power && pass "power sidecar" ||
    fail "power sidecar"
gscp "$AUDIO_SIDECAR" tester@127.0.0.1:.local/bin/caustics-audio && pass "audio sidecar" ||
    fail "audio sidecar"
gscp "$NETWORK_SIDECAR" tester@127.0.0.1:.local/bin/caustics-network && pass "network sidecar" ||
    fail "network sidecar"
gscp "$BLUETOOTH_SIDECAR" tester@127.0.0.1:.local/bin/caustics-bluetooth && pass "bluetooth sidecar" ||
    fail "bluetooth sidecar"
gscp "$NOTIFY_SIDECAR" tester@127.0.0.1:.local/bin/caustics-notify && pass "notify sidecar" ||
    fail "notify sidecar"
gscp "$APPS_SIDECAR" tester@127.0.0.1:.local/bin/caustics-apps && pass "apps sidecar" ||
    fail "apps sidecar"
gscp "$GLYPHS_SIDECAR" tester@127.0.0.1:.local/bin/caustics-glyphs && pass "glyphs sidecar" ||
    fail "glyphs sidecar"
gscp "$RELOAD_SIDECAR" tester@127.0.0.1:.local/bin/caustics-reload && pass "reload sidecar" ||
    fail "reload sidecar"
# The rest of the set: the canvas, its sidecar and client, and its two sinks.
# The bar starts none of them, but a set is all sixteen, and the install step
# (25d) installs it whole from here.
for name in caustics-canvas caustics-wallpaper caustics-canvasctl caustics-polkit caustics-lock; do
    gscp "$REPO/target/debug/$name" "tester@127.0.0.1:.local/bin/$name" && pass "$name" ||
        fail "$name"
done
# Not beside the bar: it is a fixture, not a sidecar.
gscp "$FAKE_UPOWER_BIN" tester@127.0.0.1:caustics-bar/caustics-fake-upower && pass "fake UPower" ||
    fail "fake UPower"
# Beside it too, so `$CAUSTICS_SIDECARS` resolves it by bare name.
gscp "$ROWS_DEMO" tester@127.0.0.1:.local/bin/rows_demo && pass "rows demo" ||
    fail "rows demo"
# The oracle is a test binary, not a sidecar: the oracle step runs it alone, on lavapipe.
if [[ -n "$ORACLE_BIN" ]]; then
    gscp "$ORACLE_BIN" tester@127.0.0.1:caustics-bar-oracle && pass "oracle binary" ||
        fail "oracle binary"
fi
# Every file of the caustic that is in the tree, which is every `*.slang` in
# `caustics/` (the same list `caustics-reload` watches): a directory missing one
# starts off the baked SPIR-V and **cannot reload** (`cannot open file`),
# failing every later swap. The modules a caustic imports that are not in the
# tree, the bar's rendered contract and the text engine's, are not copied: the
# guest's own binaries write them from their compiled-in copies.
gscp "$REPO"/caustics/*.slang \
    tester@127.0.0.1:caustics-bar/caustics/ && pass "caustics" || fail "caustics"
gscp "$REPO"/bar/systemd/caustics-bar.service \
    tester@127.0.0.1:.config/systemd/user/caustics-bar.service && pass "unit" || fail "unit"

# A guest image may ship lavapipe itself and record its version line in
# /usr/lib/caustics-vm/VERSION. When that line's lavapipe is this host's, the
# guest's copy is used; anything else means a copy, so a stale image costs
# only time. An image's line may still name a Slang release before the
# lavapipe hash (images built when the guest needed a compiler of its own):
# only the last field, the lavapipe one, is compared.
GUEST_LVP_DIR='$HOME'
host_toolchain=$(vm_toolchain_version)
image_toolchain=$(gssh 20 'cat /usr/lib/caustics-vm/VERSION 2>/dev/null')
if [[ -n "$image_toolchain" && "${image_toolchain##*; }" == "$host_toolchain" ]]; then
    pass "the image carries this host's lavapipe ($host_toolchain); nothing to copy"
    GUEST_LVP_DIR=/usr/lib/caustics-vm
else
    [[ -n "$image_toolchain" ]] &&
        note "the image's toolchain is '$image_toolchain', this host's '$host_toolchain'; copying"
    # lavapipe: the guest ships no working Vulkan ICD of its own, so the host's
    # (same mesa and llvm versions) drops in with a hand-written manifest.
    have_lvp=$(gssh 20 'stat -c %s ~/libvulkan_lvp.so 2>/dev/null || echo 0')
    if [[ "$(stat -c %s "$LVP_HOST")" == "$have_lvp" ]]; then
        pass "libvulkan_lvp.so already present"
    else
        gscp "$LVP_HOST" tester@127.0.0.1:libvulkan_lvp.so && pass "libvulkan_lvp.so" ||
            fail "libvulkan_lvp.so"
    fi
fi
# The manifest names the guest's own path, so it is written in the guest.
gssh 20 "dir=$GUEST_LVP_DIR"'; printf "{\n  \"file_format_version\": \"1.0.1\",\n  \"ICD\": {\n    \"library_path\": \"%s/libvulkan_lvp.so\",\n    \"api_version\": \"1.4.354\"\n  }\n}\n" "$dir" > ~/lvp_icd.json' >/dev/null

gssh 20 'cat > ~/.config/systemd/user/caustics-bar.service.d/vm.conf <<CONF
[Service]
Environment=VK_DRIVER_FILES=%h/lvp_icd.json
# The guest runs xwayland-satellite, so the session environment carries
# DISPLAY=:0 — and with it, one of the loader'"'"'s implicit layers opens the X
# display while wgpu is enumerating adapters and never comes back: the bar
# hangs in poll(), one thread, after logging its configure and before logging
# an adapter. Unsetting DISPLAY fixes it too; disabling implicit layers is the
# fix that also says what the bar wants, which is no overlays at all.
Environment=VK_LOADER_LAYERS_DISABLE=~all~
Environment=CAUSTICS_DIR=%h/caustics-bar/caustics
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
# The bar logs its layout: lines at DEBUG under their own target, since on a
# long-running bar they would be most of the journal; this harness reads them
# to know where to click, so it asks for them back. The shape: line is one per model change.
# (No apostrophes and no backticks in here: this block is inside a
# single-quoted host string *and* an unquoted guest heredoc, so one of either
# ends the quoting or runs a command.)
Environment=RUST_LOG=info,caustics_bar::layout=debug,caustics_bar::shape=debug
CONF' >/dev/null && pass "drop-in" || fail "drop-in"
fi
# The tunables as the run found them. Every step that edits the target's
# `bar_tunables.slang` puts this snapshot back (`restore_tunables`), and a
# restarted bar waits for a swap whenever the file differs from it.
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_HOME/bar_tunables.slang.orig' &&
    pass "the shipped tunables snapshotted" || fail "could not snapshot the tunables"

say "2b. the oracle: the table bar_build builds, on this adapter"
# The oracle's corpus on **this** adapter (docs/testing.md, *Three tiers*): the
# words behind the rects the other steps read. 600 s because on the VM it runs
# on lavapipe on the target's vCPUs (112 s quiet, past 300 s on a busy host);
# it is a correctness gate, and the stamps say where a slow pass was.
oracle_out=$(on_target 600 'cd "$T_HOME" && vk_env && CAUSTICS_BAR_ORACLE=1 \
    CAUSTICS_DIR=$T_CAUSTICS \
    "$T_ORACLE" oracle --nocapture --test-threads=1 2>&1 |
    awk "{ print strftime(\"%T\"), \$0; fflush() }" | tee "$T_HOME/oracle.log"' |
    tail -200)
note "$(printf '%s' "$oracle_out" | grep -E '^(oracle:|[a-z].*complaints)' | tail -25)"
assert_contains "the oracle ran and the built table holds together" \
    "$oracle_out" "/ 0 complaints"
assert_contains "and the corpus is the whole corpus" "$oracle_out" "test result: ok"
# A 150-row level clamped to a 1000 px output; a 5-row one ignores the wheel.
assert_contains "a tall menu is clamped to the output" "$oracle_out" \
    "a tall menu: want [160.0, 952.0], 36 rows placed of 150"
assert_contains "and a short one ignores the wheel" "$oracle_out" "the wheel ignored"
# `RFLAG_SHOW` with two attention rows of five: a two-row toast, ended three ways.
assert_contains "a chip that asks is shown as a toast" "$oracle_out" \
    "the toast opens without a grab, shows two rows, and ends three ways"
# Three months (one starting on a Sunday, a 29-day February) laid out right.
assert_contains "the clock's click opens this month's calendar" "$oracle_out" \
    "the calendar opens, lays out three months, and closes"
# The chart numbers for every kind and unit, shaped with no notdef.
assert_contains "the chart numbers compose every kind" "$oracle_out" \
    "the chart numbers compose every kind"
assert_contains "the strip's hover and click open the numbers" "$oracle_out" \
    "the strip's hover and click open the numbers, and they close"
# A long body wraps greedily at spaces; a toast shows four lines, then "…".
assert_contains "a notification's body wraps under its summary" "$oracle_out" \
    "oracle: a body wraps at spaces"
# A toast's rows name themselves when clicked, even across a re-indexing build.
assert_contains "a toast is the rows it opened with" "$oracle_out" \
    "oracle: a toast is the rows it opened with, and a click names the row under it"

say "3. wake the session, then output scale 2"
# The guest parks and locks after ~30 minutes idle; a nested output never does.
wake_output

if niri_msg output "$OUTPUT" scale 2 >/dev/null 2>&1; then
    pass "$OUTPUT at scale 2 (${SCREEN_W}x${SCREEN_H} px, $((SCREEN_W / 2))x$((SCREEN_H / 2)) logical)"
else
    fail "could not set $OUTPUT scale 2"
fi
sleep 2

say "4. the bar starts, lays out, and finds its own position on screen"
before_bar_start
mark_journal
bar_restart
if wait_for_journal "layout: clock" 90; then
    pass "the bar laid itself out"
else
    fail "no 'layout:' line within 90 s"
fi
log="$(journal)"
printf '%s\n' "$log" > "$SHOTS/journal-start.txt"
note "journal saved to $SHOTS/journal-start.txt"
assert_contains "unit is active" "$(bar_active)" active
assert_contains "adapter is the target's ($T_ADAPTER)" "$log" "$T_ADAPTER"
assert_contains "surface format negotiated" "$log" "surface format negotiated"
assert_contains "layer surface configured at scale 2" "$log" "scale=2"
# The output's size from xdg_output: the bound a menu is clamped to.
assert_contains "the output's logical size is known (menus are clamped to it)" "$log" "output: logical size"
shot bar.png

# A layer-shell client cannot know its own screen position (other exclusive
# zones move it), so walk the pointer down from the top edge, where the
# shipped tunables anchor the bar, until the caustic reports the clock, the
# one item that always exists, under it.
clock_rect=($(layout_rect clock))
if [[ ${#clock_rect[@]} -ne 4 ]]; then
    fail "could not parse the clock's rect from the journal"
    exit 1
fi
clock_cx=$(python3 -c "print(round(${clock_rect[0]} + ${clock_rect[2]} / 2))")
clock_cy=$(python3 -c "print(round(${clock_rect[1]} + ${clock_rect[3]} / 2))")
note "clock rect (bar px): ${clock_rect[*]}, centre ${clock_cx},${clock_cy}"

# `hover: clock at=x,y` is in bar px, so one hit gives the exact origin.
bar_origin_y=$(probe_bar_origin "$clock_cx")
if [[ -n "$bar_origin_y" ]]; then
    pass "the bar's top edge is at screen y=$bar_origin_y px"
else
    fail "never hovered the clock; the bar's screen position is unknown"
    exit 1
fi

say "5. the dock lays out the running app"
# A terminal window of our own. cosmic-term first: its entry (the seed's
# fourth) fits the dock, where kitty's (the seventh) overflows under the clock.
guest_term=$(on_target 20 'for t in cosmic-term kitty foot alacritty xterm; do
    command -v $t >/dev/null 2>&1 && { echo $t; break; }; done')
if [[ -z "$guest_term" ]]; then
    fail "the target has no terminal to open; the dock, click and preview steps cannot run"
else
    note "target terminal: $guest_term"
    guest_spawn "$guest_term"
    sleep 3
fi

# The dock keys on the app_id niri reports, so ask niri.
term_app_id=$(niri_msg --json windows |
    python3 -c "$WINDOWS_PY" app_id 2>/dev/null)
term_window=$(niri_msg --json windows |
    python3 -c "$WINDOWS_PY" id 2>/dev/null)
note "the target's window: app_id=${term_app_id:-none} id=${term_window:-none}"

if wait_for_journal "dock: " 20; then
    dock_line=$(journal | grep -F 'dock: ' | tail -1)
    entries=$(printf '%s' "$dock_line" | sed -n 's/.*dock: \([0-9]*\) entries.*/\1/p')
    if [[ "${entries:-0}" -ge 1 ]]; then
        pass "the dock laid out: $dock_line"
    else
        fail "the dock reported no entries: $dock_line"
    fi
else
    fail "no 'dock:' line within 20 s"
fi

entry_rect=()
if [[ -n "$term_app_id" ]]; then
    entry_rect=($(layout_rect "$term_app_id"))
    if [[ ${#entry_rect[@]} -eq 4 ]]; then
        pass "the running app has a dock entry: layout: $term_app_id ${entry_rect[*]}"
    else
        fail "no 'layout: $term_app_id' line; the dock did not lay the running app out"
    fi
fi
shot dock.png

# The dock draws an entry at once, wearing its app id; the name arrives from
# `caustics-apps` a round trip later as an `App` frame, and the trip is measured.

say "5b. caustics-apps names the dock entry over the pipe"
if wait_for_journal "sidecar caustics-apps: app " 20; then
    apps_line=$(journal | grep -F 'sidecar caustics-apps: app ' | tail -1)
    pass "caustics-apps answered a REQ_APP: $apps_line"
else
    fail "no 'sidecar caustics-apps: app …'; the REQ_APP round trip never closed"
fi
if [[ -n "$term_app_id" ]]; then
    # From the first `dock:` line (the build that asked) to the `App` frame:
    # one `.desktop` walk and one 64-square raster per entry.
    ROUND_TRIP_PY='
import re, sys
app = sys.argv[1]
asked = answered = None
for line in sys.stdin:
    m = re.match(r"^(\d+\.\d+)\s+(.*)$", line)
    if not m:
        continue
    at, text = float(m.group(1)), m.group(2)
    if asked is None and "caustics_bar::wayland: dock: " in text:
        asked = at
    if answered is None and ("caustics-apps: app \"" + app + "\"") in text:
        answered = at
if asked is not None and answered is not None and answered >= asked:
    print(f"{(answered - asked) * 1000:.0f}")
'
    round_ms=$(journal -o short-unix | python3 -c "$ROUND_TRIP_PY" "$term_app_id")
    if [[ -n "$round_ms" ]]; then
        pass "the dock's first build to this entry's name: ${round_ms} ms over the pipe"
    else
        note "the two lines were not both in the window; the round trip is unmeasured"
    fi
    named=$(journal | grep -F "sidecar caustics-apps: app \"$term_app_id\"" | tail -1)
    if [[ -n "$named" ]]; then
        pass "and the entry has a name: $named"
    else
        fail "caustics-apps never answered for $term_app_id"
    fi
fi

# A CJK title has no glyphs in the bar's primary face: the caustic flags the blocks, the
# bar sends their bytes to `caustics-glyphs` as a `REQ_COVER`, and the sidecar
# appends glyphs from its fontconfig chain; the count goes back to zero *while
# the window is open*. With no CJK face, the witness is `no face has U+…`.

say "5c. a CJK title the font lacks is covered by caustics-glyphs"
cjk_covered=0
guest_ja=$(on_target 20 "fc-list ':lang=ja' file 2>/dev/null | head -1")
if [[ -n "$guest_ja" ]]; then
    note "the target has a Japanese face: $guest_ja"
else
    note "the target has no Japanese face; expecting the 'no face has' witness"
fi
if [[ -z "$guest_term" ]]; then
    fail "the target has no terminal; the CJK title cannot be put on screen"
else
    # The title by the terminal escape; base64 so no quoting layer mangles UTF-8.
    cjk_script='#!/bin/sh
printf "\033]0;%s\007" "日本語のウィンドウ"
sleep 900
'
    cjk_b64=$(printf '%s' "$cjk_script" | base64 -w0)
    on_target 20 "printf %s '$cjk_b64' | base64 -d > \$T_HOME/caustics-cjk.sh; chmod +x \$T_HOME/caustics-cjk.sh" \
        >/dev/null
    before_windows=${#OPENED_WINDOWS[@]}
    # Each line below was logged once at start (before the font landed), so
    # each wait is for a *new* one.
    before_flagged=$(journal_count "runs have a glyph the font lacks")
    before_asked=$(journal_count "asking caustics-glyphs to cover")
    before_covered=$(journal_count "glyphs: cover ")
    before_clean=$(journal_count "text: every run has its glyphs")
    guest_spawn "$guest_term -e \$T_HOME/caustics-cjk.sh"
    if [[ ${#OPENED_WINDOWS[@]} -eq $before_windows ]]; then
        # Not every terminal takes `-e`; the bare form is the other spelling.
        guest_spawn "$guest_term \$T_HOME/caustics-cjk.sh"
    fi
    cjk_window=""
    if [[ ${#OPENED_WINDOWS[@]} -gt $before_windows ]]; then
        cjk_window=${OPENED_WINDOWS[-1]}
    fi
    if [[ -z "$cjk_window" ]]; then
        fail "no window opened for the CJK title"
    else
        note "the CJK window is niri id $cjk_window"
        # 1. The caustic flagged it, and the bar said so.
        if wait_for_journal_more "runs have a glyph the font lacks" 25 "$before_flagged"; then
            pass "the shaper flagged the title: $(journal |
                grep -F 'runs have a glyph the font lacks' | tail -1 |
                sed -n 's/.*caustics_bar::wayland: //p')"
        else
            fail "no 'text: N runs have a glyph the font lacks'; the title shaped clean"
        fi
        # 2. The bar asked, with the bytes and not a decoded anything.
        if wait_for_journal_more "asking caustics-glyphs to cover" 25 "$before_asked"; then
            pass "the bar asked for cover: $(journal |
                grep -F 'asking caustics-glyphs to cover' | tail -1 |
                sed -n 's/.*caustics_bar::wayland: //p')"
        else
            fail "no 'text: asking caustics-glyphs to cover …'; the REQ_COVER never went"
        fi
        # 3. The sidecar answered — with glyphs, or with the honest refusal.
        if wait_for_journal_more "glyphs: cover " 30 "$before_covered"; then
            pass "the sidecar covered it: $(journal | grep -F 'glyphs: cover ' | tail -1)"
            cjk_covered=1
        elif [[ "$(journal_count 'glyphs: no face has U+')" -gt 0 ]]; then
            pass "no face on the target's chain can draw it, and the sidecar said so: \
$(journal | grep -F 'glyphs: no face has U+' | tail -1)"
        else
            fail "caustics-glyphs neither covered the string nor refused it"
        fi
        # 4. And then the boxes are gone — with the window still on screen.
        if [[ "$cjk_covered" -eq 1 ]]; then
            if wait_for_journal_more "text: every run has its glyphs" 30 "$before_clean"; then
                pass "the run re-shaped and the notdef count is zero again"
            else
                fail "the glyphs landed but the run is still flagged: $(journal |
                    grep -E 'text: ' | tail -2 | tr '\n' ' ')"
            fi
            shot cjk-title.png
        fi
        niri_msg action close-window --id "$cjk_window" >/dev/null 2>&1
        sleep 2
    fi
fi

say "6. a dock click focuses the window, a second click cycles"
if [[ ${#entry_rect[@]} -ne 4 ]]; then
    fail "no dock entry to click"
else
    entry_cx=$(python3 -c "print(round(${entry_rect[0]} + ${entry_rect[2]} / 2))")
    entry_cy=$(python3 -c "print(round(${entry_rect[1]} + ${entry_rect[3]} / 2))")
    entry_sy=$((bar_origin_y + entry_cy))
    note "entry centre: bar px ${entry_cx},${entry_cy} -> screen ${entry_cx},${entry_sy}"

    mark=$(journal | grep -cF 'action: Activate')
    click_at "$entry_cx" "$entry_sy"
    sleep 1
    log="$(journal)"
    activate=$(printf '%s' "$log" | grep -F 'action: Activate' | tail -1)
    if printf '%s' "$activate" | grep -q -- '-> niri-ipc'; then
        pass "the click became one action: $activate"
    else
        fail "no 'action: Activate(N) -> niri-ipc' after the click (had $mark before)"
    fi
    focused=$(niri_msg --json focused-window | python3 -c "$WINDOWS_PY" id 2>/dev/null)
    if [[ -n "$focused" && "$focused" == "$term_window" ]]; then
        pass "niri focused window $focused, which is the one the caustic picked"
    else
        fail "focused-window is '${focused:-none}', expected '$term_window'"
    fi

    # A second window of the same app: a click on the focused entry cycles.
    guest_spawn "$guest_term"
    sleep 3
    second=$(niri_msg --json focused-window | python3 -c "$WINDOWS_PY" id 2>/dev/null)
    if [[ -z "$second" || "$second" == "$focused" ]]; then
        note "the second window did not take focus; the cycle check is inconclusive"
    else
        click_at "$entry_cx" "$entry_sy"
        sleep 1
        cycled=$(niri_msg --json focused-window | python3 -c "$WINDOWS_PY" id 2>/dev/null)
        if [[ "$cycled" != "$second" ]]; then
            pass "clicking the focused entry cycled: $second -> $cycled"
        else
            fail "clicking the focused entry did not cycle (still $cycled)"
        fi
    fi
    shot dock-click.png
fi

# The harness's keyboard (`keys`, `key`: docs/testing.md, *The steps and their
# targets*), proved end to end before anything relies on it: a terminal of
# our own, focused by id, takes a typed shell line and a Return, and the line
# writes a file whose bytes are the ones typed, shifted characters and all.
# Then a chord: `ctrl+u` clears a half-typed `false && ` before it can veto
# the line typed after it. The absolute path is typed, because a terminal's
# shell need not start where it was spawned.

say "6b. the harness types: text, Return and a chord reach a focused terminal"
keys_typed='Typed: A-Z & a~z, 0-9 (ok)!'
keys_home=$(on_target 10 'printf %s "$T_HOME"')
keys_window=""
if [[ -z "$guest_term" ]]; then
    fail "the target has no terminal to type into"
elif [[ ! "$keys_home" =~ ^[A-Za-z0-9/._-]+$ ]]; then
    fail "the target's home '$keys_home' has characters this step will not type into a path"
else
    on_target 10 'rm -f "$T_HOME/caustics-keys-text" "$T_HOME/caustics-keys-chord"' >/dev/null
    keys_focus_before=$(niri_msg --json focused-window | python3 -c "$WINDOWS_PY" id 2>/dev/null)
    before_windows=${#OPENED_WINDOWS[@]}
    guest_spawn "$guest_term"
    if [[ ${#OPENED_WINDOWS[@]} -gt $before_windows ]]; then
        keys_window=${OPENED_WINDOWS[-1]}
    fi
fi
if [[ -n "$keys_window" ]]; then
    niri_msg action focus-window --id "$keys_window" >/dev/null 2>&1
    if wait_until "focused-window $keys_window" 10 \
        '[[ "$(niri_msg --json focused-window | python3 -c "$WINDOWS_PY" id)" == "$keys_window" ]]'; then
        pass "the terminal's window $keys_window has keyboard focus"
    else
        fail "window $keys_window never took focus; the keys below go elsewhere"
    fi

    keys "printf '%s\\n' '$keys_typed' > $keys_home/caustics-keys-text" && key Return
    if wait_until "caustics-keys-text" 15 'on_target 10 "test -s \$T_HOME/caustics-keys-text"'; then
        got=$(on_target 10 'cat "$T_HOME/caustics-keys-text"')
        if [[ "$got" == "$keys_typed" ]]; then
            pass "a typed line and Return ran, and wrote exactly what was typed: $got"
        else
            fail "the line ran but wrote '$got', not '$keys_typed'"
        fi
    else
        fail "no caustics-keys-text within 15 s of typing the line and Return"
    fi

    keys "false && " && key ctrl+u &&
        keys "printf '%s\\n' chord > $keys_home/caustics-keys-chord" && key Return
    if wait_until "caustics-keys-chord" 15 'on_target 10 "test -s \$T_HOME/caustics-keys-chord"'; then
        pass "ctrl+u cleared the half-typed 'false && ', and the line after it ran"
    else
        fail "no caustics-keys-chord within 15 s: ctrl+u did not clear 'false && '"
    fi

    # The window closed and the focus put back, for the steps after this one.
    niri_msg action close-window --id "$keys_window" >/dev/null 2>&1
    wait_until "window $keys_window closed" 10 \
        '! niri_msg --json windows | python3 -c "import json, sys; sys.exit(not any(w[\"id\"] == $keys_window for w in json.load(sys.stdin)))"' ||
        note "window $keys_window was still open 10 s after close-window"
    [[ -n "$keys_focus_before" ]] &&
        niri_msg action focus-window --id "$keys_focus_before" >/dev/null 2>&1
    on_target 10 'rm -f "$T_HOME/caustics-keys-text" "$T_HOME/caustics-keys-chord"' >/dev/null
elif [[ -n "$guest_term" && -n "$keys_home" ]]; then
    fail "no window opened for the keyboard's terminal"
fi

say "7. hovering a dock entry opens a live window preview"
if [[ ${#entry_rect[@]} -ne 4 ]]; then
    fail "no dock entry to hover"
else
    mark_preview=$SECONDS
    pointer_to "$entry_cx" "$entry_sy"
    sleep 1.5
    log="$(journal)"
    assert_contains "a capture session started" "$log" "capture session for window"
    if grep -qE 'capture session for window [0-9]+: started' <<<"$log"; then
        pass "the capture session says 'started'"
    else
        note "$(printf '%s' "$log" | grep -F 'capture session' | tail -3)"
        fail "no 'capture session for window N: started'"
    fi
    if wait_for_journal "popup: requested kind=preview" 6; then
        pass "the caustic asked for a preview popup"
    else
        fail "no 'popup: requested kind=preview' after $((SECONDS - mark_preview)) s of hover"
    fi
    if wait_for_journal "popup: open kind=preview" 6; then
        pass "the preview popup mapped"
    else
        fail "no 'popup: open kind=preview'"
    fi
    if wait_for_journal "preview: window" 8; then
        pass "$(journal | grep -F 'preview: window' | tail -1)"
    else
        fail "no 'preview: window N WxH' line; nothing reached the preview slot"
    fi
    sleep 1
    shot preview.png

    # Into the popup, not away from it: the preview must survive the pointer
    # crossing the gap between the two surfaces (the caustic's PREVIEW_GRACE_S).
    popup_pos=$(journal | grep -F 'popup: open kind=preview' | tail -1 |
        sed -n 's/.* at \([0-9-]*\),\([0-9-]*\).*/\1 \2/p')
    if [[ -n "$popup_pos" ]]; then
        read -r ppx ppy <<<"$popup_pos"
        # The positioner's offset is logical and relative to the bar's surface.
        popup_cx=$(python3 -c "print(round($ppx * 2 + ${entry_rect[2]} / 2))")
        popup_cy=$((bar_origin_y + ppy * 2 + 40))
        note "moving into the popup at screen ${popup_cx},${popup_cy}"
        pointer_to "$popup_cx" "$popup_cy"
        sleep 1.5
        if journal | tail -n 30 | grep -qF 'popup: closed'; then
            fail "the preview closed while the pointer moved into it (the grace is too short)"
        else
            pass "the preview survived the pointer crossing into it"
        fi
    else
        note "no popup position in the journal; the cross-into check is skipped"
    fi

    # ...and away: it tears down. The far right edge, since the preview (up
    # to 320x200 logical) can cover the screen's centre.
    pointer_to $((SCREEN_W - 40)) $((SCREEN_H / 2))
    sleep 2
    log="$(journal)"
    if grep -qE 'capture session .*: stopped' <<<"$log"; then
        pass "the capture session was torn down on leave"
    else
        fail "no 'capture session ...: stopped' after the pointer left"
    fi
    assert_contains "the preview popup closed" "$log" "popup: closed"
    shot preview-gone.png
fi

say "8. caustics-tray starts and reports the tray"
# The run's bus has no SNI application: the sidecar starts, publishes an
# empty applet frame, and no tray chip is laid out.
if wait_for_journal "sidecar caustics-tray: started pid=" 30; then
    pass "the pipe feeder started it: $(journal |
        grep -F 'sidecar caustics-tray: started pid=' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-tray: started pid=N'; the bar did not find caustics-tray"
fi
if wait_for_journal "sidecar caustics-tray: rows, applet=4" 30; then
    tray_rows_line=$(journal | grep -F 'sidecar caustics-tray: rows, applet=4' | tail -1)
    pass "its applet frame arrived: $(printf '%s' "$tray_rows_line" |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-tray: rows, applet=4 …'; the rows path did not reach the bar"
fi
# No session bus, or one with nothing on it: both are a tray of nothing.
log="$(journal)"
if grep -qF 'caustics-tray: absent: session-bus' <<<"$log"; then
    pass "no session bus on the target; the sidecar reported it and went on"
elif grep -qE 'caustics-tray: published rev=[0-9]+ items=0' <<<"$log"; then
    pass "a session bus with no SNI items: $(printf '%s' "$log" |
        grep -oE 'caustics-tray: published rev=[0-9]+ items=[0-9]+ rows=[0-9]+' | tail -1)"
else
    note "the target has SNI items: $(printf '%s' "$log" |
        grep -oE 'caustics-tray: published rev=[0-9]+ items=[0-9]+ rows=[0-9]+' | tail -1)"
fi
if journal | jgrep -E 'layout: applet-caustics-tray[ -]'; then
    note "a tray chip is on the bar; the target grew an SNI application"
else
    pass "and no tray chip on the bar, because there is nothing to put on it"
fi

say "9. caustics-power starts; a battery chip only with a battery"
if wait_for_journal "sidecar caustics-power: started pid=" 30; then
    pass "the pipe feeder started it: $(journal |
        grep -F 'sidecar caustics-power: started pid=' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-power: started pid=N'; the bar did not find caustics-power"
fi
if wait_for_journal "sidecar caustics-power: rows, applet=5" 30; then
    pass "its applet frame arrived: $(journal |
        grep -F 'sidecar caustics-power: rows, applet=5' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-power: rows, applet=5 …'"
fi
# No UPower, no battery, or a display device: all honest; the step is the pipe.
log="$(journal)"
if grep -qF 'caustics-power: absent: upower' <<<"$log"; then
    pass "UPower is absent on the target and the sidecar said so"
elif grep -qE 'caustics-power: published rev=[0-9]+ battery=none' <<<"$log"; then
    pass "UPower is there and reports no battery: $(printf '%s' "$log" |
        grep -oE 'caustics-power: published rev=[0-9]+ battery=[a-z0-9%]+' | tail -1)"
elif grep -qE 'caustics-power: published rev=[0-9]+ battery=' <<<"$log"; then
    pass "UPower is there and reports a display device: $(printf '%s' "$log" |
        grep -oE 'caustics-power: published rev=[0-9]+ battery=[a-z0-9%]+' | tail -1)"
else
    fail "neither 'caustics-power: absent: upower' nor 'battery=none' ($(printf '%s' "$log" |
        grep -E 'caustics-power: ' | tail -2 | tr '\n' ' '))"
fi
# The button is on the bar. The battery chip (the sidecar's row 8) is there
# exactly when UPower reports a real battery (`battery=NN%`).
has_battery=0
grep -qE 'caustics-power: published rev=[0-9]+ battery=[0-9]+%' <<<"$log" && has_battery=1
if wait_for_journal "layout: applet-caustics-power " 20; then
    pass "the power button is on the bar: $(journal |
        grep -F 'layout: applet-caustics-power ' | tail -1 |
        sed -n 's/.*layout: //p')"
else
    fail "no 'layout: applet-caustics-power …'; the power button was not laid out"
fi
if ((has_battery)); then
    if wait_for_journal "layout: applet-caustics-power-8 target=bar" 20; then
        pass "the machine has a battery and its chip is on the bar: $(journal |
            grep -F 'layout: applet-caustics-power-8 target=bar' | tail -1 | sed -n 's/.*layout: //p')"
    else
        fail "the sidecar reports a battery but no battery chip was laid out"
    fi
elif journal | jgrep -E 'layout: applet-caustics-power-[0-9]+ target=bar'; then
    fail "a battery chip was laid out on a machine with no battery"
else
    pass "the right wing laid out without a battery chip"
fi

# Audio, network, bluetooth: what a target offers them varies, so this is the
# *contract*: each starts, writes a rows frame with its applet number, and
# says `absent: …` or publishes. Their pictures are the live tests' business.

say "9b. caustics-audio, caustics-network and caustics-bluetooth start and publish"
for spec in "caustics-audio 1" "caustics-network 2" "caustics-bluetooth 3"; do
    set -- $spec
    name=$1
    applet=$2
    if wait_for_journal "sidecar $name: started pid=" 30; then
        pass "$name: the pipe feeder started it: $(journal |
            grep -F "sidecar $name: started pid=" | tail -1 |
            sed -n 's/.*feeders::pipe: //p')"
    else
        fail "$name: no 'sidecar $name: started pid=N'; the bar did not find it"
        continue
    fi
    if wait_for_journal "sidecar $name: rows, applet=$applet" 30; then
        pass "$name: its rows frame arrived with applet=$applet: $(journal |
            grep -F "sidecar $name: rows, applet=$applet" | tail -1 |
            sed -n 's/.*feeders::pipe: //p')"
    else
        fail "$name: no 'sidecar $name: rows, applet=$applet …'"
    fi
    # Absent, or published: whichever the target gave it. Both are one line.
    log="$(journal | grep -F "$name: ")"
    if grep -qE "$name: absent: " <<<"$log"; then
        pass "$name: said what the target lacks: $(printf '%s' "$log" |
            grep -oE "$name: absent: [a-z-]+" | sort -u | tr '\n' ' ')"
    elif grep -qE "$name: published rev=[0-9]+" <<<"$log"; then
        pass "$name: published a picture: $(printf '%s' "$log" |
            grep -oE "$name: published rev=[0-9]+.*" | tail -1)"
    else
        fail "$name: neither 'absent:' nor 'published rev=' ($(printf '%s' "$log" |
            tail -2 | tr '\n' ' '))"
    fi
    # None has a reason to exit (and be restarted) while the bar is up.
    if journal | jgrep -E "sidecar $name: .*(exited|restarting)"; then
        fail "$name: exited or restarted ($(journal |
            grep -oE "sidecar $name: .*(exited|restarting).*" | tail -1))"
    else
        pass "$name: stayed up"
    fi
done

# --- helpers for the steps that restart the bar and open menus -------------

# Restart the bar and wait for its layout, re-marking the journal so no wait
# is satisfied by the run before it.
restart_bar() { # restart_bar <what changed>
    mark_journal
    bar_restart
    if wait_for_journal "layout: clock" 60; then
        note "the bar came back ($1)"
    else
        fail "the bar did not come back after $1"
    fi
    # caustics-reload's start: on the bar's format it sends the cached
    # generation for the tree as it is now, or nothing (the bar already runs
    # what it was built from). Wait for its word on which, and for the swap
    # when there is one, so that no later step counts a start's swap as the
    # answer to its own edit. Tunables edited since the run began must be a
    # hit: the edit was compiled, and cached, when it was saved. Only if
    # caustics-reload is in this start's set: its start line precedes
    # `layout: clock`, so a set without it does not wait.
    local edited=0 start="" deadline=$((SECONDS + 30))
    on_target 20 'cmp -s "$T_CAUSTICS/bar_tunables.slang" "$T_HOME/bar_tunables.slang.orig"' ||
        edited=1
    if ! journal | jgrep -F "sidecar caustics-reload: started pid="; then
        [[ "$edited" -eq 1 ]] &&
            note "caustics-reload is not in this start's sidecar set; the baked chrome stays"
        return 0
    fi
    until [[ -n "$start" ]] || ((SECONDS >= deadline)); do
        if journal | jgrep -F "the cache holds this directory's program"; then
            start=hit
        elif journal | jgrep -F "nothing cached for this directory"; then
            start=miss
        else
            sleep 1
        fi
    done
    case "$start" in
    hit)
        if wait_for_journal "$SWAPPED" 90; then
            sleep 2
        else
            fail "caustics-reload sent its cached program but no swap within 90 s"
        fi
        ;;
    miss)
        [[ "$edited" -eq 0 ]] ||
            fail "the edited tunables were not in caustics-reload's cache; the bar runs its baked chrome"
        ;;
    *)
        fail "caustics-reload never answered the bar's format within 30 s"
        ;;
    esac
}

# Put the target's tunables back as the run found them: the snapshot, written
# whole by a rename like every edit. Returns once the swap that compiles them
# lands, or 1 after 60 s without one.
restore_tunables() {
    local before
    before=$(journal_count "$SWAPPED")
    on_target 20 'cp $T_HOME/bar_tunables.slang.orig $T_CAUSTICS/bar_tunables.slang.new
        mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
    wait_for_journal_more "$SWAPPED" 60 "$before"
}

# The names laid out *now* on a target: a departed element logs `rect=gone`,
# so the newest *frame* per name answers (valid at a menu's root, straight
# after it opened). Per frame, not per line: a row changing element slot can
# log its gone line after its placement in the same frame (docs/testing.md,
# *Known behaviour the steps account for*). Lines within 5 ms are one frame.
layout_placed_now() { # layout_placed_now <name regex> <popup|any>
    journal | grep -E "layout: ($1) target=" |
        sed -n 's/^[^ ]*T\([0-9]*\):\([0-9]*\):\([0-9.]*\)Z* .*layout: \([^ ]*\) target=\([^ ]*\) .*/\1 \2 \3 \4 \5/p' |
        awk -v want="$2" '
            { t = $1 * 3600 + $2 * 60 + $3
              if (NR == 1 || t - prev > 0.005) frame++
              prev = t
              if (at[$4] != frame) { at[$4] = frame; placed[$4] = 0 }
              if ($5 == want || (want == "any" && $5 != "none")) placed[$4] = 1 }
            END { for (n in placed) if (placed[n]) print n }' |
        sort
}
power_rows_now() {
    # `caustics-power`'s row ids (`rows.rs`): 2 Lock, 3 Log Out, 4 the separator,
    # 5 Suspend, 6 Restart, 7 Shut Down; row 8, the battery, is on the bar.
    layout_placed_now 'applet-caustics-power-[0-9]+|ELEM_BACK' popup
}

# A mark, and everything after it. A submenu descent reuses element slots
# without `rect=gone`, so "on screen now" is "laid out since the click".
journal_lines() { journal | wc -l; }
journal_since() { journal | tail -n +$(($1 + 1)); }

# Click one element of the open popup: its origin from the newest `popup:
# open` line (logical, bar-relative), the row's rect in popup px. The x
# fraction's default is exactly the middle of a slider's travel.
popup_click() { # popup_click <ELEM_NAME> [x fraction, default 0.5] [menu|toast]
    local rect ppos ppx ppy x y frac kind
    frac="${2:-0.5}"
    kind="${3:-menu}"
    rect=($(layout_rect "$1"))
    if [[ ${#rect[@]} -ne 4 ]]; then
        fail "no 'layout: $1' rect in the popup"
        return 1
    fi
    ppos=$(journal | grep -F "popup: open kind=$kind" | tail -1 |
        sed -n 's/.* at \([0-9-]*\),\([0-9-]*\).*/\1 \2/p')
    if [[ -z "$ppos" ]]; then
        fail "no 'popup: open kind=$kind … at x,y' line to place $1 against"
        return 1
    fi
    read -r ppx ppy <<<"$ppos"
    x=$(python3 -c "print(round($ppx * 2 + ${rect[0]} + ${rect[2]} * $frac))")
    y=$(python3 -c "print(round($bar_origin_y + $ppy * 2 + ${rect[1]} + ${rect[3]} / 2))")
    note "$1: popup rect ${rect[*]} -> screen $x,$y"
    click_at "$x" "$y"
}

# How red a rectangle is over its own green and blue: the CPU chart is
# #ff0600, and a difference of means survives a translucent slab.
# One notification over the bus (`gdbus`; `notify-send` may be absent): the
# chip grows its count, the menu opens, the row is clicked, and the daemon
# closes it (its witness, the `NotificationClosed` a monitor saw, the count).

say "9c. caustics-notify serves the name; a click dismisses a notification"
# The session bus, as the target's shell reaches it.
guest_bus='bus_env'
if wait_for_journal "sidecar caustics-notify: started pid=" 30; then
    pass "the pipe feeder started it: $(journal |
        grep -F 'sidecar caustics-notify: started pid=' | tail -1 | sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-notify: started pid=N'; the bar did not find caustics-notify"
fi
# If another daemon holds the name, the sidecar queues; the holder is killed
# **by the pid the bus names**, and a respawn of it is recorded.
holder=$(on_target 20 "$guest_bus; busctl --user status org.freedesktop.Notifications 2>/dev/null |
    sed -n 's/^PID=\([0-9]*\).*/\1/p'")
holder_comm=$(on_target 20 "$guest_bus; busctl --user status org.freedesktop.Notifications 2>/dev/null |
    sed -n 's/^Comm=//p'")
# On a private bus the sidecar itself already holds it.
if [[ "$holder_comm" == caustics-notify ]]; then
    holder=""
    holder_comm=""
fi
if [[ -z "$holder" ]] && ! vm_only "the handover from the image's own daemon: on a private bus nothing else holds org.freedesktop.Notifications, so only the 'acquired at once' half runs"; then
    :
elif [[ -n "$holder" ]]; then
    if wait_for_journal "caustics-notify: waiting for org.freedesktop.Notifications" 10; then
        pass "the name is held by $holder_comm (pid $holder) and the sidecar says it is waiting"
    else
        fail "the name is held by $holder_comm (pid $holder) but the sidecar did not say it is waiting"
    fi
    on_target 20 "kill $holder" >/dev/null 2>&1 || true
    # The old daemon may take tens of seconds to let go, so time the handover on the bus.
    t_kill=$SECONDS
    while ((SECONDS - t_kill < 60)); do
        now_holder=$(on_target 20 "$guest_bus; busctl --user status org.freedesktop.Notifications 2>/dev/null |
            sed -n 's/^PID=\([0-9]*\).*/\1/p'")
        [[ "$now_holder" != "$holder" ]] && break
        sleep 1
    done
    if [[ "$now_holder" != "$holder" ]]; then
        note "the bus moved the name off pid $holder after $((SECONDS - t_kill)) s (now pid ${now_holder:-none})"
    else
        fail "$holder_comm (pid $holder) still holds the name 60 s after the kill"
    fi
else
    note "nothing on the target holds org.freedesktop.Notifications"
fi
if wait_for_journal "serving org.freedesktop.Notifications" 30; then
    pass "it serves the name: $(journal | grep -oE 'caustics-notify: (acquired|serving) [^;]*' | tail -1)"
else
    fail "it does not serve the name ($(journal | grep -F 'caustics-notify: ' | tail -2 | tr '\n' ' '))"
fi
if [[ -n "$holder" ]]; then
    sleep 8
    again=$(on_target 20 "pgrep -x $holder_comm | tr '\n' ' '")
    if [[ -n "$again" ]]; then
        note "$holder_comm came back (pid $again), respawned by the session; it does not hold the name: $(on_target 20 "$guest_bus; busctl --user status org.freedesktop.Notifications 2>/dev/null | sed -n 's/^Comm=//p'")"
    else
        note "$holder_comm did not come back within 8 s"
    fi
fi
if wait_for_journal "sidecar caustics-notify: rows, applet=7" 30; then
    pass "its frame arrived with applet=7: $(journal |
        grep -F 'sidecar caustics-notify: rows, applet=7' | tail -1 | sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar caustics-notify: rows, applet=7 …'"
fi
bell_before=()
if wait_for_journal "layout: applet-caustics-notify " 20; then
    bell_before=($(layout_rect applet-caustics-notify))
    pass "the bell is on the bar: ${bell_before[*]}"
else
    fail "no 'layout: applet-caustics-notify …'; the bell was not laid out"
fi

# A monitor of the daemon's signals, bounded by `timeout`.
have_gdbus=$(on_target 20 'command -v gdbus >/dev/null && echo yes')
if [[ "$have_gdbus" == yes ]]; then
    on_target 20 "$guest_bus; rm -f \$T_HOME/notify-monitor.txt
        (setsid timeout 120 gdbus monitor --session --dest org.freedesktop.Notifications \
            >\$T_HOME/notify-monitor.txt 2>&1 </dev/null &) ; true" >/dev/null
    sleep 1
    reply=$(on_target 20 "$guest_bus; gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify -- \
        caustics-bar-test 0 dialog-information 'Hello from the test' 'the notification daemon step' '@as []' '@a{sv} {}' -1" 2>&1)
    notify_id=$(sed -n 's/^(uint32 \([0-9]*\),)$/\1/p' <<<"$reply")
else
    note "no gdbus on the target; busctl sends it and the signal is read from the daemon's witness"
    reply=$(on_target 20 "$guest_bus; busctl --user call org.freedesktop.Notifications \
        /org/freedesktop/Notifications org.freedesktop.Notifications Notify \
        susssasa{sv}i caustics-bar-test 0 dialog-information 'Hello from the test' 'the notification daemon step' 0 0 -1" 2>&1)
    notify_id=$(sed -n 's/^u \([0-9]*\)$/\1/p' <<<"$reply")
fi
if [[ -n "$notify_id" ]]; then
    pass "Notify answered with id $notify_id"
else
    fail "Notify did not answer with an id: $reply"
fi
if wait_for_journal "unread=1 entries=1" 20; then
    pass "the daemon published the count: $(journal | grep -F 'caustics-notify: published' |
        grep -F 'unread=1 entries=1' | tail -1 | sed -n 's/.*caustics-notify: /caustics-notify: /p')"
else
    fail "no 'caustics-notify: published … unread=1 entries=1'"
fi
# The count is a label beside the bell, so the chip is laid out wider.
if [[ ${#bell_before[@]} -eq 4 ]]; then
    deadline=$((SECONDS + 20))
    bell_now=("${bell_before[@]}")
    while ((SECONDS < deadline)); do
        bell_now=($(layout_rect applet-caustics-notify))
        [[ ${#bell_now[@]} -eq 4 ]] &&
            python3 -c "import sys; sys.exit(0 if ${bell_now[2]} > ${bell_before[2]} else 1)" && break
        sleep 1
    done
    if python3 -c "import sys; sys.exit(0 if ${bell_now[2]:-0} > ${bell_before[2]} else 1)"; then
        pass "the chip grew its label '1': ${bell_before[2]} -> ${bell_now[2]} px wide"
    else
        fail "the chip did not grow a label (width ${bell_before[2]} -> ${bell_now[2]:-?})"
    fi
    shot notify-chip.png
    bell_cx=$(python3 -c "print(round(${bell_now[0]} + ${bell_now[2]} / 2))")
    bell_cy=$(python3 -c "print(round(${bell_now[1]} + ${bell_now[3]} / 2))")
    opens_before=$(journal_count "popup: open kind=menu")
    # The toast already laid this row out: wait for one more, after the click.
    rows_before=$(journal_count "layout: applet-caustics-notify-$notify_id ")
    click_at "$bell_cx" $((bar_origin_y + bell_cy))
    if wait_for_journal_more "popup: open kind=menu" 8 "$opens_before"; then
        pass "the bell's click opened its menu"
    else
        fail "no menu popup from the bell"
    fi
    if [[ -n "$notify_id" ]] && wait_for_journal_more "layout: applet-caustics-notify-$notify_id " 20 "$rows_before"; then
        pass "the notification is a row of the menu: $(journal |
            grep -F "layout: applet-caustics-notify-$notify_id " | tail -1 | sed -n 's/.*layout: //p')"
        shot notify-menu.png
        closes_before=$(journal_count "popup: closed")
        empties_before=$(journal_count "unread=0 entries=0")
        if popup_click "applet-caustics-notify-$notify_id"; then
            if wait_for_journal "caustics-notify: closed id=$notify_id reason=2" 10; then
                pass "the click dismissed it: closed id=$notify_id reason=2"
            else
                fail "no 'caustics-notify: closed id=$notify_id reason=2' after the click"
            fi
            if wait_for_journal_more "unread=0 entries=0" 10 "$empties_before"; then
                pass "and the count is gone"
            else
                fail "the daemon still holds a notification after the click"
            fi
            if [[ "$have_gdbus" == yes ]]; then
                sleep 1
                if on_target 20 'cat "$T_HOME/notify-monitor.txt"' |
                    grep -qF "NotificationClosed (uint32 $notify_id, uint32 2)"; then
                    pass "the bus saw NotificationClosed($notify_id, 2)"
                else
                    fail "the monitor saw no NotificationClosed($notify_id, 2): $(on_target 20 \
                        'tail -3 "$T_HOME/notify-monitor.txt"' | tr '\n' ' ')"
                fi
            fi
        fi
    else
        fail "no 'layout: applet-caustics-notify-$notify_id' row in the menu"
    fi
    if wait_for_journal_more "popup: closed" 10 "${closes_before:-0}"; then
        pass "the menu closed behind the click"
    else
        fail "the menu stayed open"
    fi
fi
if journal | jgrep -E "sidecar caustics-notify: .*(exited|restarting)"; then
    fail "caustics-notify exited or restarted"
else
    pass "caustics-notify stayed up"
fi
on_target 20 'rm -f "$T_HOME/notify-monitor.txt"' >/dev/null

# A notification sets `RFLAG_SHOW` on the bell; the caustic opens a grabless
# toast showing its row and ends it when the bit clears at the timeout; with
# do-not-disturb nothing opens. "Within a second" is timed between two lines
# of the same journal, so transport round trips cost nothing.

say "9d. a notification opens a toast that closes on its timeout"

# Notify; `--` so gdbus does not read the timeout as an option.
notify_send() { # notify_send <summary> <body> <expire ms> -> the id, or nothing
    local reply
    if [[ "$have_gdbus" == yes ]]; then
        reply=$(on_target 20 "$guest_bus; gdbus call --session --dest org.freedesktop.Notifications \
            --object-path /org/freedesktop/Notifications \
            --method org.freedesktop.Notifications.Notify \
            -- vm-test 0 dialog-information '$1' '$2' '@as []' '@a{sv} {}' $3" 2>&1)
        sed -n 's/^(uint32 \([0-9]*\),)$/\1/p' <<<"$reply"
    else
        reply=$(on_target 20 "$guest_bus; busctl --user call org.freedesktop.Notifications \
            /org/freedesktop/Notifications org.freedesktop.Notifications Notify \
            susssasa{sv}i vm-test 0 dialog-information '$1' '$2' 0 0 $3" 2>&1)
        sed -n 's/^u \([0-9]*\)$/\1/p' <<<"$reply"
    fi
}

# "ta tb": the times of the newest line holding <b> and the last <a> before it.
journal_pair_times() { # journal_pair_times <a> <b>
    journal -o short-unix |
        python3 -c "
import sys
a, b = sys.argv[1], sys.argv[2]
lines = [l.split(' ', 1) for l in sys.stdin.read().splitlines() if ' ' in l]
at = max((i for i, (_, text) in enumerate(lines) if b in text), default=None)
if at is None:
    raise SystemExit
for t, text in reversed(lines[:at]):
    if a in text:
        print(t, lines[at][0])
        break
" "$1" "$2"
}

# Open the bell's menu with a click on the chip.
open_bell_menu() {
    local bell opens
    bell=($(layout_rect applet-caustics-notify))
    if [[ ${#bell[@]} -ne 4 ]]; then
        fail "no bell on the bar to click"
        return 1
    fi
    opens=$(journal_count "popup: open kind=menu")
    click_at "$(python3 -c "print(round(${bell[0]} + ${bell[2]} / 2))")" \
        "$(python3 -c "print(round($bar_origin_y + ${bell[1]} + ${bell[3]} / 2))")"
    if wait_for_journal_more "popup: open kind=menu" 8 "$opens"; then
        sleep 1 # the level's rows are built and laid out a frame after the open
        return 0
    fi
    fail "the bell's click opened no menu"
    return 1
}

# `caustics-notify`'s do-not-disturb and clear-all row ids (notify/src/rows.rs).
NOTIFY_DND_ROW=$((0xFFFF0001))
NOTIFY_CLEAR_ROW=$((0xFFFF0002))

toasts_before=$(journal_count "popup: requested kind=toast")
toast_closed_before=$(journal_count "toast: closed (flag)")
toast_shown_before=$(journal_count "toast: shown slot=")
popup_closed_before=$(journal_count "popup: closed")
bodies_before=$(journal_count "body: slot=")
# ~70 characters: past one toast line (~45 at 14 px), short of three. No
# apostrophe: it is quoted for the target's shell.
TOAST_BODY='This body is two sentences long. Both of them fit on two lines.'
toast_id=$(notify_send 'Toast from the test' "$TOAST_BODY" 10000)
if [[ -n "$toast_id" ]]; then
    pass "Notify answered with id $toast_id (expire_timeout 10000 ms)"
else
    fail "Notify did not answer with an id"
fi
toast_t0=$SECONDS

if wait_for_journal_more "popup: requested kind=toast" 5 "$toasts_before"; then
    line=$(journal | grep -F "popup: requested kind=toast" | tail -1)
    if [[ "$line" == *"grab=false"* ]]; then
        pass "the caustic asked for a toast, without a grab: ${line#*popup: }"
    else
        fail "the toast was asked for with a grab: ${line#*popup: }"
    fi
    times=($(journal_pair_times "caustics-notify: published" "popup: requested kind=toast"))
    # The newest `published` before the request is the frame that asked.
    if [[ ${#times[@]} -eq 2 ]]; then
        took=$(python3 -c "print(f'{${times[1]} - ${times[0]}:.3f}')")
        if python3 -c "import sys; sys.exit(0 if ${times[1]} - ${times[0]} <= 1.0 else 1)"; then
            pass "the toast was asked for ${took} s after the daemon's frame (≤ 1 s)"
        else
            fail "the toast was asked for ${took} s after the daemon's frame (> 1 s)"
        fi
    else
        fail "could not pair the daemon's frame with the toast's request in the journal"
    fi
    if journal | grep -F "caustics-notify: published" | tail -1 | grep -qF "show=1"; then
        pass "the daemon's frame carried the bit: $(journal | grep -F 'caustics-notify: published' |
            tail -1 | sed -n 's/.*caustics-notify: /caustics-notify: /p')"
    fi
    assert_contains "the caustic says why it opened" "$(journal)" "toast: open slot="
else
    fail "no 'popup: requested kind=toast' within 5 s of the Notify"
fi

toast_pos=()
if wait_for_journal "popup: open kind=toast" 5; then
    open_line=$(journal | grep -F "popup: open kind=toast" | tail -1)
    pass "the toast mapped: ${open_line#*popup: }"
    toast_pos=($(sed -n 's/.* px=\([0-9]*\)x\([0-9]*\) at \([0-9-]*\),\([0-9-]*\).*/\1 \2 \3 \4/p' \
        <<<"$open_line"))
    # Shot first: on a slow target the probes below can outlast the toast.
    sleep 0.5
    shot toast.png
else
    fail "no 'popup: open kind=toast'"
fi
if [[ -n "$toast_id" ]] && wait_for_journal "layout: applet-caustics-notify-$toast_id " 5; then
    pass "the notification is the toast's row: $(journal |
        grep -F "layout: applet-caustics-notify-$toast_id " | tail -1 | sed -n 's/.*layout: //p')"
else
    fail "no 'layout: applet-caustics-notify-$toast_id' row in the toast"
fi
# The `body:` witness says how many lines of the body the toast shows.
if [[ -n "$toast_id" ]] && wait_for_journal_more "body: slot=" 5 "$bodies_before"; then
    line=$(journal | grep -F "row=$toast_id lines=" | tail -1)
    if [[ "$line" == *"lines=2 cut=0"* ]]; then
        pass "the toast's body wraps to two lines under its summary: ${line#*body: }"
    elif [[ -n "$line" ]]; then
        fail "the toast's body is not two whole lines: ${line#*body: }"
    else
        fail "a body: line, but none for row $toast_id"
    fi
else
    fail "no 'body:' witness for the toast's row within 5 s"
fi

# The toast's texels against the same rect once it has gone.
if wait_for_journal_more "toast: closed (flag)" 14 "$toast_closed_before"; then
    if ((SECONDS - toast_t0 <= 12)); then
        pass "the toast closed when the daemon cleared the bit, $((SECONDS - toast_t0)) s after the Notify"
    else
        fail "the toast closed $((SECONDS - toast_t0)) s after the Notify (> 12 s)"
    fi
else
    fail "no 'toast: closed (flag)' within 14 s of a 10 s notification"
fi
# As it ends, a toast names the rows it showed (`ACTION_TOAST_SHOWN`).
if wait_for_journal_more "toast: shown slot=" 3 "$toast_shown_before"; then
    pass "the toast said what it showed: $(journal | grep -F 'toast: shown slot=' | tail -1 | sed -n 's/.*toast: shown/toast: shown/p')"
else
    fail "no 'toast: shown slot=' after the toast ended"
fi
if wait_for_journal_more "popup: closed" 3 "$popup_closed_before"; then
    pass "the embassy destroyed the toast's surface"
else
    fail "no 'popup: closed' after the toast ended"
fi
sleep 0.5
shot toast-gone.png
if [[ ${#toast_pos[@]} -eq 4 ]]; then
    tx=$((toast_pos[2] * 2))
    ty=$((bar_origin_y + toast_pos[3] * 2))
    moved=$(png_pixel_diff "$SHOTS/toast.png" "$SHOTS/toast-gone.png" \
        "$tx" "$ty" "${toast_pos[0]}" "${toast_pos[1]}")
    area=$((toast_pos[0] * toast_pos[1]))
    if ((moved > area / 10)); then
        pass "the toast drew: $moved of $area px in its rect differ from the background"
    else
        fail "the toast's rect ($tx,$ty ${toast_pos[0]}x${toast_pos[1]}) looks like the background ($moved of $area px differ)"
    fi
else
    fail "no toast position to compare texels in"
fi

# A toast is the rows it opened with. Two notifications 3 s apart: the second
# is held while the first's toast is up; a click on the first row closes that
# id alone; then the second toasts by itself.
say "9e. a toast keeps the rows it opened with; do-not-disturb opens none"
opens_before=$(journal_count "toast: open slot=")
two_rows_before=$(journal | grep -F "toast: open slot=" | grep -cF " rows=2 ")
held_before=$(journal_count "toast: held (new row while up)")
first_id=$(notify_send 'First of two' 'the first of two' 10000)
if [[ -n "$first_id" ]] && wait_for_journal_more "toast: open slot=" 5 "$opens_before"; then
    line=$(journal | grep -F "toast: open slot=" | tail -1)
    if [[ "$line" == *" rows=1 ids=$first_id"* ]]; then
        pass "the first notification opened a one-row toast: ${line#*toast: }"
    else
        fail "the first toast is not one row of id $first_id: ${line#*toast: }"
    fi
else
    fail "no toast for the first of two (id ${first_id:-none})"
fi
wait_for_journal "layout: applet-caustics-notify-$first_id " 5 ||
    fail "no 'layout: applet-caustics-notify-$first_id' row in the toast"
sleep 3
second_id=$(notify_send 'Second of two' 'the second of two' 10000)
if [[ -n "$second_id" ]] && wait_for_journal_more "toast: held (new row while up)" 5 "$held_before"; then
    line=$(journal | grep -F "toast: held (new row while up)" | tail -1)
    if [[ "$line" == *"row=$second_id"* ]]; then
        pass "the second waits for the first toast to end: ${line#*toast: }"
    else
        fail "a held line, but not for id $second_id: ${line#*toast: }"
    fi
else
    fail "no 'toast: held (new row while up)' for the second (id ${second_id:-none})"
fi
sleep 1
if (($(journal | grep -F "toast: open slot=" | grep -cF " rows=2 ") == two_rows_before)); then
    pass "the toast still shows one row, not two"
else
    fail "a toast opened with two rows: $(journal | grep -F 'toast: open slot=' | tail -1)"
fi
requested_before=$(journal_count "popup: requested kind=toast")
first_closed_before=$(journal_count "caustics-notify: closed id=$first_id reason=2")
if popup_click "applet-caustics-notify-$first_id" 0.5 toast; then
    if wait_for_journal_more "caustics-notify: closed id=$first_id reason=2" 5 "$first_closed_before"; then
        pass "the click on the first row named the first id: closed id=$first_id reason=2"
    else
        fail "no 'caustics-notify: closed id=$first_id reason=2' after the click on its row"
    fi
    if (($(journal_count "caustics-notify: closed id=$second_id ") == 0)); then
        pass "and not the second (id $second_id)"
    else
        fail "the click closed the second notification ($second_id) too"
    fi
    # The second, held and never shown, is owed its own toast now.
    if wait_for_journal_more "popup: requested kind=toast" 3 "$requested_before"; then
        line=$(journal | grep -F "toast: open slot=" | tail -1)
        # The ids field whole: a substring test would find "1" in "rows=1".
        if [[ "${line##* ids=}" == "$second_id" ]]; then
            pass "the second, held behind the clicked toast, toasts alone after it: ${line#*toast: }"
        else
            fail "the toast after the click is not the second alone: ${line#*toast: }"
        fi
    else
        fail "no toast for the second within 3 s of the click (a row held behind a toast is owed its own)"
    fi
    if journal | jgrep -F "toast: shown slot="; then
        pass "the clicked toast said what it showed: $(journal | grep -F 'toast: shown slot=' | tail -1 | sed -n 's/.*toast: shown/toast: shown/p')"
    else
        fail "no 'toast: shown slot=' when the clicked toast ended"
    fi
    line=$(journal | grep -F "caustics-notify: published" | tail -1)
    if [[ "$line" == *"unread=2"* ]]; then
        pass "both are still unread, and the second asks for its toast: ${line#*caustics-notify: }"
    else
        fail "the daemon after the click: ${line#*caustics-notify: }"
    fi
fi

# Do-not-disturb: the history still fills, the bit is never set, and no toast.
if open_bell_menu && popup_click "applet-caustics-notify-$NOTIFY_DND_ROW"; then
    if wait_for_journal "dnd=1" 10; then
        pass "do-not-disturb is on"
    else
        fail "the do-not-disturb row did not turn it on"
    fi
fi
sleep 1
toasts_before=$(journal_count "popup: requested kind=toast")
published_before=$(journal_count "caustics-notify: published")
quiet_id=$(notify_send 'Quiet from the test' 'sent under do-not-disturb' 5000)
if wait_for_journal_more "caustics-notify: published" 10 "$published_before"; then
    line=$(journal | grep -F "caustics-notify: published" | tail -1)
    if [[ "$line" == *"dnd=1 show=0"* ]]; then
        pass "the daemon kept it without the bit: ${line#*caustics-notify: }"
    else
        fail "the daemon's frame under do-not-disturb: ${line#*caustics-notify: }"
    fi
else
    fail "no frame from the daemon for the second notification (id ${quiet_id:-none})"
fi
sleep 3
if (($(journal_count "popup: requested kind=toast") == toasts_before)); then
    pass "do-not-disturb: no toast"
else
    fail "a toast opened under do-not-disturb"
fi

# Leave the daemon as the notification step found it: do-not-disturb off, the history empty.
dnd_off_before=$(journal_count "dnd=0")
if open_bell_menu && popup_click "applet-caustics-notify-$NOTIFY_DND_ROW"; then
    if wait_for_journal_more "dnd=0" 10 "$dnd_off_before"; then
        pass "do-not-disturb is off again"
    else
        fail "do-not-disturb did not turn off again"
    fi
fi
sleep 1
empties_before=$(journal_count "unread=0 entries=0")
if open_bell_menu && popup_click "applet-caustics-notify-$NOTIFY_CLEAR_ROW"; then
    if wait_for_journal_more "unread=0 entries=0" 10 "$empties_before"; then
        pass "the history is clear again for the steps after this one"
    else
        fail "clear-all left notifications in the history"
    fi
fi
if journal | jgrep -E "sidecar caustics-notify: .*(exited|restarting)"; then
    fail "caustics-notify exited or restarted"
else
    pass "caustics-notify stayed up through the toasts"
fi

# A click on a row with `RFLAG_ACTIVATION` gets an `xdg_activation_v1` token
# sent down the pipe as `REQ_ACTIVATION`; the daemon emits `ActivationToken`
# then `ActionInvoked`. If niri grants no token (its policy), the click must
# still invoke within the daemon's 100 ms grace.

say "9f. a notification click carries an activation token"
# The greeting asks for nothing: activation tokens are requested per click
# (`REQ_ACTIVATION`), never at the greeting.
if journal | jgrep -F "sidecar caustics-notify: hello from" &&
    ! journal | grep -F "sidecar caustics-notify: hello from" | tail -1 | jgrep -F "wants activation tokens"; then
    pass "caustics-notify greeted, and its greeting asks for nothing"
else
    fail "caustics-notify's greeting is missing or still asks for activation tokens: $(journal |
        grep -F 'sidecar caustics-notify: hello from' | tail -1 | sed -n 's/.*feeders::pipe: //p')"
fi
if journal | jgrep -F "bound xdg_activation_v1"; then
    pass "the bar bound xdg_activation_v1"
else
    fail "the bar did not bind xdg_activation_v1: $(journal | grep -F 'xdg_activation_v1' | tail -1)"
fi

if [[ "$have_gdbus" == yes ]]; then
    # `busctl monitor`, not `gdbus monitor`: the signals are directed to the
    # notifier, and only a monitor sees a directed signal. One JSON per line.
    on_target 20 "$guest_bus; rm -f \$T_HOME/notify-activation.txt
        (setsid timeout 120 busctl --user monitor --json=short \
            --match \"type='signal',interface='org.freedesktop.Notifications'\" \
            >\$T_HOME/notify-activation.txt 2>&1 </dev/null &) ; true" >/dev/null
    sleep 1
    reply=$(on_target 20 "$guest_bus; gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify \
        -- caustics-bar-test 0 dialog-information 'Focus from the test' 'click to invoke the default action' \"['default', 'Open']\" '@a{sv} {}' -1" 2>&1)
    focus_id=$(sed -n 's/^(uint32 \([0-9]*\),)$/\1/p' <<<"$reply")
else
    reply=$(on_target 20 "$guest_bus; busctl --user call org.freedesktop.Notifications \
        /org/freedesktop/Notifications org.freedesktop.Notifications Notify \
        susssasa{sv}i caustics-bar-test 0 dialog-information 'Focus from the test' 'click to invoke the default action' 2 default Open 0 -1" 2>&1)
    focus_id=$(sed -n 's/^u \([0-9]*\)$/\1/p' <<<"$reply")
fi
if [[ -n "$focus_id" ]]; then
    pass "Notify with a default action answered with id $focus_id"
else
    fail "Notify with a default action did not answer with an id: $reply"
fi

if [[ -n "$focus_id" ]]; then
    # The toast laid this row out first.
    focus_rows_before=$(journal_count "layout: applet-caustics-notify-$focus_id ")
    invoked_before=$(journal_count "caustics-notify: action id=$focus_id ")
    tokens_before=$(journal_count "activation: token for row=$focus_id ")
    notokens_before=$(journal_count "activation: no token")
    if open_bell_menu &&
        wait_for_journal_more "layout: applet-caustics-notify-$focus_id " 10 "$focus_rows_before"; then
        sleep 1
        if popup_click "applet-caustics-notify-$focus_id"; then
            if wait_for_journal_more "caustics-notify: action id=$focus_id " 10 "$invoked_before"; then
                pass "the click invoked the default action: $(journal |
                    grep -F "caustics-notify: action id=$focus_id " | tail -1 | sed -n 's/.*caustics-notify: /caustics-notify: /p')"
            else
                fail "no 'caustics-notify: action id=$focus_id …' after the click"
            fi
            granted=0
            if (($(journal_count "activation: token for row=$focus_id ") > tokens_before)); then
                granted=1
                pass "the embassy got a token and sent it: $(journal |
                    grep -F "activation: token for row=$focus_id " | tail -1 | sed -n 's/.*activation: /activation: /p')"
            elif (($(journal_count "activation: no token") > notokens_before)); then
                note "the embassy got no token: $(journal | grep -F 'activation: no token' | tail -1 |
                    sed -n 's/.*activation: /activation: /p')"
            else
                note "no 'activation:' witness from the embassy within the click's wait"
            fi
            if [[ "$have_gdbus" == yes ]]; then
                sleep 1
                monitor=$(on_target 20 'cat "$T_HOME/notify-activation.txt"')
                at_token=$(grep -n '"member":"ActivationToken"' <<<"$monitor" | grep -F "\"data\":[$focus_id," | head -1 | cut -d: -f1)
                at_invoked=$(grep -n '"member":"ActionInvoked"' <<<"$monitor" | grep -F "\"data\":[$focus_id,\"default\"]" | head -1 | cut -d: -f1)
                if [[ -z "$at_invoked" ]]; then
                    fail "the monitor saw no ActionInvoked($focus_id, 'default'): $(tail -3 <<<"$monitor" | cut -c1-160 | tr '\n' ' ')"
                elif [[ -n "$at_token" ]] && ((at_token < at_invoked)); then
                    pass "the bus saw ActivationToken($focus_id, …) before ActionInvoked($focus_id, 'default')"
                elif [[ -n "$at_token" ]]; then
                    fail "ActivationToken($focus_id) came after ActionInvoked (lines $at_token, $at_invoked)"
                elif ((granted)); then
                    fail "the embassy sent a token but the bus saw no ActivationToken($focus_id)"
                else
                    pass "no token was granted and ActionInvoked($focus_id, 'default') still went out"
                fi
                # A broadcast has no `destination` key.
                if [[ -n "$at_invoked" ]]; then
                    if sed -n "${at_invoked}p" <<<"$monitor" | grep -q '"destination":":'; then
                        pass "ActionInvoked($focus_id) was directed to the notifier's connection, not broadcast"
                    else
                        fail "ActionInvoked($focus_id) was a broadcast; caustics-notify directs it to the notifier's connection"
                    fi
                fi
            fi
        fi
    else
        fail "no 'layout: applet-caustics-notify-$focus_id' row in the bell's menu"
    fi
fi
if journal | jgrep -E "sidecar caustics-notify: .*(exited|restarting)"; then
    fail "caustics-notify exited or restarted"
else
    pass "caustics-notify stayed up through the activation"
fi
on_target 20 'rm -f "$T_HOME/notify-activation.txt"' >/dev/null

say "9g. the clock's click opens this month's calendar"
# The clock's click opens this month as a grabbed menu. The `calendar: open`
# witness is checked against the target's `date` in the C locale (which spells
# month and weekday as the caustic's `MONTHS`/`WEEKDAYS` do).
cal_rect=($(layout_rect clock))
if [[ ${#cal_rect[@]} -ne 4 || -z "$bar_origin_y" ]]; then
    fail "no clock rect (or no bar origin) to click for the calendar"
else
    cal_x=$(python3 -c "print(round(${cal_rect[0]} + ${cal_rect[2]} / 2))")
    cal_y=$(python3 -c "print(round($bar_origin_y + ${cal_rect[1]} + ${cal_rect[3]} / 2))")
    # "Oct 2026 Thu 31 10": month, year, the 1st's weekday, length, today.
    cal_facts=($(on_target 20 'first=$(date +%Y-%m-01); LC_ALL=C date -d "$first" "+%b %Y %a"; \
        date -d "$first +1 month -1 day" +%-d; date +%-d' | tr '\n' ' '))
    cal_opens=$(journal_count "calendar: open")
    cal_menus=$(journal_count "popup: requested kind=menu")
    cal_mapped=$(journal_count "popup: open kind=menu")
    cal_days=$(journal_count "layout: calendar-day-")
    cal_closed=$(journal_count "popup: closed")
    park_pointer
    shot calendar-before.png
    click_at "$cal_x" "$cal_y"
    if wait_for_journal_more "popup: requested kind=menu" 10 "$cal_menus"; then
        line=$(journal | grep -F "popup: requested kind=menu" | tail -1)
        if [[ "$line" == *"grab=true"* ]]; then
            pass "the clock's click asked for a menu with a grab: ${line#*popup: }"
        else
            fail "the calendar was asked for without a grab: ${line#*popup: }"
        fi
    else
        fail "no 'popup: requested kind=menu' within 10 s of a click on the clock"
    fi
    if [[ ${#cal_facts[@]} -eq 5 ]] && wait_for_journal_more "calendar: open" 5 "$cal_opens"; then
        line=$(journal | grep -F "calendar: open" | tail -1)
        want="calendar: open ${cal_facts[0]} ${cal_facts[1]} (${cal_facts[3]} days, the 1st a ${cal_facts[2]},"
        if [[ "$line" == *"$want"* ]]; then
            pass "the caustic's month is the target's: ${line#*calendar: }"
        else
            fail "the caustic's month '${line#*calendar: }' is not the target's '${cal_facts[*]}'"
        fi
    else
        fail "no 'calendar: open' line (target date: '${cal_facts[*]}')"
    fi
    cal_pos=()
    if wait_for_journal_more "popup: open kind=menu" 5 "$cal_mapped"; then
        open_line=$(journal | grep -F "popup: open kind=menu" | tail -1)
        cal_pos=($(sed -n 's/.* px=\([0-9]*\)x\([0-9]*\) at \([0-9-]*\),\([0-9-]*\).*/\1 \2 \3 \4/p' \
            <<<"$open_line"))
        pass "the calendar mapped: ${open_line#*popup: }"
    else
        fail "no 'popup: open kind=menu' for the calendar"
    fi
    if [[ ${#cal_facts[@]} -eq 5 ]] &&
        wait_for_journal "layout: calendar-${cal_facts[0]}-${cal_facts[1]} target=popup" 5; then
        pass "the title is laid out: $(journal |
            grep -F "layout: calendar-${cal_facts[0]}-${cal_facts[1]} target=popup" | tail -1 |
            sed -n 's/.*layout: //p')"
    else
        fail "no 'layout: calendar-${cal_facts[0]:-?}-${cal_facts[1]:-?} target=popup' line"
    fi
    if [[ ${#cal_facts[@]} -eq 5 ]] &&
        wait_for_journal_more "layout: calendar-day-" 5 $((cal_days + cal_facts[3] - 1)); then
        pass "${cal_facts[3]} days laid out, today among them: $(journal |
            grep -F "layout: calendar-day-${cal_facts[4]} target=popup" | tail -1 |
            sed -n 's/.*layout: //p')"
    else
        fail "fewer than ${cal_facts[3]:-?} 'layout: calendar-day-' lines after the click"
    fi
    sleep 0.5
    shot calendar.png
    if [[ ${#cal_pos[@]} -eq 4 ]]; then
        cx=$((cal_pos[2] * 2))
        cy=$((bar_origin_y + cal_pos[3] * 2))
        moved=$(png_pixel_diff "$SHOTS/calendar-before.png" "$SHOTS/calendar.png" \
            "$cx" "$cy" "${cal_pos[0]}" "${cal_pos[1]}")
        area=$((cal_pos[0] * cal_pos[1]))
        if ((moved > area / 10)); then
            pass "the calendar drew: $moved of $area px in its rect differ from before the click"
        else
            fail "the calendar's rect ($cx,$cy ${cal_pos[0]}x${cal_pos[1]}) looks like the background ($moved of $area px differ)"
        fi
    else
        fail "no calendar position to compare texels in"
    fi
    # The second click: the same clock, which closes the menu it opened.
    click_at "$cal_x" "$cal_y"
    if wait_for_journal_more "popup: closed" 5 "$cal_closed"; then
        pass "a second click on the clock closed the calendar"
    else
        fail "the calendar stayed open after a second click on the clock"
    fi
    if (($(journal_count "calendar: open") == cal_opens + 1)); then
        pass "and did not open it again"
    else
        fail "the second click opened the calendar again"
    fi
fi

say "10. every sidecar starts; caustics-glyphs supplies the font; caustics-sysmon samples"
# Witnesses are prefixed by the sidecar's file stem (docs/abi.md, *Lifecycle*).
if wait_for_journal "sidecar caustics-sysmon: started pid=" 20; then
    sysmon_started=$(journal | grep -F 'sidecar caustics-sysmon: started pid=' | tail -1)
    pass "the pipe feeder started the sidecar: $sysmon_started"
else
    fail "no 'sidecar caustics-sysmon: started pid=N'; the bar did not find caustics-sysmon"
fi
# ...and the rest of the compiled-in set.
started_n=$(journal |
    grep -cE 'sidecar (caustics-sysmon|caustics-tray|caustics-power|caustics-audio|caustics-network|caustics-bluetooth|caustics-notify|caustics-apps|caustics-glyphs|caustics-reload): started pid=')
if [[ "${started_n:-0}" -ge 10 ]]; then
    pass "the compiled-in set is ten, and all ten started"
else
    fail "only $started_n of the ten compiled-in sidecars started ($(journal |
        grep -oE 'sidecar [^:]*: started pid=[0-9]*' | tr '\n' ' '))"
fi
# The font reaches the bar as bytes: the sidecar's witness, then the bar's.
if wait_for_journal "glyphs: font sent:" 30; then
    pass "caustics-glyphs sent the font: $(journal |
        grep -F 'glyphs: font sent:' | tail -1 | sed -n 's/.*glyphs: /glyphs: /p')"
else
    fail "no 'glyphs: font sent: N frames'; nothing filled the caustic's font buffers"
fi
if wait_for_journal "font: " 30 && journal | jgrep -E 'font: .* glyphs at [0-9]+ upem'; then
    pass "and the bar took its facts from the Font frame: $(journal |
        grep -oE 'font: .* upem, cap [0-9.]+ em, icons from [0-9]+\) from caustics-glyphs' | tail -1)"
else
    fail "no 'font: <family> (N glyphs at M upem …)'; the bar never saw a Font frame"
fi
if wait_for_journal "text: the first curves landed" 30; then
    first_curves=$(journal | grep -F 'text: the first curves landed' | tail -1 |
        sed -n 's/.*text: \(the first curves landed [0-9]* ms\).*/\1/p')
    pass "$first_curves after the bar started"
else
    fail "no 'text: the first curves landed N ms after start'; no Bytes frame reached a buffer"
fi
if journal | jgrep -E 'fonts: .*(warm|cold)'; then
    fail "the bar is loading a font itself; fonts are caustics-glyphs' job"
else
    pass "the bar loaded no font of its own"
fi
# No version word in the line (docs/abi.md, *The ABI hash*).
if wait_for_journal "sidecar caustics-sysmon: samples," 20; then
    frame_line=$(journal | grep -F 'sidecar caustics-sysmon: samples,' | tail -1)
    charts_n=$(printf '%s' "$frame_line" | sed -n 's/.*samples, \([0-9]*\) charts.*/\1/p')
    if [[ "${charts_n:-0}" -eq 6 ]]; then
        pass "the first body carries the default six: $frame_line"
    else
        fail "the body carries ${charts_n:-no} charts, expected 6 ($frame_line)"
    fi
else
    fail "no 'sidecar caustics-sysmon: samples, N charts'; nothing valid came off the pipe"
fi
# The sidecar's own stderr arrives on its own schedule, so wait for it.
wait_for_journal "sysmon: charts: cpu cpu-temp mem net disk gpu" 10
log="$(journal)"
if grep -qF 'sysmon: charts: cpu cpu-temp mem net disk gpu' <<<"$log"; then
    pass "the sidecar picked its compiled-in six with no arguments from the bar"
else
    fail "no 'sysmon: charts: …' line ($(printf '%s' "$log" | grep -F 'sysmon: charts' | tail -1))"
fi
if grep -qF 'no AMD or NVIDIA device' <<<"$log"; then
    pass "$(printf '%s' "$log" | grep -F 'no AMD or NVIDIA device' | tail -1)"
else
    note "the target reported a GPU: $(printf '%s' "$log" | grep -E 'sysmon: gpu' | tail -1)"
fi
printf '%s' "$log" | grep -E 'sysmon: ' | head -6 | while read -r l; do note "$l"; done

say "11. the charts lay out, keeping a prefix when space is short"
if wait_for_journal "charts: 6 laid out" 20; then
    pass "the item table holds six charts"
else
    fail "no 'charts: 6 laid out' ($(journal | grep -F 'charts:' | tail -1))"
fi
# At 640 logical px the drop rule may correctly place none (the left wing is
# most of the width). So the tunables make room: no dock, the clock in the left
# wing, the strip alone in the centre (trimming the right wing would drop
# APPLET_OTHER, which the extra-sidecar step needs). The edit stays until the
# garbage-sidecar step restores the snapshot.
if journal | jgrep -E 'layout: chart-'; then
    pass "the caustic placed a chart with the shipped tunables"
else
    note "no chart placed with the shipped tunables: the drop rule, correctly, \
at 640 logical px with the seeded dock"
fi
# Park first: a chart landing under the resting pointer would open its preview.
park_pointer
reloads_before=$(journal_count "$SWAPPED")
# Tunables files are written whole, by a rename: an in-place `sed -i` on the
# watched file lets caustics-reload compile it half-written.
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    sed -i -e "s/^public static const uint LEFT_ORDER\[\] = .*/public static const uint LEFT_ORDER[] = { ITEM_OVERVIEW, ITEM_CLOCK };/" \
        -e "s/^public static const uint CENTER_ORDER\[\] = .*/public static const uint CENTER_ORDER[] = { ITEM_CHART };/" \
        $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "$SWAPPED" 60 "$reloads_before"; then
    pass "a dock-less LEFT_ORDER compiled and swapped in"
else
    fail "the tunables that make room for the charts did not reload"
fi
if wait_for_journal "layout: chart-cpu" 20; then
    pass "and the strip went down: $(journal | grep -F 'layout: chart-cpu' | tail -1 |
        sed -n 's/.*layout: //p')"
else
    fail "still no 'layout: chart-cpu' with the wing out of the way"
fi
# How many fit depends on the target's right wing; the rule promises a
# *prefix* (charts are shed from the end), and at least one chart. A count
# would read the first frame, laid out at the 14 px placeholder size.
placed=()
for kind in cpu temp mem net disk gpu; do
    rect=($(layout_rect "chart-$kind"))
    [[ ${#rect[@]} -eq 4 ]] && placed+=("chart-$kind")
done
expect=(chart-cpu chart-temp chart-mem chart-net chart-disk chart-gpu)
if [[ ${#placed[@]} -ge 1 && "${placed[*]}" == "${expect[*]:0:${#placed[@]}}" ]]; then
    pass "the drop rule kept a prefix, ${#placed[@]} of six: ${placed[*]}"
elif [[ ${#placed[@]} -ge 1 ]]; then
    fail "${#placed[@]} placed but not the first ${#placed[@]}: ${placed[*]}"
else
    fail "no chart placed at all"
fi
cpu_rect=($(layout_rect chart-cpu))
shot charts.png

say "11b. the chart numbers open on hover and on click"
# Resting on the CPU chart opens a grabless preview of one line per chart
# (`layout: numbers-cpu 7 % …`); leaving ends it; a click opens the same as a
# grabbed menu; a second click closes it and the hover does not reopen it.
if [[ ${#cpu_rect[@]} -ne 4 || -z "$bar_origin_y" ]]; then
    fail "no CPU chart rect (or no bar origin) to rest the pointer on"
else
    num_x=$(python3 -c "print(round(${cpu_rect[0]} + ${cpu_rect[2]} / 2))")
    num_y=$(python3 -c "print(round($bar_origin_y + ${cpu_rect[1]} + ${cpu_rect[3]} / 2))")
    park_pointer
    num_previews=$(journal_count "popup: requested kind=preview")
    num_opens=$(journal_count "numbers: open kind=preview chart=cpu lines=6")
    num_cpu=$(journal_count "layout: numbers-cpu ")
    num_left=$(journal_count "numbers: closed (left)")
    pointer_to "$num_x" "$num_y"
    if wait_for_journal_more "popup: requested kind=preview" 10 "$num_previews"; then
        line=$(journal | grep -F "popup: requested kind=preview" | tail -1)
        if [[ "$line" == *"grab=false"* ]]; then
            pass "resting on the CPU chart asked for a preview without a grab: ${line#*popup: }"
        else
            fail "the numbers' preview was asked for with a grab: ${line#*popup: }"
        fi
    else
        fail "no 'popup: requested kind=preview' within 10 s of resting on the CPU chart"
    fi
    if wait_for_journal_more "numbers: open kind=preview chart=cpu lines=6" 5 "$num_opens"; then
        pass "the caustic opened six lines for the CPU chart"
    else
        fail "no 'numbers: open kind=preview chart=cpu lines=6' ($(journal |
            grep -F 'numbers:' | tail -1))"
    fi
    if wait_for_journal_more "layout: numbers-cpu " 30 "$num_cpu"; then  # 30 s: lavapipe JITs the popup's first frame
        line=$(journal | grep -F "layout: numbers-cpu " | grep -v 'rect=gone' | tail -1)
        if [[ "$line" =~ layout:\ numbers-cpu\ [0-9]+\ %\ target=popup ]]; then
            pass "the CPU's line is laid out with its percent: ${line#*layout: }"
        else
            fail "the CPU's line carries no percent: ${line#*layout: }"
        fi
    else
        fail "no 'layout: numbers-cpu' line in the preview"
    fi
    for kind in temp mem net disk gpu; do
        if journal | jgrep -F "layout: numbers-$kind "; then
            note "$(journal | grep -F "layout: numbers-$kind " | grep -v 'rect=gone' | tail -1 |
                sed -n 's/.*layout: //p')"
        else
            fail "no 'layout: numbers-$kind' line: the numbers are not all six"
        fi
    done
    shot numbers-preview.png
    park_pointer
    if wait_for_journal_more "numbers: closed (left)" 5 "$num_left"; then
        pass "leaving the strip ended the preview"
    else
        fail "the numbers' preview outlived the pointer leaving the strip"
    fi

    # Pressed at once (no `click_at` rest), so no preview opens first.
    num_menus=$(journal_count "popup: requested kind=menu")
    num_menu_opens=$(journal_count "numbers: open kind=menu chart=cpu lines=6")
    num_mapped=$(journal_count "popup: open kind=menu")
    num_lines=$(journal_count "layout: numbers-")
    num_closed=$(journal_count "popup: closed")
    num_clicked=$(journal_count "numbers: closed (click)")
    shot numbers-before.png
    pointer_to "$num_x" "$num_y"; sleep 0.1; button true; sleep 0.1; button false; sleep 0.6
    if wait_for_journal_more "popup: requested kind=menu" 10 "$num_menus"; then
        line=$(journal | grep -F "popup: requested kind=menu" | tail -1)
        if [[ "$line" == *"grab=true"* ]]; then
            pass "a click on the strip asked for a menu with a grab: ${line#*popup: }"
        else
            fail "the numbers' menu was asked for without a grab: ${line#*popup: }"
        fi
    else
        fail "no 'popup: requested kind=menu' within 10 s of a click on the CPU chart"
    fi
    if wait_for_journal_more "numbers: open kind=menu chart=cpu lines=6" 5 "$num_menu_opens"; then
        pass "the click opened the same six lines as a menu"
    else
        fail "no 'numbers: open kind=menu chart=cpu lines=6' ($(journal |
            grep -F 'numbers:' | tail -1))"
    fi
    num_pos=()
    if wait_for_journal_more "popup: open kind=menu" 5 "$num_mapped"; then
        open_line=$(journal | grep -F "popup: open kind=menu" | tail -1)
        num_pos=($(sed -n 's/.* px=\([0-9]*\)x\([0-9]*\) at \([0-9-]*\),\([0-9-]*\).*/\1 \2 \3 \4/p' \
            <<<"$open_line"))
        pass "the numbers' menu mapped: ${open_line#*popup: }"
    else
        fail "no 'popup: open kind=menu' for the numbers"
    fi
    if wait_for_journal_more "layout: numbers-" 5 $((num_lines + 5)); then
        pass "six rows laid out in the menu: $(journal | grep -F 'layout: numbers-cpu ' |
            grep -v 'rect=gone' | tail -1 | sed -n 's/.*layout: //p')"
    else
        fail "fewer than six new 'layout: numbers-' lines after the click ($(($(journal_count \
            "layout: numbers-") - num_lines)))"
    fi
    sleep 0.5
    shot numbers-menu.png
    if [[ ${#num_pos[@]} -eq 4 ]]; then
        nx=$((num_pos[2] * 2))
        ny=$((bar_origin_y + num_pos[3] * 2))
        moved=$(png_pixel_diff "$SHOTS/numbers-before.png" "$SHOTS/numbers-menu.png" \
            "$nx" "$ny" "${num_pos[0]}" "${num_pos[1]}")
        area=$((num_pos[0] * num_pos[1]))
        if ((moved > area / 10)); then
            pass "the numbers drew: $moved of $area px in their rect differ from before the click"
        else
            fail "the numbers' rect ($nx,$ny ${num_pos[0]}x${num_pos[1]}) looks like the background ($moved of $area px differ)"
        fi
    else
        fail "no numbers position to compare texels in"
    fi

    # A second click closes it; resting there does not reopen the preview.
    num_previews=$(journal_count "popup: requested kind=preview")
    pointer_to "$num_x" "$num_y"; sleep 0.1; button true; sleep 0.1; button false
    if wait_for_journal_more "popup: closed" 5 "$num_closed" &&
        wait_for_journal_more "numbers: closed (click)" 5 "$num_clicked"; then
        pass "a second click on the strip closed the numbers"
    else
        fail "the numbers stayed open after a second click on the strip"
    fi
    sleep 2
    if (($(journal_count "popup: requested kind=preview") == num_previews)); then
        pass "and resting on the strip after it did not open them again"
    else
        fail "the hover reopened the numbers the second click had closed"
    fi
    park_pointer
fi

say "12. a killed sidecar restarts; an absent one is one line and no charts"
sysmon_pid=$(journal | grep -F 'sidecar caustics-sysmon: started pid=' | tail -1 |
    sed -n 's/.*pid=\([0-9]*\).*/\1/p')
if [[ -z "$sysmon_pid" ]]; then
    fail "no sidecar pid in the journal to kill"
else
    starts_before=$(journal_count "sidecar caustics-sysmon: started pid=")
    # By pid, from the journal; never by name.
    note "killing the sidecar, pid $sysmon_pid"
    target_kill "$sysmon_pid" >/dev/null
    if wait_for_journal_more "sidecar caustics-sysmon: started pid=" 20 "$starts_before"; then
        pass "the feeder brought it back: $(journal |
            grep -F 'sidecar caustics-sysmon: started pid=' | tail -1)"
    else
        fail "the sidecar did not come back within 20 s of being killed"
    fi
    exited=$(journal | grep -F 'sidecar caustics-sysmon: exited' | tail -1)
    if printf '%s' "$exited" | grep -qE 'signal|SIGTERM|15'; then
        pass "and the witness names the signal: $exited"
    else
        note "the exit witness does not name a signal: ${exited:-none}"
    fi
    # The table never changed shape: six charts are still laid out.
    if wait_for_journal "charts: 6 laid out" 20; then
        pass "six charts through the restart"
    else
        fail "the chart items did not survive the sidecar's restart"
    fi
fi

# A sidecar not installed at all (`$CAUSTICS_SIDECARS` *replaces* the set).
bar_env sysmon "CAUSTICS_SIDECARS=no-such-binary"
restart_bar "CAUSTICS_SIDECARS=no-such-binary"
if wait_for_journal "absent: no-such-binary" 30; then
    pass "an absent sidecar is one 'absent: no-such-binary' line"
else
    fail "no 'absent: no-such-binary' with CAUSTICS_SIDECARS pointing at nothing"
fi
if journal | jgrep -E 'layout: chart-'; then
    fail "a chart was laid out with no sidecar to fill it"
else
    pass "no chart items at all, the way an absent battery has none"
fi
assert_contains "and the bar is still up" "$(bar_active)" active
shot no-sidecar.png

bar_env_clear sysmon
restart_bar "the sidecar restored"
if wait_for_journal "charts: 6 laid out" 30; then
    pass "six charts again"
else
    fail "the charts did not come back after the drop-in was removed"
fi

say "14. a sidecar tick redraws once a second without stalling or animating"
# A settled bar logs nothing per frame, so the evidence is the picture: the
# chart's columns move one step a tick as samples arrive, so ten seconds
# apart the same rect does not hold the same texels. Counted pixel by pixel:
# an idle guest's flat chart can shift a column and keep the same rounded
# mean over the whole rect (seen on the VM target), so the mean is only noted.
stalls_before=$(journal_count "sidecar caustics-sysmon: stalled")
animating_before=$(journal_count "redraw: still animating")
cpu_rect=($(layout_rect chart-cpu))
if [[ ${#cpu_rect[@]} -eq 4 && -n "$bar_origin_y" ]]; then
    probe_x=${cpu_rect[0]}
    probe_y=$(python3 -c "print(round($bar_origin_y + ${cpu_rect[1]}))")
    probe_w=${cpu_rect[2]}
    probe_h=${cpu_rect[3]}
    shot scroll-before.png
    before_rgb=$(png_mean "$SHOTS/scroll-before.png" "$probe_x" "$probe_y" "$probe_w" "$probe_h")
    sleep 10
    shot scroll-after.png
    after_rgb=$(png_mean "$SHOTS/scroll-after.png" "$probe_x" "$probe_y" "$probe_w" "$probe_h")
    moved=$(png_pixel_diff "$SHOTS/scroll-before.png" "$SHOTS/scroll-after.png" "$probe_x" "$probe_y" "$probe_w" "$probe_h")
    note "chart-cpu mean RGB over ten quiet seconds: [$before_rgb] -> [$after_rgb]; $moved px of its rect differ"
    if [[ "${moved:-0}" -gt 0 ]]; then
        pass "the chart moved with nothing else happening: the frames keep arriving ($moved px)"
    else
        fail "the chart froze over ten seconds (no pixel of its rect changed; mean [$before_rgb])"
    fi
else
    sleep 10
    note "no chart rect to watch; only the journal's evidence is checked"
fi
if [[ "$(journal_count 'sidecar caustics-sysmon: stalled')" -gt "$stalls_before" ]]; then
    fail "the sidecar stalled: $(journal |
        grep -F 'sidecar caustics-sysmon: stalled' | tail -1)"
else
    pass "no 'sidecar caustics-sysmon: stalled' in ten seconds (no gap over 5 s between bodies)"
fi
if [[ "$(journal_count 'redraw: still animating')" -gt "$animating_before" ]]; then
    fail "the bar never settled: $(journal | grep -F 'redraw: still animating' | tail -1)"
else
    pass "no stall warning: a sidecar tick is a frame, not an animation"
fi
if journal | grep -F 'redraw:' | tail -1 | grep -qF 'settled'; then
    pass "and the newest redraw line says settled"
else
    note "the newest redraw line is $(journal | grep -F 'redraw:' | tail -1)"
fi

say "15. the power menu opens from the sidecar; its rows perform under the dry run"
# The sidecar inherits the bar's environment, which arms its dry run: every
# action is one line on its stderr.
bar_env power "CAUSTICS_POWER_DRY_RUN=1"
restart_bar "CAUSTICS_POWER_DRY_RUN=1"
wait_for_journal "sidecar caustics-power: rows, applet=5" 30

power_rect=($(layout_rect applet-caustics-power))
if [[ ${#power_rect[@]} -ne 4 ]]; then
    fail "no 'layout: applet-caustics-power' rect; the power button was not laid out"
else
    pass "the power button laid out: ${power_rect[*]}"
    power_cx=$(python3 -c "print(round(${power_rect[0]} + ${power_rect[2]} / 2))")
    power_cy=$(python3 -c "print(round(${power_rect[1]} + ${power_rect[3]} / 2))")
    click_at "$power_cx" $((bar_origin_y + power_cy))
    if wait_for_journal "popup: requested kind=menu" 8 && wait_for_journal "popup: open kind=menu" 8; then
        pass "the power click asked for a menu popup and it mapped"
    else
        fail "no menu popup from the power button"
    fi
    # Rows land over several frames: wait for the count.
    deadline=$((SECONDS + 20))
    while [[ "$(power_rows_now | grep -c .)" -lt 6 ]] && ((SECONDS < deadline)); do
        sleep 1
    done
    shot power-menu.png
    rows=$(power_rows_now)
    n=$(printf '%s\n' "$rows" | grep -c .)
    # The six of `power/src/rows.rs`, sorted by name.
    want="applet-caustics-power-2 applet-caustics-power-3 applet-caustics-power-4 applet-caustics-power-5 applet-caustics-power-6 applet-caustics-power-7"
    if [[ "$(printf '%s' "$rows" | tr '\n' ' ' | xargs)" == "$want" ]]; then
        pass "six rows: five items and one separator, all the sidecar's"
    else
        fail "the popup holds $n rows: $(printf '%s' "$rows" | tr '\n' ' ')"
    fi

    # The menu fits the screen, so the wheel moves nothing.
    lock_rect=($(layout_rect applet-caustics-power-2))
    if [[ ${#lock_rect[@]} -eq 4 ]]; then
        ppos=$(journal | grep -F 'popup: open kind=menu' | tail -1 |
            sed -n 's/.* at \([0-9-]*\),\([0-9-]*\).*/\1 \2/p')
        read -r ppx ppy <<<"$ppos"
        before=$(journal_count "layout: applet-caustics-power-")
        pointer_to "$(python3 -c "print(round(${ppx:-0} * 2 + ${lock_rect[0]} + ${lock_rect[2]} / 2))")" \
            "$(python3 -c "print(round($bar_origin_y + ${ppy:-0} * 2 + ${lock_rect[1]} + ${lock_rect[3]} / 2))")"
        sleep 0.4
        for _ in 1 2 3; do button true wheel-down; button false wheel-down; sleep 0.2; done
        sleep 1
        after=$(journal_count "layout: applet-caustics-power-")
        if [[ "$after" -eq "$before" && "$(power_rows_now | grep -c .)" -eq 6 ]]; then
            pass "the wheel over a menu that fits moves nothing"
        else
            fail "the wheel moved a menu that fits ($before -> $after layout lines)"
        fi
    fi

    # A leaf: it performs and the popup closes. Suspend is row 5.
    if popup_click applet-caustics-power-5; then
        if wait_for_journal "caustics-power: suspend: dry run" 8; then
            pass "$(journal | grep -F 'caustics-power: suspend: dry run' | tail -1 |
                sed -n 's/.*caustics-power: /caustics-power: /p')"
        else
            fail "no 'caustics-power: suspend: dry run' after clicking Suspend"
        fi
        if journal | tail -n 25 | grep -qF 'popup: closed'; then
            pass "and the popup closed behind it"
        else
            fail "the popup stayed open after a leaf row was clicked"
        fi
    fi
fi

say "16. Shut Down performs only its dry run"
if [[ ${#power_rect[@]} -eq 4 ]]; then
    # An open since the click (layout lines follow an open, so not a tail read).
    opens_before=$(journal_count "popup: open kind=menu")
    click_at "$power_cx" $((bar_origin_y + power_cy))
    if wait_for_journal_more "popup: open kind=menu" 8 "$opens_before"; then
        note "the menu is up again"
    fi
    sleep 1
    shot power-menu-again.png
    if popup_click applet-caustics-power-7; then
        if wait_for_journal "caustics-power: shutdown: dry run" 8; then
            pass "$(journal | grep -F 'caustics-power: shutdown: dry run' | tail -1 |
                sed -n 's/.*caustics-power: /caustics-power: /p')"
        else
            fail "no 'caustics-power: shutdown: dry run' after clicking Shut Down"
        fi
        if journal | tail -n 25 | grep -qF 'popup: closed'; then
            pass "and the popup closed"
        else
            fail "the popup stayed open after Shut Down"
        fi
        # The one thing that must never happen in an unattended run.
        if journal | jgrep -E 'caustics-power: shutdown: (ok|failed)'; then
            fail "the dry run did not hold: a real PowerOff was attempted"
        else
            pass "and nothing was actually performed"
        fi
    fi
fi

say "16b. the battery chip's menu is the state row and settings, nothing else"
# `fake_upower` on the session bus, given to the bar as its system bus (zbus
# honours `DBUS_SYSTEM_BUS_ADDRESS`). With no power profiles or logind there,
# the menu is the state row, a separator and "Power settings…".
fixture_start fake-upower '$T_FAKE_UPOWER'
fake_up=no
deadline=$((SECONDS + 15))
while ((SECONDS < deadline)); do
    if on_target 20 'bus_env; busctl --user --no-pager status org.freedesktop.UPower >/dev/null 2>&1'; then
        fake_up=yes
        break
    fi
    sleep 1
done
if [[ $fake_up == yes ]]; then
    pass "the fake UPower owns org.freedesktop.UPower on the session bus"
else
    fail "the fake UPower never took its name ($(fixture_log fake-upower | tr '\n' ' '))"
fi
bar_env power "CAUSTICS_POWER_DRY_RUN=1" "DBUS_SYSTEM_BUS_ADDRESS=$U_SESSION_BUS"
restart_bar "a battery on the session bus"
if wait_for_journal "caustics-power: published rev=" 30 &&
    wait_for_journal 'state="87% · 2 h 13 m until empty"' 20; then
    pass "$(journal | grep -F 'state="87% · 2 h 13 m until empty"' | tail -1 |
        sed -n 's/.*caustics-power: /caustics-power: /p')"
else
    fail "no battery with its state line ($(journal | grep -F 'caustics-power: published' |
        tail -1))"
fi
# A backlight (sysfs, not the bus) adds the brightness slider, row 10.
has_backlight=0
journal | jgrep -E 'caustics-power: backlight: ' && has_backlight=1
if ((has_backlight)); then
    note "this machine has a backlight: $(journal | grep -oE 'caustics-power: backlight: .*' | tail -1)"
    absents=("absent: power-profiles")
else
    absents=("absent: backlight" "absent: power-profiles")
fi
for absent in "${absents[@]}"; do
    if journal | jgrep -F "caustics-power: $absent"; then
        pass "caustics-power: $absent"
    else
        fail "no 'caustics-power: $absent' ($(journal | grep -F 'caustics-power: ' | tail -3 |
            tr '\n' ' '))"
    fi
done

# The battery menu's rows in the popup now (the chip, row 8, is on the bar).
battery_rows_now() {
    layout_placed_now 'applet-caustics-power-[0-9]+' popup
}
if wait_for_journal "layout: applet-caustics-power-8 target=bar" 20; then
    battery_rect=($(layout_rect applet-caustics-power-8))
    pass "the battery chip laid out: ${battery_rect[*]}"
    opens_before=$(journal_count "popup: open kind=menu")
    click_at "$(python3 -c "print(round(${battery_rect[0]} + ${battery_rect[2]} / 2))")" \
        "$((bar_origin_y + $(python3 -c "print(round(${battery_rect[1]} + ${battery_rect[3]} / 2))")))"
    if wait_for_journal_more "popup: open kind=menu" 8 "$opens_before"; then
        pass "the battery click opened a menu popup"
    else
        fail "no menu popup from the battery chip"
    fi
    deadline=$((SECONDS + 20))
    while [[ "$(battery_rows_now | grep -c .)" -lt $((3 + has_backlight)) ]] && ((SECONDS < deadline)); do
        sleep 1
    done
    shot battery-menu.png
    rows=$(battery_rows_now | tr '\n' ' ' | xargs)
    # 9 the state row, 12 the separator, 13 "Power settings…" (power/src/rows.rs);
    # no 10 (the slider) and no 11 or 256+ (the power modes).
    if ((has_backlight)) &&
        [[ "$rows" == "applet-caustics-power-10 applet-caustics-power-12 applet-caustics-power-13 applet-caustics-power-9" ]]; then
        pass "four rows: the state row, the brightness slider, a separator and the settings row"
    elif ((!has_backlight)) && [[ "$rows" == "applet-caustics-power-12 applet-caustics-power-13 applet-caustics-power-9" ]]; then
        pass "three rows: the state row, a separator and the settings row"
    else
        fail "the battery menu holds: $rows"
    fi
    # The settings row is a leaf: it performs (dry run) and the popup closes.
    if popup_click applet-caustics-power-13; then
        if wait_for_journal "caustics-power: settings: dry run" 8; then
            pass "caustics-power: settings: dry run"
        else
            fail "no 'caustics-power: settings: dry run' after clicking Power settings…"
        fi
        if journal | tail -n 25 | grep -qF 'popup: closed'; then
            pass "and the popup closed behind it"
        else
            fail "the popup stayed open after the settings row was clicked"
        fi
    fi
else
    fail "no 'layout: applet-caustics-power-8 target=bar'; the battery chip was not laid out"
fi
bar_env_clear power
fixture_stop fake-upower

say "16c. the Output submenu switches the default sink; Input lists the inputs"
# Two null sinks on a VM with no sound card: the chip appears, Output lists
# two radio rows, and a click on the other moves the default (per the next
# frame and `wpctl`); Input's one row is the disabled "No device" (11). Row
# ids (`audio/src/rows.rs`): 7 Output, 8 Input, devices `(1|2 << 16) | node`.
# VM only: on a nested run it would move the developer's default sink.
if vm_only "the audio-devices step loads null sinks into the session's PipeWire and moves its default sink: on the nested target that is the developer's audio"; then
guest_pactl() { gssh 20 "export XDG_RUNTIME_DIR=/run/user/\$(id -u); $*"; }
audio_sinks_now() { # "node description" per Caustics null sink, from `wpctl status`
    guest_pactl wpctl status | sed -n '/^Audio/,/^Video/p' | sed -n '/Sinks:/,/Sources:/p' |
        grep -oE '[0-9]+\. Caustics[A-Za-z]+' | sed 's/\. / /'
}
audio_mods=()
for name in CausticsSpeakers CausticsHeadphones; do
    m=$(guest_pactl "pactl load-module module-null-sink sink_name=caustics_test_$name sink_properties=device.description=$name" 2>&1 |
        tr -d '\r' | tail -1)
    if [[ "$m" =~ ^[0-9]+$ ]]; then
        audio_mods+=("$m")
    else
        note "pactl load-module for $name said: ${m:-nothing}"
    fi
done
guest_pactl wpctl status 2>&1 | sed -n '/^Audio/,/^Video/p' | grep -v '^Video' |
    while IFS= read -r l; do note "guest wpctl: $l"; done
sinks=$(audio_sinks_now)
if [[ ${#audio_mods[@]} -eq 2 && "$(printf '%s\n' "$sinks" | grep -c .)" -eq 2 ]]; then
    pass "two null sinks in the guest: $(printf '%s' "$sinks" | tr '\n' ' ')"
else
    fail "the guest did not make two null sinks (modules: ${audio_mods[*]:-none}; sinks: $(printf '%s' "$sinks" | tr '\n' ' '))"
fi
# If WirePlumber picks no default within ten seconds, the run picks one.
if ! wait_for_journal "outputs=2 output=" 10; then
    first=$(printf '%s\n' "$sinks" | head -1 | cut -d' ' -f1)
    if [[ -n "$first" ]]; then
        note "no default sink after 10 s; wpctl set-default $first in the guest"
        guest_pactl "wpctl set-default $first" >/dev/null
    fi
fi
if wait_for_journal "outputs=2 output=" 30; then
    audio_line=$(journal | grep -F 'caustics-audio: published rev=' | grep -F 'outputs=2 output=' | tail -1 |
        sed -n 's/.*caustics-audio: /caustics-audio: /p')
    pass "the sidecar lists both: $audio_line"
else
    fail "no 'caustics-audio: published … outputs=2'; the device list never reached a frame ($(journal |
        grep -F 'caustics-audio: ' | tail -2 | tr '\n' ' '))"
    audio_line=""
fi
audio_default=$(printf '%s' "$audio_line" | sed -n 's/.* output=\([0-9]*\).*/\1/p')
audio_other=$(printf '%s\n' "$sinks" | cut -d' ' -f1 | grep -vx "${audio_default:-x}" | head -1)
if printf '%s' "$audio_line" | grep -qE 'inputs=0 input=none'; then
    pass "and no input: the Input submenu will carry the disabled row"
else
    note "the guest has an input after all: $audio_line"
fi

audio_rect=()
if wait_for_journal "layout: applet-caustics-audio target=bar" 20; then
    audio_rect=($(layout_rect applet-caustics-audio))
    pass "the audio chip is on the bar: ${audio_rect[*]}"
else
    fail "no 'layout: applet-caustics-audio target=bar'; the chip did not appear with a sink"
fi

# Open the menu and wait until the two submenu rows are laid out in it.
audio_open_menu() {
    local opens_before deadline
    opens_before=$(journal_count "popup: open kind=menu")
    audio_mark=$(journal_lines)
    click_at "$audio_cx" $((bar_origin_y + audio_cy))
    wait_for_journal_more "popup: open kind=menu" 8 "$opens_before" || return 1
    deadline=$((SECONDS + 20))
    until journal_since "$audio_mark" | grep -q 'layout: applet-caustics-audio-8 target=popup'; do
        ((SECONDS < deadline)) || return 1
        sleep 1
    done
}
# Wait until a row is laid out since the mark (a descent re-lays the popup).
audio_wait_row() { # audio_wait_row <row id> <seconds>
    local deadline=$((SECONDS + $2))
    until journal_since "$audio_mark" | grep -q "layout: applet-caustics-audio-$1 target=popup"; do
        ((SECONDS < deadline)) || return 1
        sleep 1
    done
}

if [[ ${#audio_rect[@]} -eq 4 ]]; then
    audio_cx=$(python3 -c "print(round(${audio_rect[0]} + ${audio_rect[2]} / 2))")
    audio_cy=$(python3 -c "print(round(${audio_rect[1]} + ${audio_rect[3]} / 2))")
    if audio_open_menu; then
        pass "the menu opened with Output and Input in it: $(journal_since "$audio_mark" |
            grep -oE 'layout: applet-caustics-audio-[78] target=popup rect=[0-9.,]*' | sort -u | tr '\n' ' ')"
    else
        fail "the audio menu did not open with its two submenu rows ($(journal_since "${audio_mark:-0}" |
            grep -oE 'layout: applet-caustics-audio-[0-9]+ target=[a-z]+' | sort -u | tr '\n' ' '))"
    fi
    shot audio-menu.png

    # Output: descend, and both sinks are radio rows in the level.
    audio_mark=$(journal_lines)
    if popup_click applet-caustics-audio-7; then
        missing=""
        for node in $(printf '%s\n' "$sinks" | cut -d' ' -f1); do
            audio_wait_row $(((1 << 16) | node)) 15 || missing="$missing $node"
        done
        if [[ -z "$missing" ]]; then
            pass "Output opened: a row per sink ($(journal_since "$audio_mark" |
                grep -oE 'layout: applet-caustics-audio-[0-9]{5,} target=popup' | sort -u | tr '\n' ' '))"
        else
            fail "Output opened without the rows for node(s)$missing ($(journal_since "$audio_mark" |
                grep -oE 'layout: applet-caustics-audio-[0-9]+ target=[a-z]+' | sort -u | tr '\n' ' '))"
        fi
        sleep 1
        shot audio-output.png

        # The other sink: `wpctl set-default`, and the next frame names it.
        if [[ -n "$audio_other" ]] && popup_click "applet-caustics-audio-$(((1 << 16) | audio_other))"; then
            if wait_for_journal "caustics-audio: set-output: wpctl set-default $audio_other" 10; then
                pass "the click ran: $(journal | grep -F 'caustics-audio: set-output:' | tail -1 |
                    sed -n 's/.*caustics-audio: /caustics-audio: /p')"
            else
                fail "no 'caustics-audio: set-output: wpctl set-default $audio_other' after the click"
            fi
            if wait_for_journal "outputs=2 output=$audio_other " 15; then
                pass "and the next frame checks it: $(journal | grep -F "outputs=2 output=$audio_other " |
                    tail -1 | sed -n 's/.*caustics-audio: /caustics-audio: /p')"
            else
                fail "the sidecar never published output=$audio_other"
            fi
            now_default=$(guest_pactl 'wpctl inspect @DEFAULT_AUDIO_SINK@' 2>/dev/null | head -1 |
                sed -n 's/^id \([0-9]*\),.*/\1/p')
            if [[ "$now_default" == "$audio_other" ]]; then
                pass "wpctl agrees: the default sink is $now_default"
            else
                fail "wpctl says the default sink is ${now_default:-unknown}, not $audio_other"
            fi
            if journal | tail -n 40 | grep -qF 'popup: closed'; then
                pass "and the popup closed behind the leaf"
            else
                note "the popup is still open after the device row"
            fi
        else
            fail "no second sink to switch to (default ${audio_default:-none}, sinks $(printf '%s' "$sinks" | tr '\n' ' '))"
        fi
    fi

    # Input: descend into the submenu that has no device.
    sleep 1
    if audio_open_menu; then
        audio_mark=$(journal_lines)
        if popup_click applet-caustics-audio-8; then
            no_input=0
            if audio_wait_row 11 15; then
                no_input=1
                pass "Input opened on its one disabled row, 'No device' (id 11)"
            elif journal_since "$audio_mark" | grep -qE 'layout: applet-caustics-audio-1310[0-9]{2} target=popup'; then
                pass "Input opened on the guest's own input(s): $(journal_since "$audio_mark" |
                    grep -oE 'layout: applet-caustics-audio-1310[0-9]{2}' | sort -u | tr '\n' ' ')"
            else
                fail "Input opened with neither 'No device' nor a device row ($(journal_since "$audio_mark" |
                    grep -oE 'layout: applet-caustics-audio-[0-9]+ target=[a-z]+' | sort -u | tr '\n' ' '))"
            fi
            sleep 1
            shot audio-input.png
            # A disabled row performs nothing: nothing runs.
            if ((no_input)) && popup_click applet-caustics-audio-11; then
                sleep 1
                if journal | jgrep -F 'caustics-audio: set-input:'; then
                    fail "a click on 'No device' ran something: $(journal |
                        grep -F 'caustics-audio: set-input:' | tail -1)"
                else
                    pass "the disabled row ran nothing"
                fi
            fi
        fi
        close_mark=$(journal_lines)
        click_at $((SCREEN_W / 2)) $((SCREEN_H / 3))
        deadline=$((SECONDS + 10))
        until journal_since "$close_mark" | grep -qF 'popup: closed' || ((SECONDS >= deadline)); do
            sleep 1
        done
        if journal_since "$close_mark" | grep -qF 'popup: closed'; then
            pass "a click elsewhere closed the menu"
        elif journal_since "$audio_mark" | grep -qF 'popup: closed'; then
            note "the menu had closed before the click elsewhere"
        else
            fail "the audio menu stayed open after a click elsewhere"
        fi
    else
        fail "the audio menu did not open a second time"
    fi
fi

# Leave the guest as it was: no Caustics sinks, so no chip.
absent_before=$(journal_count "caustics-audio: absent: sink")
for m in "${audio_mods[@]}"; do
    guest_pactl "pactl unload-module $m" >/dev/null 2>&1
done
if [[ -z "$(audio_sinks_now)" ]]; then
    pass "the null sinks are gone again"
else
    fail "null sinks left behind: $(audio_sinks_now | tr '\n' ' ')"
fi
if wait_for_journal_more "caustics-audio: absent: sink" 20 "$absent_before"; then
    pass "and the sidecar said so: the chip is gone with them"
else
    note "no fresh 'caustics-audio: absent: sink' after the unload ($(journal | grep -F 'caustics-audio: ' | tail -1))"
fi
fi

say "17. the chart set is the sidecar's, not the bar's"
bar_env charts "CAUSTICS_SYSMON_CHARTS=cpu"
restart_bar "CAUSTICS_SYSMON_CHARTS=cpu"
if wait_for_journal "sidecar caustics-sysmon: samples, 1 charts" 30; then
    pass "the sidecar settled on one chart: $(journal |
        grep -F 'sidecar caustics-sysmon: samples,' | tail -1)"
else
    fail "no 'sidecar caustics-sysmon: samples, 1 charts' ($(journal |
        grep -F 'sidecar caustics-sysmon: samples' | tail -1))"
fi
if wait_for_journal "charts: 1 laid out" 20; then
    pass "and the bar followed the frame's shape, having no opinion of its own"
else
    fail "no 'charts: 1 laid out' ($(journal | grep -F 'charts:' | tail -1))"
fi
if wait_for_journal "layout: chart-cpu" 30; then
    one_rect=($(layout_rect chart-cpu))
    pass "layout: chart-cpu ${one_rect[*]}"
else
    fail "the one chart was never placed"
fi
if journal | jgrep -E 'layout: chart-(mem|net|disk|gpu|temp) '; then
    fail "a chart the sidecar was not asked for was laid out"
else
    pass "and no other chart exists"
fi
shot one-chart.png

bar_env_clear charts
restart_bar "the six charts restored"
wait_for_journal "charts: 6 laid out" 30 && pass "six charts again" ||
    fail "the six charts did not come back"

say "18. an extra sidecar's applet, menu and slider work"
# The demo publishes one applet with three rows, one a slider (an ITEM_APPLET,
# and menu rows while its menu is open; docs/abi.md, *Rows*). Every click
# comes back as a `WIRE_ACTION` frame, which the demo echoes to stderr.
bar_env sidecars "CAUSTICS_SIDECARS=caustics-sysmon caustics-tray caustics-power caustics-glyphs caustics-reload rows_demo"
restart_bar "CAUSTICS_SIDECARS=caustics-sysmon caustics-tray caustics-power caustics-glyphs caustics-reload rows_demo"
if wait_for_journal "sidecar rows_demo: started pid=" 30; then
    pass "the demo started beside the five sidecars named with it: $(journal |
        grep -F 'sidecar rows_demo: started pid=' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar rows_demo: started pid=N'; the demo never ran"
fi
if wait_for_journal "sidecar rows_demo: rows," 30; then
    rows_line=$(journal | grep -F 'sidecar rows_demo: rows,' | tail -1)
    if printf '%s' "$rows_line" | grep -qF 'rows=4 rev=1'; then
        pass "the rows frame arrived whole: $(printf '%s' "$rows_line" |
            sed -n 's/.*feeders::pipe: //p')"
    else
        fail "the rows witness is not four rows at revision 1: $rows_line"
    fi
else
    fail "no 'sidecar rows_demo: rows, …'; the rows path did not reach the bar"
fi
# Its stderr lands after the frame it describes: wait.
if wait_for_journal "rows_demo: published rev=1" 10; then
    pass "and the demo said so on its own stderr"
else
    note "no 'rows_demo: published' line on stderr"
fi
if wait_for_journal "charts: 6 laid out" 30; then
    pass "the sampler is unchanged beside it"
else
    fail "the charts did not survive an extra sidecar"
fi

# Four rows in, one chip on the bar: the rest exist only while the menu is up.
applet_rect=()
if wait_for_journal "layout: applet-rows_demo target=bar" 30; then
    applet_rect=($(layout_rect applet-rows_demo))
    pass "the demo's applet is on the bar: ${applet_rect[*]}"
else
    fail "no 'layout: applet-rows_demo target=bar …'; the rows never became an item"
fi
# At least the demo's chip and the power button.
applets_n=$(journal | grep -oE 'applets: [0-9]+' | tail -1 | sed -n 's/applets: //p')
if [[ "${applets_n:-0}" -ge 2 ]]; then
    pass "and the summary counts $applets_n applets: the demo's chip and the power button"
else
    fail "the summary counts ${applets_n:-no} applets ($(journal | grep -F 'applets:' | tail -1))"
fi
if journal | jgrep -E 'layout: applet-rows_demo-[0-9]+ '; then
    fail "a menu row was laid out with no menu open"
else
    pass "and not one of its menu rows: they exist only while its menu is up"
fi
shot applet.png

if [[ ${#applet_rect[@]} -eq 4 ]]; then
    applet_cx=$(python3 -c "print(round(${applet_rect[0]} + ${applet_rect[2]} / 2))")
    applet_cy=$(python3 -c "print(round(${applet_rect[1]} + ${applet_rect[3]} / 2))")
    click_at "$applet_cx" $((bar_origin_y + applet_cy))
    if wait_for_journal "popup: requested kind=menu" 8 && wait_for_journal "popup: open kind=menu" 8; then
        pass "the applet click asked for a menu popup and it mapped"
    else
        fail "no menu popup from the applet"
    fi
    sleep 1
    shot applet-menu.png
    # The demo's rows: Hello (2), Toggle (3) and the slider (4).
    missing=""
    for row in 2 3 4; do
        journal | jgrep "layout: applet-rows_demo-$row target=popup" ||
            missing="$missing applet-rows_demo-$row"
    done
    if [[ -z "$missing" ]]; then
        pass "its two rows and its slider are laid out in the popup"
    else
        fail "the popup is missing$missing ($(journal |
            grep -o 'layout: applet-rows_demo-[0-9]* target=[^ ]*' | tr '\n' ' '))"
    fi

    # The slider's middle is 50 of the demo's 0..100: normalising both ways.
    if popup_click applet-rows_demo-4; then
        if wait_for_journal "rows_demo: action row=4 kind=6" 10; then
            slider_echo=$(journal | grep -F 'rows_demo: action row=4 kind=6' | tail -1)
            value=$(printf '%s' "$slider_echo" | sed -n 's/.*value=\([0-9.]*\).*/\1/p')
            pass "the slider reached the demo: $(printf '%s' "$slider_echo" |
                sed -n 's/.*rows_demo: //p')"
            if python3 -c "import sys; sys.exit(0 if 45 <= ${value:-0} <= 55 else 1)"; then
                pass "in the row's own 0..100, at the middle of its track ($value)"
            else
                fail "the value is $value, not the middle of 0..100"
            fi
        else
            fail "no 'rows_demo: action row=4 kind=6' after dragging the slider"
        fi
        # The reply is the next frame, and the menu stays up. rev=3: rev=2 was
        # the demo's answer to the menu opening (it appends a row while its menu is up).
        if wait_for_journal "rows_demo: published rev=3" 10; then
            pass "and the demo republished at the new level: $(journal |
                grep -F 'rows_demo: published rev=3' | tail -1 |
                sed -n 's/.*rows_demo: //p')"
        else
            fail "the demo never republished after the slider moved"
        fi
        if journal | tail -n 40 | grep -qF 'popup: closed'; then
            fail "the popup closed under the slider"
        else
            pass "the menu stayed up under it"
        fi
    fi

    # A leaf row: it performs and the popup closes, exactly as a tray row does.
    if popup_click applet-rows_demo-2; then
        if wait_for_journal "rows_demo: action row=2 kind=1" 10; then
            pass "clicking a row reached the demo: $(journal |
                grep -F 'rows_demo: action row=2 kind=1' | tail -1 |
                sed -n 's/.*rows_demo: //p')"
        else
            fail "no 'rows_demo: action row=2 kind=1' after clicking Hello"
        fi
        if journal | tail -n 25 | grep -qF 'popup: closed'; then
            pass "and the popup closed behind it"
        else
            fail "the popup stayed open after a leaf row was clicked"
        fi
    fi
fi

# The sidecar dies (by the pid the bar logged) and comes back with its rows.
demo_pid=$(journal | grep -F 'sidecar rows_demo: started pid=' | tail -1 |
    sed -n 's/.*pid=\([0-9]*\).*/\1/p')
starts_before=$(journal_count "sidecar rows_demo: started pid=")
rows_before=$(journal_count "sidecar rows_demo: rows,")
# From here: the question is this kill, not the bar's settling first second.
layout_mark=$(journal_lines)
if [[ -n "$demo_pid" ]] && target_kill "$demo_pid"; then
    note "killed the demo (pid $demo_pid)"
    if wait_for_journal_more "sidecar rows_demo: started pid=" 30 "$starts_before"; then
        pass "the sidecar was restarted: $(journal |
            grep -F 'sidecar rows_demo: started pid=' | tail -1 |
            sed -n 's/.*feeders::pipe: //p')"
    else
        fail "the demo was not restarted after it was killed"
    fi
    if wait_for_journal_more "sidecar rows_demo: rows," 30 "$rows_before"; then
        pass "and published the same four rows again: $(journal |
            grep -F 'sidecar rows_demo: rows,' | tail -1 |
            sed -n 's/.*feeders::pipe: //p')"
    else
        fail "the restarted demo never published its rows"
    fi
    if journal_since "$layout_mark" | grep -qF 'layout: applet-rows_demo target=none'; then
        fail "the applet left the bar while its sidecar restarted"
    else
        pass "the applet never left the bar"
    fi
else
    fail "no pid to kill from 'sidecar rows_demo: started pid=N'"
fi
# A health line comes ten seconds after every start.
health_line=$(journal | grep -F 'health (' | tail -1)
if printf '%s' "$health_line" | grep -qF 'sidecars='; then
    note "health: $(printf '%s' "$health_line" | sed -n 's/.*sidecars=/sidecars=/p')"
    # Image slots and curves; the health line has no atlas field.
    if printf '%s' "$health_line" | grep -qE 'images=[0-9]+/[0-9]+ slots'; then
        pass "the health line reports the slot array: $(printf '%s' "$health_line" |
            grep -oE 'images=[^c]*curves=[^ ]* [^ ]* [^ ]* [^ ]* [^ ]* [^ ]*')"
    else
        fail "no 'images=N/M slots' in the health line"
    fi
    if printf '%s' "$health_line" | grep -qF 'atlas='; then
        fail "the health line names an atlas, which the bar does not have"
    fi
else
    fail "no health line ten seconds after a start (health.rs)"
fi
shot two-sidecars.png
bar_env_clear sidecars
restart_bar "the compiled-in sidecar set restored"
if journal | jgrep -E 'layout: applet-rows_demo'; then
    fail "the demo's applet is on the bar with the demo out of the sidecar set"
else
    pass "and with the demo gone from the set, so is its applet"
fi

say "18b. a menu gains a row while open, and stays open"
# The demo appends a row on `ACTION_MENU_OPEN` (kind 7) and drops it on
# `ACTION_MENU_CLOSE` (kind 8; docs/abi.md, *Action*). The menu must stay open:
# the caustic holds the open level as an id, not an item index.
bar_env sidecars "CAUSTICS_SIDECARS=caustics-sysmon caustics-tray caustics-power caustics-glyphs caustics-reload rows_demo"
restart_bar "CAUSTICS_SIDECARS with rows_demo again"
if wait_for_journal "sidecar rows_demo: rows, applet=0 rows=4 rev=1" 30; then
    pass "the demo is back with its four rows: $(journal |
        grep -F 'sidecar rows_demo: rows,' | tail -1 | sed -n 's/.*feeders::pipe: //p')"
else
    fail "the demo did not publish four rows after the restart"
fi

grow_rect=()
if wait_for_journal "layout: applet-rows_demo target=bar" 30; then
    grow_rect=($(layout_rect applet-rows_demo))
fi
if [[ ${#grow_rect[@]} -ne 4 ]]; then
    fail "no rect for the demo's chip; the menu-grows step cannot be driven"
else
    grow_cx=$(python3 -c "print(round(${grow_rect[0]} + ${grow_rect[2]} / 2))")
    grow_cy=$(python3 -c "print(round(${grow_rect[1]} + ${grow_rect[3]} / 2))")
    # Read *since this click*: the question is one of order.
    grow_mark=$(journal_lines)
    click_at "$grow_cx" $((bar_origin_y + grow_cy))
    if wait_for_journal "popup: open kind=menu" 8; then
        pass "the demo's menu opened"
    else
        fail "the demo's menu did not open"
    fi

    # 1. The bar tells the sidecar its menu opened (row 1, the demo's ROW_BAR).
    if wait_for_journal "rows_demo: action row=1 kind=7" 10; then
        pass "the open notice reached the demo: $(journal |
            grep -F 'rows_demo: action row=1 kind=7' | tail -1 | sed -n 's/.*rows_demo: //p')"
    else
        fail "no 'rows_demo: action row=1 kind=7'; the bar never said the menu opened"
    fi
    # 2. The demo answers with a five-row frame, while the menu is up.
    if wait_for_journal "rows_demo: published rev=2 level=40 opens=1 menu=open" 10; then
        pass "and it republished with the extra row: $(journal |
            grep -F 'rows_demo: published rev=2' | tail -1 | sed -n 's/.*rows_demo: //p')"
    else
        fail "the demo never republished after the open notice"
    fi
    if wait_for_journal "sidecar rows_demo: rows, applet=0 rows=5 rev=2" 10; then
        pass "the bar read five rows off the wire"
    else
        fail "the bar never saw a five-row frame ($(journal |
            grep -F 'sidecar rows_demo: rows,' | tail -1))"
    fi
    # 3. And draws it, in the popup, which is the whole point.
    if wait_for_journal "layout: applet-rows_demo-5 target=popup" 10; then
        pass "the appended row is laid out in the open popup: $(journal |
            grep -F 'layout: applet-rows_demo-5 target=popup' | tail -1 |
            sed -n 's/.*layout: //p')"
    else
        fail "no 'layout: applet-rows_demo-5 target=popup'; the new row was never drawn"
    fi

    # 4. **Nothing closed.** The slice from the click to the appended row's
    #    rect, with no `popup: closed` in it: the assertion the step exists for.
    grow_slice=$(journal_since "$grow_mark" |
        sed -n '1,/layout: applet-rows_demo-5 target=popup/p')
    if printf '%s\n' "$grow_slice" | grep -qF 'popup: closed'; then
        fail "the popup closed between the open notice and the new row: $(
            printf '%s\n' "$grow_slice" | grep -F 'popup: closed' | tail -1 |
            sed -n 's/.*popup: //p')"
    else
        pass "and the popup never closed on the way: a reshape under a menu is not its end"
    fi
    # And **taller**, measured (any menu repositions when its rows arrive).
    grow_heights=$(journal_since "$grow_mark" |
        sed -n 's/.*layout: ELEM_POPUP target=popup rect=[0-9.-]*,[0-9.-]*,[0-9.-]*,\([0-9.-]*\).*/\1/p')
    grow_h0=$(printf '%s\n' "$grow_heights" | grep -v '^$' | head -1)
    grow_h1=$(printf '%s\n' "$grow_heights" | grep -v '^$' | tail -1)
    if [[ -n "$grow_h0" && -n "$grow_h1" ]] &&
        python3 -c "import sys; sys.exit(0 if $grow_h1 > $grow_h0 else 1)"; then
        pass "and the popup grew to hold it: ${grow_h0} -> ${grow_h1} px tall"
    else
        fail "the popup did not grow for the extra row (ELEM_POPUP heights: $(
            printf '%s' "$grow_heights" | tr '\n' ' '))"
    fi
    if journal_since "$grow_mark" | grep -qF 'popup: repositioned to'; then
        note "$(journal_since "$grow_mark" | grep -F 'popup: repositioned to' | tail -1 |
            sed -n 's/.*popup: //p')"
    fi
    grow_popup=$(journal | grep -F 'layout: ELEM_POPUP ' | tail -1)
    if printf '%s' "$grow_popup" | grep -qF 'target=popup'; then
        pass "its own rect is present after the row arrived: $(printf '%s' "$grow_popup" |
            sed -n 's/.*layout: //p')"
    else
        fail "ELEM_POPUP is gone after the row arrived: $grow_popup"
    fi
    sleep 1
    shot menu-grown.png

    # 5. Close it with a click off the bar, and the row goes with the notice.
    grow_mark=$(journal_lines)
    click_at $((SCREEN_W / 2)) $((SCREEN_H / 3))
    if wait_for_journal "rows_demo: action row=1 kind=8" 10; then
        pass "the close notice reached the demo: $(journal |
            grep -F 'rows_demo: action row=1 kind=8' | tail -1 | sed -n 's/.*rows_demo: //p')"
    else
        fail "no 'rows_demo: action row=1 kind=8'; the bar never said the menu closed"
    fi
    if wait_for_journal "rows_demo: published rev=3 level=40 opens=1 menu=closed" 10; then
        pass "and the row went with it: $(journal |
            grep -F 'rows_demo: published rev=3' | tail -1 | sed -n 's/.*rows_demo: //p')"
    else
        fail "the demo never republished without the extra row"
    fi
    if wait_for_journal "sidecar rows_demo: rows, applet=0 rows=4 rev=3" 10; then
        pass "four rows again on the wire"
    else
        fail "the next frame is not four rows ($(journal |
            grep -F 'sidecar rows_demo: rows,' | tail -1))"
    fi
    if journal_since "$grow_mark" | grep -qF 'popup: closed'; then
        pass "the popup closed once, when it was asked to: $(journal_since "$grow_mark" |
            grep -F 'popup: closed' | tail -1 | sed -n 's/.*popup: //p')"
    else
        fail "the menu did not close on a click elsewhere"
    fi
fi

# No menu in the run closed because the table reshaped under it.
if journal | jgrep -F 'reshaped under the open menu'; then
    fail "a menu was closed by a reshape: $(journal |
        grep -F 'reshaped under the open menu' | tail -1)"
else
    pass "no 'reshaped under the open menu' anywhere: a reshape never closes a menu"
fi

bar_env_clear sidecars
restart_bar "the compiled-in sidecar set restored"

say "19. a sidecar writing garbage is one witness and a restart, never a dead bar"
# Text on stdout (the wire) is caught on its first four bytes (docs/abi.md
# *Framing*): the child restarts with backoff, the bar never notices.
bar_restarts_before=$(bar_restarts)
bar_env sidecars "CAUSTICS_SIDECARS=/usr/bin/yes"
restart_bar "CAUSTICS_SIDECARS=/usr/bin/yes"
if wait_for_journal "sidecar yes: bad magic" 30; then
    pass "the bad magic is one witness: $(journal |
        grep -F 'sidecar yes: bad magic' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "no 'sidecar yes: bad magic …'; a text stream was not caught on its first bytes"
fi
if wait_for_journal "sidecar yes: exited" 30; then
    pass "and it was restarted with a backoff: $(journal |
        grep -F 'sidecar yes: exited' | tail -1 |
        sed -n 's/.*feeders::pipe: //p')"
else
    fail "the garbage sidecar was never restarted"
fi
assert_contains "the bar is still up" "$(bar_active)" active
bar_restarts_after=$(bar_restarts)
if [[ "${bar_restarts_after:-0}" -eq "${bar_restarts_before:-0}" ]]; then
    pass "and the bar was not restarted (restarts ${bar_restarts_after:-0})"
else
    fail "the bar itself restarted ($bar_restarts_before -> $bar_restarts_after)"
fi
if journal | jgrep -E 'layout: chart-'; then
    fail "a chart was laid out with no sampler in the set at all"
else
    pass "no charts: \$CAUSTICS_SIDECARS replaced the whole set, sampler included"
fi
shot bad-sidecar.png
bar_env_clear sidecars
restart_bar "the compiled-in sidecar set restored"
if wait_for_journal "charts: 6 laid out" 30; then
    pass "six charts again"
else
    fail "the charts did not come back after the drop-in was removed"
fi

# The room the chart step made goes back: the bar as it ships.
if restore_tunables; then
    pass "the shipped tunables are back and the dock with them"
else
    fail "restoring the shipped tunables did not reload"
fi

# The swap is logged a frame before the layout it causes: let the clock move.
sleep 2

# Re-probe the origin after the restarts.
clock_rect=($(layout_rect clock))
if [[ ${#clock_rect[@]} -eq 4 ]]; then
    clock_cx=$(python3 -c "print(round(${clock_rect[0]} + ${clock_rect[2]} / 2))")
fi
bar_origin_y=$(probe_bar_origin "$clock_cx")
note "the bar's top edge is at screen y=${bar_origin_y:-unknown} px"

# Caffeine stays in the bar's process: it owns a Wayland object on the bar's
# connection (docs/architecture.md, *Sidecars*).
# niri cannot be asked about inhibitors, so the bar's witnesses are the proof.

say "20. the caffeine chip holds and releases an idle inhibitor"
summary=$(journal | grep -F 'feeders: ' | tail -1)
if printf '%s' "$summary" | grep -qF 'caffeine=bound'; then
    pass "the manager is bound: $summary"
elif printf '%s' "$summary" | grep -qF 'caffeine=absent'; then
    fail "zwp_idle_inhibit_manager_v1 absent; does this niri still offer it? ($summary)"
else
    fail "no 'feeders: …' summary line to read caffeine off ($summary)"
fi

# Which of the four icon names resolve; with none there is no chip.
caffeine_icons=$(on_target 25 'for n in weather-clear-symbolic weather-clear-night-symbolic \
        preferences-desktop-screensaver-symbolic user-idle-symbolic; do
    find /usr/share/icons $HOME/.local/share/icons -name "$n.*" 2>/dev/null |
        head -1 | sed "s|^|$n -> |"
done')
note "the target's caffeine icons:"
printf '%s\n' "${caffeine_icons:-  (none of the four resolve)}" | sed 's/^/        /'

caffeine_rect=($(layout_rect caffeine))
if [[ ${#caffeine_rect[@]} -ne 4 ]]; then
    # No icon, no chip, one witness.
    if journal | jgrep -E 'caffeine: no icon for (on|off)'; then
        pass "no icon in the target's themes, so no chip and one witness: $(journal |
            grep -F 'caffeine: no icon' | tail -1 | sed -n 's/.*caffeine: /caffeine: /p')"
    else
        fail "no 'layout: caffeine' rect and no 'caffeine: no icon' witness either"
    fi
else
    pass "the caffeine chip laid out: ${caffeine_rect[*]}"
    caffeine_cx=$(python3 -c "print(round(${caffeine_rect[0]} + ${caffeine_rect[2]} / 2))")
    caffeine_cy=$(python3 -c "print(round(${caffeine_rect[1]} + ${caffeine_rect[3]} / 2))")
    caffeine_sy=$(python3 -c "print(round($bar_origin_y + ${caffeine_rect[1]}))")

    # The icon is the state: pixels moved, pointer parked for every shot.
    caffeine_probe=("${caffeine_rect[0]}" "$caffeine_sy" "${caffeine_rect[2]}" "${caffeine_rect[3]}")
    park_pointer
    shot caffeine-free.png

    held_before=$(journal_count "caffeine: inhibitor held")
    click_at "$caffeine_cx" $((bar_origin_y + caffeine_cy))
    if wait_for_journal "action: ToggleCaffeine -> caffeine" 8; then
        pass "the click resolved to the in-process feeder, not to a sidecar"
    else
        fail "no 'action: ToggleCaffeine -> caffeine' after clicking the chip"
    fi
    if wait_for_journal_more "caffeine: inhibitor held" 8 "$held_before"; then
        pass "an inhibitor is held"
    else
        fail "no 'caffeine: inhibitor held' after the click"
    fi
    park_pointer
    shot caffeine-held.png
    moved=$(png_pixel_diff "$SHOTS/caffeine-free.png" "$SHOTS/caffeine-held.png" \
        "${caffeine_probe[@]}")
    if [[ "${moved:-0}" -ge 100 ]]; then
        pass "the icon changed with the state ($moved pixels of the chip moved)"
    else
        fail "the chip looks the same held as free (only $moved pixels moved)"
    fi

    released_before=$(journal_count "caffeine: inhibitor released")
    click_at "$caffeine_cx" $((bar_origin_y + caffeine_cy))
    if wait_for_journal_more "caffeine: inhibitor released" 8 "$released_before"; then
        pass "and the second click released it"
    else
        fail "no 'caffeine: inhibitor released' after the second click"
    fi
    park_pointer
    shot caffeine-free-again.png
    back=$(png_pixel_diff "$SHOTS/caffeine-free.png" "$SHOTS/caffeine-free-again.png" \
        "${caffeine_probe[@]}")
    if [[ "${back:-99}" -le 20 ]]; then
        pass "and the chip is back to the free icon ($back pixels differ)"
    else
        fail "the chip did not go back to the free icon ($back pixels differ)"
    fi

    # A health line comes ten seconds after each start and hourly after;
    # read the newest.
    health=$(journal | grep -F 'health (' | tail -1)
    if [[ -n "$health" ]]; then
        if printf '%s' "$health" | grep -qE 'caffeine=(free|held) toggles=[0-9]+'; then
            pass "the health line carries the field: $(printf '%s' "$health" |
                grep -oE 'caffeine=[a-z]+ toggles=[0-9]+')"
        else
            fail "a health line with no caffeine field: $health"
        fi
    else
        note "no health line since the last start; the field is confirmed on a target that runs past the hour"
    fi
fi

# The dock entry's right-click menu (New window, Close, Pin/Unpin). Its window
# means the hover preview is up when the button goes down: the embassy must
# swap the preview for the menu rather than wedge.

say "21. the dock's right-click menu pins, unpins and closes"

guest_favourites_now() { on_target 20 "/usr/bin/cat $guest_favourites 2>/dev/null" | tr '\n' ' '; }

# Which rows of the dock's menu are laid out *now*.
dock_rows_now() {
    layout_placed_now 'dock-(new-window|close|pin|unpin)' any | tr '\n' ' ' | xargs
}

# Right-click our entry; re-read its rect every time (a pin moves it).
open_dock_menu() {
    entry_rect=($(layout_rect "${term_app_id:-}"))
    if [[ ${#entry_rect[@]} -ne 4 ]]; then
        fail "no 'layout: ${term_app_id:-none}' rect to right-click"
        return 1
    fi
    entry_cx=$(python3 -c "print(round(${entry_rect[0]} + ${entry_rect[2]} / 2))")
    entry_cy=$(python3 -c "print(round(${entry_rect[1]} + ${entry_rect[3]} / 2))")
    entry_sy=$(python3 -c "print(round($bar_origin_y + $entry_cy))")
    note "$term_app_id at bar px ${entry_cx},${entry_cy} -> screen ${entry_cx},${entry_sy}"
    local before
    before=$(journal_count "popup: open kind=menu")
    right_click_at "$entry_cx" "$entry_sy"
    if ! wait_for_journal_more "popup: open kind=menu" 10 "$before"; then
        fail "no menu popup from a right click on the dock"
        return 1
    fi
    sleep 1
    return 0
}

# Favourites without our terminal, so its row is Pin: a running non-favourite
# is at the dock's end and a pin appends it there, so it does not move (an
# unpin could push an entry under the clock). It also exercises the file read.
on_target 20 "mkdir -p \$(dirname $guest_favourites)
    printf 'org.mozilla.firefox\nkitty\n' > $guest_favourites" >/dev/null
restart_bar "a favourites file without $term_app_id"
if wait_for_journal "favourites: 2 read from" 20; then
    pass "$(journal | grep -F 'favourites: 2 read from' | tail -1 |
        sed -n 's/.*favourites: /favourites: /p')"
else
    fail "the bar did not read the favourites file it was given"
fi

if [[ -z "${term_app_id:-}" ]]; then
    fail "the dock step found no app id for the terminal; the dock menu cannot be tested"
elif open_dock_menu; then
    pass "the right click opened a menu popup"
    # The preview must be torn down, not left half-open waiting for its configure.
    if journal | jgrep -F 'popup: replaced'; then
        pass "and the hover preview it replaced was torn down: $(journal |
            grep -F 'popup: replaced' | tail -1 | sed -n 's/.*popup: /popup: /p')"
    else
        note "no preview was up when the click landed; the swap path was not exercised"
    fi
    shot dock-menu.png

    rows=$(dock_rows_now)
    if [[ "$rows" == "dock-close dock-new-window dock-pin" ]]; then
        pass "three rows, sorted: $rows"
        wrote_before=$(journal_count "favourites: wrote")
        note "the favourites file before: $(guest_favourites_now)(end)"
        if popup_click dock-pin; then
            if wait_for_journal_more "favourites: wrote" 10 "$wrote_before"; then
                pass "$(journal | grep -F 'favourites: wrote' | tail -1 |
                    sed -n 's/.*favourites: /favourites: /p')"
            else
                fail "no 'favourites: wrote' after clicking Pin"
            fi
            after_file=$(guest_favourites_now)
            note "the favourites file after: ${after_file:-(absent)}"
            if printf ' %s ' "$after_file" | grep -qF " $term_app_id "; then
                pass "$term_app_id is in the file the dock wrote, after the two it read"
            else
                fail "Pin did not put $term_app_id in the file ($after_file)"
            fi

            # Reopened, the row has flipped to Unpin.
            if open_dock_menu; then
                flipped=$(dock_rows_now)
                if [[ "$flipped" == "dock-close dock-new-window dock-unpin" ]]; then
                    pass "the row flipped to Unpin: $flipped"
                else
                    fail "the row did not flip to dock-unpin (rows: $flipped)"
                fi
                shot dock-menu-flipped.png

                # Close: the window the entry's left click would have picked.
                windows_before=$(niri_msg --json windows |
                    python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null)
                if popup_click dock-close; then
                    if wait_for_journal "action: Close(" 8; then
                        pass "$(journal | grep -F 'action: Close(' | tail -1 |
                            sed -n 's/.*action: /action: /p')"
                    else
                        fail "no 'action: Close(N) -> …' after clicking Close"
                    fi
                    sleep 2
                    windows_after=$(niri_msg --json windows |
                        python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null)
                    if [[ -n "${windows_after:-}" &&
                          "${windows_after:-0}" -lt "${windows_before:-0}" ]]; then
                        pass "niri closed it: ${windows_before} windows -> ${windows_after}"
                    else
                        fail "the window count did not fall (${windows_before:-?} -> ${windows_after:-?})"
                    fi
                fi
            fi
        fi
    else
        fail "the dock menu holds '$rows', not New window / Close / Pin"
    fi
fi

# The file is read once, so the seed returns only on a restart.
on_target 20 "rm -f $guest_favourites" >/dev/null 2>&1 || true
restart_bar "the favourites file removed, so the dock is the built-in seed again"


# New window: the bar hands an argv to niri's `Spawn` over its IPC socket and
# forks nothing. The window appears, and its process is not the bar's child.

say "21b. New window launches through the compositor's IPC, not as the bar's child"

# `gio launch` (preferred: it resolves Flatpak wrappers) or the entry's `Exec=`.
if on_target 20 'test -x /usr/bin/gio' >/dev/null 2>&1; then
    note "the target has /usr/bin/gio: the 'gio launch <path>' arm is under test"
else
    note "the target has no /usr/bin/gio: the entry's own 'Exec=' arm is under test"
fi

# Every window id niri knows, as one line.
window_ids_now() {
    niri_msg --json windows | python3 -c '
import json, sys
print(" ".join(str(w["id"]) for w in json.load(sys.stdin)))' 2>/dev/null
}

bar_pid=$(bar_pid)
note "the bar is pid ${bar_pid:-unknown} on the target"

if [[ -z "${term_app_id:-}" ]]; then
    fail "the dock step found no app id for the terminal; New window cannot be tested"
elif open_dock_menu; then
    ids_before=$(window_ids_now)
    count_before=$(printf '%s' "$ids_before" | wc -w)
    spawn_before=$(journal_count "action: Spawn {")
    note "$count_before windows before the click"
    if popup_click dock-new-window; then
        if wait_for_journal_more "action: Spawn {" 10 "$spawn_before"; then
            spawn_line=$(journal | grep -F 'action: Spawn {' | tail -1 |
                sed -n 's/.*action: /action: /p')
            # `-> niri-ipc (argv …)`: the compositor seam, with the argv.
            if printf '%s' "$spawn_line" | grep -q -- '-> niri-ipc (argv '; then
                pass "$spawn_line"
            else
                fail "the launch did not reach the compositor seam: $spawn_line"
            fi
        else
            fail "no 'action: Spawn { … } -> niri-ipc (argv …)' after clicking New window"
        fi

        # niri started it. A new window can take seconds, so poll.
        ids_after=""
        count_after=0
        deadline=$((SECONDS + 30))
        while ((SECONDS < deadline)); do
            ids_after=$(window_ids_now)
            count_after=$(printf '%s' "$ids_after" | wc -w)
            [[ "$count_after" -gt "$count_before" ]] && break
            sleep 2
        done
        if [[ "$count_after" -gt "$count_before" ]]; then
            pass "niri opened it: $count_before windows -> $count_after"
        else
            fail "the window count did not grow ($count_before -> $count_after)"
        fi
        shot new-window.png

        # The window id this click added, and nothing else.
        new_id=""
        for id in $ids_after; do
            case " $ids_before " in
                *" $id "*) ;;
                *) new_id="$id" ;;
            esac
        done

        # The bar's only children are its sidecars.
        if [[ -n "${bar_pid:-}" && "$bar_pid" != "0" ]]; then
            bar_kids=$(on_target 20 "ps --no-headers --ppid $bar_pid -o args= 2>/dev/null" |
                sed 's/^ *//' | sort -u)
            note "the bar's children: $(printf '%s' "$bar_kids" | tr '\n' '|')"
            if printf '%s\n' "$bar_kids" | grep -qE "(^|/)(gio|$guest_term)( |$)"; then
                fail "the bar forked the launch itself; launches go through the compositor's IPC"
            else
                pass "no launcher and no $guest_term under the bar: every child is a sidecar"
            fi
        else
            note "no bar pid; the no-child check did not run"
        fi

        # This run opened that window, so it closes it, by id, never by name.
        if [[ -n "$new_id" ]]; then
            niri_msg action close-window --id "$new_id" >/dev/null 2>&1 || true
            sleep 3
            closed=$(window_ids_now | wc -w)
            if [[ "$closed" -le "$count_before" ]]; then
                pass "and the window it opened is closed again: $count_after -> $closed"
            else
                fail "window $new_id did not close ($count_after -> $closed)"
            fi
        else
            note "could not tell which window id was new; nothing was closed"
        fi
    fi
fi


# The tunables are the bar's configuration (docs/architecture.md,
# *Configuration is the caustic*): editing the tree's own file and saving it is
# the whole story, and the running bar moves.

say "22. a tunables edit moves the bar; a broken one leaves it running"
reloads_before=$(journal_count "$SWAPPED")
failures_before=$(journal_count "reload: failed")
anchors_before=$(journal_count "re-anchored to the bottom edge")
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    sed -i -e "s/^public static const uint EDGE = .*/public static const uint EDGE = EDGE_BOTTOM;/" \
           -e "s/^public static const float BAR_HEIGHT = .*/public static const float BAR_HEIGHT = 40.0;/" \
           -e "s/^public static const float ICON_LOGICAL = .*/public static const float ICON_LOGICAL = 24.0;/" \
        $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "$SWAPPED" 40 "$reloads_before"; then
    pass "$(journal | grep -F "$SWAPPED" | tail -1)"
else
    fail "no swap within 40 s of saving caustics/bar_tunables.slang"
fi
if wait_for_journal "tunables: applied" 8; then
    pass "$(journal | grep -F 'tunables: applied' | tail -1)"
else
    fail "no 'tunables: applied'; the caustic's ask did not reach the embassy"
fi
# 30 s: the re-anchor is the swapped caustic's first frame, and new pipelines
# cost seconds (20–27 s on lavapipe; docs/testing.md, *Known behaviour the
# steps account for*).
if wait_for_journal_more "re-anchored to the bottom edge" 30 "$anchors_before"; then
    pass "the layer surface moved to the bottom edge"
else
    fail "no 're-anchored' line; the edge change did not reach the surface"
    # A loaded host slows the compile; the lines around the swap say how.
    note "anchor/reload lines: $(journal | grep -E 'anchor|tunables|caustics-reload|swapped' | tail -6 | tr '\n' '|' | cut -c1-600)"
fi
sleep 2
shot tunables-bottom.png
# The clock's new layout line: the table was re-laid-out, not only moved.
if journal | tail -n 60 | grep -qF 'layout: clock'; then
    pass "the layout re-emitted after the tunables changed"
else
    fail "no 'layout:' lines after the tunables changed"
fi

# An edit that does not compile must leave the last good bar running, and
# Slang's diagnostic names the file in the tree, the one being edited, at the
# line and column (`…/caustics/bar_tunables.slang:<line>:<col>`).
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    printf "\n#error deliberate tunables failure\n" >> $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "reload: failed" 40 "$failures_before"; then
    pass "a broken tunables file is a diagnostic, not a dead bar"
    note "$(journal | tail -n 6 | tail -4)"
else
    fail "a broken bar_tunables.slang produced no diagnostic"
fi
assert_contains "the bar is still up" "$(bar_active)" active
if journal | tail -n 40 | grep -qE 'caustics/bar_tunables\.slang:[0-9]+:[0-9]+'; then
    pass "the diagnostic names caustics/bar_tunables.slang, at a line and column"
else
    fail "the diagnostic did not name caustics/bar_tunables.slang:<line>:<col>: $(journal | tail -n 8 | tr '\n' '|' | cut -c1-600)"
fi

anchors_before=$(journal_count "re-anchored to the top edge")
if restore_tunables && wait_for_journal_more "re-anchored to the top edge" 30 "$anchors_before"; then
    pass "restoring the shipped tunables put the bar back at the top"
else
    fail "the bar did not re-anchor to the top after the tunables were restored"
fi
sleep 1

# The rest of the run wants the bar back at the top and its origin re-probed.
clock_rect=($(layout_rect clock))
bar_origin_y=$(probe_bar_origin "$clock_cx")
note "the bar's top edge is at screen y=${bar_origin_y:-unknown} px again"

say "23. editing the caustic hot-reloads it; a broken edit is a diagnostic"
clock_rect=($(layout_rect clock))
probe_ok=0
if [[ ${#clock_rect[@]} -eq 4 && -n "$bar_origin_y" ]]; then
    probe_x=${clock_rect[0]}
    probe_y=$((bar_origin_y + ${clock_rect[1]}))
    probe_w=${clock_rect[2]}
    probe_h=${clock_rect[3]}
    probe_ok=1
    note "clock texel probe: screen rect $probe_x,$probe_y ${probe_w}x${probe_h}"
fi
shot reload-before.png
before_rgb=""
[[ "$probe_ok" -eq 1 ]] &&
    before_rgb=$(png_mean "$SHOTS/reload-before.png" "$probe_x" "$probe_y" "$probe_w" "$probe_h")

reloads_before=$(journal_count "$SWAPPED")
failures_before=$(journal_count "reload: failed")
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    sed -i -e "s/^public static const float3 RELOAD_SMOKE_TINT = .*/public static const float3 RELOAD_SMOKE_TINT = float3(1.0, 0.30, 0.45);/" \
        $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "$SWAPPED" 40 "$reloads_before"; then
    pass "$SWAPPED"
    # The swap line is written before the swapped caustic's first frame is
    # presented, seconds before it on a software device, so the probe is
    # polled for the change rather than read off one screenshot a second on.
    after_rgb=
    for _ in $(seq 15); do
        sleep 1
        shot reload-after.png
        [[ "$probe_ok" -eq 1 ]] || break
        after_rgb=$(png_mean "$SHOTS/reload-after.png" "$probe_x" "$probe_y" "$probe_w" "$probe_h")
        [[ -n "$before_rgb" && "$after_rgb" != "$before_rgb" ]] && break
    done
    if [[ "$probe_ok" -eq 1 ]]; then
        note "clock mean RGB: [$before_rgb] -> [$after_rgb]"
        if [[ -n "$before_rgb" && "$before_rgb" != "$after_rgb" ]]; then
            pass "the clock's texels changed colour: the swapped caustic is on screen"
        else
            fail "the clock's texels did not change ([$before_rgb] -> [$after_rgb])"
        fi
    else
        note "no clock rect to probe; the reload is proven only by the journal"
    fi
else
    fail "no swap within 40 s"
    note "$(journal | tail -n 20)"
fi

on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    printf "\n#error deliberate reload failure\n" >> $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "reload: failed" 40 "$failures_before"; then
    pass "reload: failed, with Slang's diagnostic"
    note "$(journal | tail -n 6 | tail -4)"
else
    fail "a broken caustic did not produce 'reload: failed'"
fi
if [[ "$(bar_active)" == active ]]; then
    pass "the bar survived the bad caustic"
else
    fail "the bar died on a bad caustic"
fi
restore_tunables && note "restored caustic reloaded"

# A restarted compiler has forgotten the session's swapchain format; the pipe
# replays the bar's `REQ_PROGRAM` to every child (docs/abi.md, *Request*).
# Kill it by pid, see it respawn, get the replay, and compile again. The
# run's cache is emptied first, so the replay finds nothing cached and the
# quiet start is the one seen; the cache step below sees the warm one.

say "23b. a restarted caustics-reload is sent the program request again"
reload_pid=$(journal | grep -oE 'sidecar caustics-reload: started pid=[0-9]+' |
    tail -1 | grep -oE '[0-9]+$')
if [[ -z "$reload_pid" ]]; then
    fail "no 'sidecar caustics-reload: started pid=' in the journal; the compiler never ran"
else
    note "caustics-reload is pid $reload_pid on the target"
    starts_before=$(journal_count "sidecar caustics-reload: started pid=")
    replays_before=$(journal_count "remembered request(s) sent to pid=")
    quiet_before=$(journal_count "nothing cached for this directory")
    on_target 20 'rm -f $T_CACHE/caustics/reload/*.modules' >/dev/null
    reloads_before=$(journal_count "$SWAPPED")
    # By pid, never by name.
    target_kill "$reload_pid" >/dev/null
    if wait_for_journal_more "sidecar caustics-reload: started pid=" 40 "$starts_before"; then
        pass "$(journal | grep -F 'sidecar caustics-reload: started pid=' | tail -1)"
    else
        fail "the supervisor did not restart caustics-reload within 40 s"
    fi
    if wait_for_journal_more "remembered request(s) sent to pid=" 20 "$replays_before"; then
        pass "$(journal | grep -F 'remembered request(s) sent to pid=' | tail -1)"
    else
        fail "the new child was not sent the remembered REQ_PROGRAM"
    fi
    # Nothing cached, so nothing sent on the replay: the bar already runs
    # this directory. The save below proves the format landed.
    if wait_for_journal_more "nothing cached for this directory" 20 "$quiet_before"; then
        pass "$(journal | grep -F 'nothing cached for this directory' | tail -1 |
            sed -n 's/.*caustics-reload: //p')"
    else
        fail "the restarted compiler did not witness its quiet start"
    fi
    if [[ "$(journal_count "$SWAPPED")" -eq "$reloads_before" ]]; then
        pass "and it compiled nothing on the replay"
    else
        fail "the restarted compiler sent a generation with nothing cached"
    fi
    on_target 20 'touch $T_CAUSTICS/bar_tunables.slang'
    if wait_for_journal_more "$SWAPPED" 90 "$reloads_before"; then
        pass "a save reaches the bar through the restarted compiler: $(journal |
            grep -F "$SWAPPED" | tail -1 | sed -n 's/.*sidecar //p')"
    else
        fail "touching the tunables after the restart produced no swap"
    fi
fi

# The SPIR-V cache, keyed on the bytes that reach the compiler, spares a restart
# after an edit seconds of the baked chrome. A never-seen file is a miss; a
# restart over it is a hit, timed from the format to the generation sent.
reloads_before=$(journal_count "$SWAPPED")
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    sed -i -e "s/^public static const float3 RELOAD_SMOKE_TINT = .*/public static const float3 RELOAD_SMOKE_TINT = float3(0.20, 0.80, 0.55);/" \
        $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "$SWAPPED" 120 "$reloads_before"; then
    log="$(journal)"
    cold_ms=$(grep -oE 'generation [0-9]+ sent \([^)]*\)' <<<"$log" | tail -1 |
        sed -n 's/.*, \([0-9]*\) ms, .*/\1/p')
    assert_contains "tunables the cache has never seen are compiled" \
        "$(grep -F 'reload: cache:' <<<"$log" | tail -2 | tr '\n' ' ')" "miss"
    note "the cold compile took ${cold_ms:-?} ms on the target"
else
    fail "no swap within 120 s of saving an edited bar_tunables.slang"
fi

mark_journal
bar_restart
if wait_for_journal "$SWAPPED" 90; then
    log="$(journal -o short-unix)"
    warm_ms=$(grep -oE 'generation [0-9]+ sent \([^)]*\)' <<<"$log" | tail -1 |
        sed -n 's/.*, \([0-9]*\) ms, .*/\1/p')
    assert_contains "the same bytes are read back, not compiled again" \
        "$(grep -F 'reload: cache:' <<<"$log" | tail -1)" "cache: hit"
    # Two lines of one process: the whole of what the bar waits for.
    asked_at=$(grep -F "the bar's program is" <<<"$log" | tail -1 | awk '{print $1}')
    sent_at=$(grep -E 'generation [0-9]+ sent' <<<"$log" | tail -1 | awk '{print $1}')
    if [[ -n "$asked_at" && -n "$sent_at" ]] &&
        awk -v a="$asked_at" -v b="$sent_at" 'BEGIN { exit !(b - a >= 0 && b - a <= 1.0) }'; then
        pass "the swap followed the format by $(awk -v a="$asked_at" -v b="$sent_at" \
            'BEGIN { printf "%.3f", b - a }') s (${warm_ms:-?} ms in the sidecar)"
    else
        fail "the warm start did not swap within a second of learning its format ($(
            awk -v a="${asked_at:-0}" -v b="${sent_at:-0}" 'BEGIN { printf "%.3f", b - a }') s)"
    fi
else
    fail "no swap within 90 s of a restart with the cache warm"
fi
# At most `CACHE_KEEP` (8, `reload/src/lib.rs`) entries, plus a temp file.
cache_state="$(on_target 20 'ls -1 $T_CACHE/caustics/reload 2>/dev/null | wc -l
    du -sh $T_CACHE/caustics/reload 2>/dev/null | cut -f1')"
cache_files=$(head -1 <<<"$cache_state")
note "the target's cache holds ${cache_files:-0} file(s), $(tail -1 <<<"$cache_state")"
if [[ "${cache_files:-0}" -ge 1 && "${cache_files:-0}" -le 9 ]]; then
    pass "the cache directory is bounded (${cache_files} files, at most 8 entries and a temp)"
else
    fail "the cache directory holds ${cache_files:-0} files"
fi

if restore_tunables; then
    note "the shipped tunables are back"
else
    fail "restoring the shipped tunables produced no swap"
fi
sleep 1

say "23c. a sidecar tick rebuilds nothing"
# The `shape:` line times the three model passes, once per *model change*. A
# sidecar tick should cause **no** rebuild.
shape_lines=$(journal | grep -F 'built, composed and shaped in' || true)
if [[ -n "$shape_lines" ]]; then
    shape_ms=$(printf '%s\n' "$shape_lines" |
        /usr/bin/sed -n 's/.*shaped in \([0-9.]*\) ms.*/\1/p')
    note "$(printf '%s' "$shape_ms" | wc -l) model passes this run; $(
        printf '%s\n' "$shape_ms" | sort -g | head -1) ms fastest, $(
        printf '%s\n' "$shape_ms" | sort -g | tail -1) ms slowest"
    note "$(printf '%s\n' "$shape_lines" | tail -1 | /usr/bin/sed -n 's/.*caustics_bar::shape: //p')"
    pass "the three model passes are timed in the journal"
else
    fail "no 'built, composed and shaped' line; the model passes were never timed"
fi
# A tick (new numbers, same rows) is an upload and a redraw, never a build.
# Start from quiet: the previous step's last swap can land late and owe a rebuild.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    quiet_before=$(journal | grep -cF 'built, composed and shaped in' || true)
    sleep 1
    [[ "$(journal | grep -cF 'built, composed and shaped in' || true)" == "$quiet_before" ]] &&
        break
done
# Not across a minute (the clock's label is an owed rebuild), by the target's
# clock, and not from :00 (the minute timer fires up to a second late).
for _ in $(seq 14); do
    sec=$(on_target 5 'date +%-S')
    [[ -n "$sec" ]] && ((sec >= 2 && sec < 50)) && break
    sleep 1
done
# Each sidecar's row count before the window: a frame that changes it (a
# network coming into range) is a reshape and owes a rebuild.
ROWS_SHAPE_PY='
import re, sys
before, window = sys.argv[1], sys.argv[2]
pat = re.compile(r"sidecar ([^:]*): rows, applet=[0-9]+ rows=([0-9]+)")
last = {}
for m in pat.finditer(open(before).read()):
    last[m.group(1)] = m.group(2)
changed = []
for m in pat.finditer(open(window).read()):
    if last.get(m.group(1), m.group(2)) != m.group(2):
        changed.append(f"{m.group(1)} rows {last[m.group(1)]}->{m.group(2)}")
    last[m.group(1)] = m.group(2)
print(len(changed), " ".join(changed))
'
journal >"$SHOTS/tick-before.txt"
mark_journal
sleep 6
window=$(journal)
printf '%s\n' "$window" >"$SHOTS/tick-window.txt"
read -r reshaped reshapes <<<"$(python3 -c "$ROWS_SHAPE_PY" "$SHOTS/tick-before.txt" "$SHOTS/tick-window.txt")"
ticked=$(printf '%s\n' "$window" | grep -cF 'built, composed and shaped in' || true)
if [[ "${ticked:-0}" -eq 0 ]]; then
    pass "six seconds of sidecar ticks rebuilt nothing"
elif ((ticked <= ${reshaped:-0})); then
    pass "six seconds of sidecar ticks rebuilt nothing a tick caused: $ticked rebuild(s), each owed to a rows frame that changed shape ($reshapes)"
else
    fail "$ticked rebuilds in six idle seconds; a tick is reshaping the bar${reshapes:+ (reshaping frames: $reshapes)}"
    # What else the window held: a tick, the minute, or a late swap?
    note "$(printf '%s\n' "$window" | grep -vE 'layout:|hover:' | tail -20)"
fi

say "24. a protocol error exits 1 and the bar is restarted"
restarts_before=$(bar_restarts)
# The fault fires on every restart until cleared: a fresh cursor sees every
# cycle; the run's is restored afterwards for the verdict's dump.
run_cursor=$JOURNAL_CURSOR
mark_journal
bar_env fault "CAUSTICS_BAR_FAULT=protocol"
bar_restart
# Waited for: the bar's stderr can trail the supervisor's exit line.
if wait_for_journal "wayland protocol error" 60; then
    pass "the protocol error is in the journal verbatim"
else
    fail "no 'wayland protocol error' in the journal within 60 s"
    note "the journal's own account: $(journal | grep -F 'dispatch failed' | tail -1 | cut -c1-200)"
fi
# The compositor's message can be empty: `wl_surface` code 0 is invalid_scale.
if wait_for_journal "object_interface=wl_surface code=0" 20; then
    pass "and it is invalid_scale (wl_surface error code 0)"
else
    fail "not wl_surface code 0 within 20 s ($(journal |
        grep -F 'wayland protocol error' | tail -1 | cut -c1-200))"
fi
# The supervisor's exit line.
if wait_for_journal "status=1/FAILURE" 20; then
    pass "the process exited with status 1"
else
    fail "no 'status=1/FAILURE' in the journal within 20 s"
fi
log="$(journal)"
# `RestartSec=1`: the count moves a second after the exit line, so poll.
restarts_after=${restarts_before:-0}
for _ in $(seq 15); do
    restarts_after=$(bar_restarts)
    [[ "${restarts_after:-0}" -gt "${restarts_before:-0}" ]] && break
    sleep 1
done
if [[ "${restarts_after:-0}" -gt "${restarts_before:-0}" ]]; then
    pass "systemd restarted it ($restarts_before -> $restarts_after)"
else
    fail "NRestarts did not increase ($restarts_before -> $restarts_after)"
fi

# Clear the fault before the start limit (ten in sixty seconds) is tripped.
bar_env_clear fault
# A fresh cursor, or the fault cycles' own `layout: clock` lines would count.
mark_journal
bar_restart
if wait_for_journal "layout: clock" 40; then
    pass "the bar came back clean"
else
    fail "the bar did not come back after the fault"
fi
JOURNAL_CURSOR=$run_cursor
# The layout line precedes the surface's first composited frame.
sleep 2
shot recovered.png

say "25. a second bar on the same edge refuses with exit 2"
second_out=$(on_target 30 'gui_env; vk_env
    "$T_BIN/caustics-bar" --max-runtime 5 2>&1 | tail -5; echo "rc=${PIPESTATUS[0]}"' 2>&1)
if printf '%s' "$second_out" | grep -qF 'another caustics-bar holds the top edge; exiting'; then
    pass "a second bar on the same edge refused to start"
else
    fail "the second bar did not refuse (got: $(printf '%s' "$second_out" | tail -2))"
fi
if printf '%s' "$second_out" | grep -qF 'rc=2'; then
    pass "and it exited 2, which RestartPreventExitStatus=2 makes final"
else
    fail "the refusal did not exit 2 ($(printf '%s' "$second_out" | grep -o 'rc=[0-9]*'))"
fi

# The action and witness rings (docs/abi.md, *The action ring*, *The witness ring*) drop and count a
# push past capacity; the caps are sized so a whole run drops nothing.
say "25b. the two rings never overflowed"
JOURNAL_CURSOR=""
ring_log="$(journal)"
if grep -qF 'witness lost:' <<<"$ring_log"; then
    fail "the witness ring overflowed: $(grep -F 'witness lost:' <<<"$ring_log" | tail -1)"
else
    pass "no 'witness lost' line in the whole run"
fi
if grep -qF 'actions: ' <<<"$ring_log" && grep -qF "past the ring's" <<<"$ring_log"; then
    fail "the action ring overflowed: $(grep -F "past the ring's" <<<"$ring_log" | tail -1)"
else
    pass "and no action was dropped either"
fi

say "25c. a sidecar with another ABI hash is refused and not restarted"
# A first frame naming another contract hash is killed and **not restarted**
# (docs/abi.md, *Hello*): a backoff would replay it all session. The fake
# plays back a greeting with this bar's hash, every bit flipped.
BAR_HASH=$("$BINARY" --abi-hash 2>/dev/null)
if [[ "$BAR_HASH" =~ ^[0-9a-f]{16}$ ]]; then
    pass "the bar prints its ABI hash ($BAR_HASH)"
else
    fail "caustics-bar --abi-hash printed '${BAR_HASH:-nothing}'"
fi

# hello <file> <hex hash> <name>
hello_frame() {
    python3 - "$1" "$2" "$3" <<'PY'
import struct, sys
path, hexhash, name = sys.argv[1], sys.argv[2], sys.argv[3].encode()
value = int(hexhash, 16)
body = struct.pack("<IIII", value & 0xffffffff, value >> 32, len(name), 0) + name
open(path, "wb").write(struct.pack("<III", 0x53554143, 11, len(body)) + body)
PY
}

wrong=$(python3 -c "print(format(int('$BAR_HASH', 16) ^ 0xffffffffffffffff, '016x'))")
hello_frame "$SHOTS/not-ours.bin" "$wrong" "caustics-glyphs"
to_target "$SHOTS/not-ours.bin" not-ours.bin >/dev/null
on_target 20 'printf "#!/bin/sh\ncat $T_HOME/not-ours.bin\nexec sleep 300\n" > "$T_HOME/not-ours.sh"
    chmod +x "$T_HOME/not-ours.sh"' >/dev/null
# **Quoted**: systemd splits an unquoted `Environment=` on whitespace.
bar_env sidecars "CAUSTICS_SIDECARS=caustics-sysmon $U_HOME/not-ours.sh"
restart_bar "a sidecar from another tree"

if wait_for_journal "sidecar not-ours: refused" 30; then
    pass "refused: $(journal | grep -F 'sidecar not-ours: refused' | tail -1 |
        sed -n 's/.*feeders::pipe: //p' | cut -c1-160)"
else
    fail "no 'sidecar not-ours: refused …'; a foreign sidecar was read from"
fi
assert_contains "and the witness names both hashes" "$(journal)" "$BAR_HASH"
if wait_for_journal "sidecar not-ours: killed, and not started again" 20; then
    pass "the child was killed"
else
    fail "the refused child was not killed"
fi
# The health line, ten seconds in, says "refused" and closes the no-respawn
# window (a backoff would have restarted it several times by then).
if wait_for_journal "health (startup)" 40; then
    assert_contains "the health line says refused, not absent" \
        "$(journal | grep -F 'health (startup)' | tail -1)" "not-ours(refused"
else
    fail "no startup health line within 40 s"
fi
starts=$(journal_count "sidecar not-ours: started pid=")
if [[ "${starts:-0}" -eq 1 ]]; then
    pass "and started exactly once by the health line — no respawn"
else
    fail "the refused sidecar was started $starts times"
fi
assert_contains "the bar itself is fine" "$(bar_active)" active

bar_env_clear sidecars
restart_bar "the compiled-in sidecar set restored"

say "25d. install.sh installs a whole set, and a bar started through it runs that set"
# VM only: on a nested run it would install into the developer's home.
if vm_only "the install script runs only in the guest; on the nested target it would install into the developer's home"; then
# `--from` mode (the guest has no cargo): the set's name, the sixteen installs,
# the symlink, and a bar started through it finding its own set's sidecars.
GUEST_HOME=$(gssh 20 'printf %s "$HOME"')
gscp "$REPO/bar/install.sh" tester@127.0.0.1:install.sh >/dev/null
gssh 20 'chmod +x ~/install.sh; rm -rf ~/caustics-install-test' >/dev/null
install_out=$(gssh 120 '$HOME/install.sh --from $HOME/.local/bin --prefix $HOME/caustics-install-test 2>&1')
note "$(printf '%s' "$install_out" | tail -5)"
assert_contains "the script named the set" "$install_out" "set $BAR_HASH"
assert_contains "and installed sixteen binaries" "$install_out" "installed 16 of 16 into"
assert_contains "and did not restart a unit it does not point at" \
    "$install_out" "not restarting caustics-bar"

installed=$(gssh 20 "ls \$HOME/caustics-install-test/lib/caustics-bar/$BAR_HASH | wc -l")
if [[ "${installed:-0}" -eq 16 ]]; then
    pass "sixteen binaries in the set directory"
else
    fail "the set directory holds ${installed:-0} binaries, not sixteen"
fi
link=$(gssh 20 'readlink $HOME/caustics-install-test/bin/caustics-bar')
assert_contains "the symlink points into the set" "$link" "/lib/caustics-bar/$BAR_HASH/caustics-bar"
assert_contains "and the bar behind it is the same build" \
    "$(gssh 20 '$HOME/caustics-install-test/bin/caustics-bar --abi-hash')" "$BAR_HASH"

# `current_exe` is the resolved path, so the bar spawns *its own* set's
# sidecars. The unit's bar holds the edge, so it is stopped meanwhile.
bar_stop
trial=$(gssh 60 'export WAYLAND_DISPLAY=$(systemctl --user show-environment | sed -n "s/^WAYLAND_DISPLAY=//p")
    export XDG_RUNTIME_DIR=/run/user/$(id -u)
    export VK_DRIVER_FILES=$HOME/lvp_icd.json VK_LOADER_LAYERS_DISABLE="~all~"
    export CAUSTICS_DIR=$HOME/caustics-bar/caustics
    $HOME/caustics-install-test/bin/caustics-bar --max-runtime 6 2>&1 | head -60' 2>&1)
assert_contains "a bar started through the link says which set it is" \
    "$trial" "set: $GUEST_HOME/caustics-install-test/lib/caustics-bar/$BAR_HASH"
if grep -qE 'sidecar caustics-(sysmon|glyphs): started pid=' <<<"$trial"; then
    pass "and found its own set's sidecars beside itself"
else
    fail "the trial bar started no sidecar from its set"
fi
gssh 20 'rm -rf ~/caustics-install-test ~/install.sh' >/dev/null
mark_journal
bar_restart
if wait_for_journal "layout: clock" 60; then
    pass "the unit's bar is back"
else
    fail "the bar did not come back after the trial install"
fi

fi

say "25e. a malformed OpenType arena costs wrong pixels, never the device"
# VM only: a malformed arena can hang a real GPU's ring; a gate does not try
# that under the developer's session.
if vm_only "a malformed arena can hang a real GPU's ring; on lavapipe a hang is a slow process"; then
# A mismatched install's flattened `(codepoint, gid)` array under the OpenType
# arena's buffer number, handed to a *running* bar after the real font. The
# in-kernel deadline keeps the walk short, so the bar draws nonsense and must
# survive (the ABI-hash greeting prevents it in practice; the next step proves
# the watchdog). The array is the shape a `cmap` flattens to: one `u32`
# codepoint and one `u32` glyph id a pair, sorted by codepoint, over the ranges
# a Latin, Greek and Cyrillic UI font covers. Read as an arena, its first pair
# is a version and a face count.
python3 - "$SHOTS/bad-arena.bin" <<'PY'
import struct, sys
ranges = [(0x20, 0x7F), (0xA0, 0x250), (0x370, 0x400), (0x400, 0x530), (0x2000, 0x2C00)]
pairs = [cp for lo, hi in ranges for cp in range(lo, hi)]
with open(sys.argv[1], "wb") as out:
    for gid, cp in enumerate(pairs, start=3):
        out.write(struct.pack("<II", cp, gid))
PY
gscp "$SHOTS/bad-arena.bin" tester@127.0.0.1:bad-arena.bin >/dev/null
arena_bytes=$(gssh 20 'stat -c %s ~/bad-arena.bin 2>/dev/null || echo 0')
if [[ "${arena_bytes:-0}" -gt 1024 ]]; then
    pass "the mismatch arena is $arena_bytes bytes"
else
    fail "the mismatch arena was not written"
fi

# An accepted greeting, then a `Bytes` header for BUF_OT; the arena follows.
hello_frame "$SHOTS/bad-arena-prefix.bin" "$BAR_HASH" "caustics-glyphs"
python3 - "$SHOTS/bad-arena-prefix.bin" "$arena_bytes" <<'PY'
import struct, sys
path, size = sys.argv[1], int(sys.argv[2])
# WIRE_BYTES = 4; BytesHeader is {buffer, offset, len, total}; BUF_OT = 6.
head = struct.pack("<IIII", 6, 0, size, size)
frame = struct.pack("<III", 0x53554143, 4, len(head) + size) + head
open(path, "ab").write(frame)
PY
gscp "$SHOTS/bad-arena-prefix.bin" tester@127.0.0.1:bad-arena-prefix.bin >/dev/null
# The fake waits for the go file, touched once `caustics-glyphs` is done.
gssh 20 'rm -f ~/bad-arena.go
    printf "#!/bin/sh\nwhile [ ! -e \$HOME/bad-arena.go ]; do sleep 0.2; done\ncat \$HOME/bad-arena-prefix.bin \$HOME/bad-arena.bin\nexec sleep 300\n" > ~/bad-arena.sh
    chmod +x ~/bad-arena.sh' >/dev/null
bar_env sidecars "CAUSTICS_SIDECARS=caustics-glyphs caustics-apps $U_HOME/bad-arena.sh"
restart_bar "a sidecar writing an arena that is not one"
assert_contains "the shape pass is timed on this adapter" "$(journal)" \
    "gpu: the shape pass is timed on the device"

if wait_for_journal "glyphs: font sent:" 60; then
    note "caustics-glyphs has sent its font; releasing the fake"
else
    fail "caustics-glyphs never sent its font"
fi
gssh 20 'touch ~/bad-arena.go' >/dev/null
if wait_for_journal "sidecar bad-arena: hello" 60; then
    pass "the fake greeted with this bar's own hash and was read from"
else
    fail "the fake sidecar never greeted"
fi
# Wrong metrics may follow, but not a lost device or process; the startup
# health line says the loop came round after the arena landed.
if wait_for_journal "health (startup)" 40; then
    pass "the bar's loop came round after the arena: $(journal |
        grep -F 'health (startup)' | tail -1 | sed -n 's/.*health (startup)/health (startup)/p' |
        cut -c1-100)"
else
    fail "no startup health line after the bad arena"
fi
assert_contains "the bar is still up" "$(bar_active)" active
bad_arena_log="$(journal)"
if grep -qF 'the device is gone' <<<"$bad_arena_log"; then
    fail "an arena that is not one cost the bar its device"
else
    pass "and its device: a bad arena is wrong pixels, never a lost GPU"
fi
note "$(grep -F 'built, composed and shaped in' <<<"$bad_arena_log" | tail -1 |
    sed -n 's/.*caustics_bar::shape: //p')"
shot bad-arena.png

bar_env_clear sidecars
restart_bar "the compiled-in sidecar set restored"
fi

say "25f. the shape watchdog trips at its tunable and releases on a rewritten arena"
# `SHAPE_WATCHDOG_MS` reaches the embassy as an `ACT_WATCHDOG`; a timed shape
# pass over it stops shaping until the arena's front block is rewritten. 1 ms
# makes it fire; a restarted `caustics-glyphs` resending the arena releases it.
reloads_before=$(journal_count "$SWAPPED")
on_target 20 'cp $T_CAUSTICS/bar_tunables.slang $T_CAUSTICS/bar_tunables.slang.new
    sed -i -e "s/^public static const uint SHAPE_WATCHDOG_MS = .*/public static const uint SHAPE_WATCHDOG_MS = 1u;/" \
        $T_CAUSTICS/bar_tunables.slang.new
    mv -f $T_CAUSTICS/bar_tunables.slang.new $T_CAUSTICS/bar_tunables.slang' >/dev/null
if wait_for_journal_more "$SWAPPED" 60 "$reloads_before"; then
    pass "the tunable reached the GPU: $(journal | grep -F "$SWAPPED" | tail -1 |
        sed -n 's/.*caustics_bar::gpu: //p')"
else
    fail "no swap within 60 s of lowering SHAPE_WATCHDOG_MS"
fi
# On lavapipe `WATCHDOG_CPU_SCALE` (bar/src/gpu.rs) makes 1 ms 8 ms, which
# passes can meet: the trip is the nested gate's to prove.
watchdog_ms=1
[[ "$T_ADAPTER" == llvmpipe ]] && watchdog_ms=8
if wait_for_journal "gpu: a shape pass may take $watchdog_ms ms" 20; then
    pass "and the embassy was told so, as an ACT_WATCHDOG: $(journal | grep -F 'a shape pass may take' | tail -1 |
        sed -n 's/.*caustics_bar::gpu: //p')"
else
    fail "no 'a shape pass may take $watchdog_ms ms'; the threshold never left the caustic"
fi
if [[ "$watchdog_ms" == 1 ]]; then
    if wait_for_journal "shape watchdog:" 60; then
        pass "$(journal | grep -F 'shape watchdog:' | tail -1 |
            sed -n 's/.*caustics_bar::gpu: //p' | cut -c1-120)"
    else
        fail "the watchdog never fired at a threshold no pass can meet"
    fi
elif wait_for_journal "shape watchdog:" 10; then
    note "the watchdog fired even at $watchdog_ms ms: $(journal | grep -F 'shape watchdog:' | tail -1 |
        sed -n 's/.*caustics_bar::gpu: //p' | cut -c1-120)"
else
    note "no trip at $watchdog_ms ms on a software adapter, as expected; the nested gate proves the trip at 1 ms"
fi
assert_contains "the bar is still up" "$(bar_active)" active

# The release: end `caustics-glyphs` by pid; it restarts and resends the arena.
glyphs_pid=$(journal | grep -F 'sidecar caustics-glyphs: started pid=' | tail -1 |
    sed -n 's/.*pid=\([0-9]*\).*/\1/p')
if [[ -n "$glyphs_pid" ]]; then
    note "ending caustics-glyphs, pid $glyphs_pid, so it resends the arena"
    glyphs_starts=$(journal_count "sidecar caustics-glyphs: started pid=")
    target_kill "$glyphs_pid" >/dev/null
    if [[ "$watchdog_ms" != 1 ]]; then
        # No trip to release on a software adapter.
        if wait_for_journal_more "sidecar caustics-glyphs: started pid=" 60 "$glyphs_starts"; then
            pass "caustics-glyphs came back and resent the arena (no trip to release on a software adapter)"
        else
            fail "caustics-glyphs did not come back within 60 s of being ended"
        fi
    elif wait_for_journal "the arena's front block was rewritten" 60; then
        pass "and the bar shapes again once the arena is written"
    else
        fail "the watchdog never let go after the arena was rewritten"
    fi
else
    fail "no caustics-glyphs pid in the journal to end"
fi
shot watchdog.png

restore_tunables || fail "restoring the shipped SHAPE_WATCHDOG_MS produced no swap"
restart_bar "the shipped SHAPE_WATCHDOG_MS restored"

say "25g. moving the pointer along the bar stirs its wake; at rest the bar stops drawing"
# The wake (bar_tunables.slang, *the wake*) costs frames only while the
# pointer moves on the bar. A sweep in the background, so the first picture
# lands mid-motion; then the wake must say it is still within WAKE_SETTLE_S of
# the last move (plus the journal's slack), and the bar settle after it.
if [[ -z "$bar_origin_y" || ${#clock_rect[@]} -ne 4 ]]; then
    fail "no bar position or clock rect to sweep along"
else
    park_pointer
    wake_settle=$(sed -n 's/^public static const float WAKE_SETTLE_S = \([0-9.]*\);.*/\1/p' \
        "$REPO/caustics/bar_tunables.slang")
    band_y=$(python3 -c "print(round($bar_origin_y + ${clock_rect[1]}))")
    band_h=$(python3 -c "print(round(${clock_rect[3]}))")
    wake_y=$((band_y + band_h / 2))
    stirred_before=$(journal_count "wake: stirred")
    still_before=$(journal_count "wake: still")
    pointer_to 40 "$wake_y"
    sleep 1.5 # arriving is not moving; the hover springs settle
    sweep_start=$(now_us)
    (
        for x in $(seq 40 48 $((SCREEN_W - 40))) $(seq $((SCREEN_W - 40)) -48 40); do
            pointer_to "$x" "$wake_y"
        done
        now_us >"$SHOTS/.sweep-end"
    ) &
    sweep_pid=$!
    sleep 0.8
    shot wake-moving.png
    wait "$sweep_pid"
    sweep_end=$(cat "$SHOTS/.sweep-end")
    rm -f "$SHOTS/.sweep-end"
    note "the sweep: $(python3 -c "print(round(2 * ($SCREEN_W - 80) / (($sweep_end - $sweep_start) / 1e6)))") px/s on screen"
    if wait_for_journal_more "wake: stirred" 5 "$stirred_before"; then
        pass "the sweep stirred the wake"
    else
        fail "no 'wake: stirred' after a sweep along the bar"
    fi
    if wait_for_journal_more "wake: still" 10 "$still_before"; then
        still_at=$(journal -o short-unix | grep -F 'wake: still' | tail -1 | cut -d' ' -f1)
        still_after=$(python3 -c "print(f'{$still_at - $sweep_end / 1e6:.2f}')")
        if python3 -c "import sys; sys.exit(0 if $still_after <= $wake_settle + 1.0 else 1)"; then
            pass "the wake was still ${still_after} s after the last move (WAKE_SETTLE_S = $wake_settle)"
        else
            fail "the wake was still only ${still_after} s after the last move (WAKE_SETTLE_S = $wake_settle)"
        fi
    else
        fail "the wake never said it was still after the sweep"
    fi
    # The resting pointer is on the bar, so this is the at-rest bar, hover and all.
    sleep 1
    if journal | grep -F 'redraw:' | tail -1 | grep -qF 'settled'; then
        pass "and the bar stopped asking for frames: the newest redraw line says settled"
    else
        fail "the bar still draws with the pointer at rest: $(journal | grep -F 'redraw:' | tail -1)"
    fi
    shot wake-settled.png
    lit=$(png_pixel_diff "$SHOTS/wake-moving.png" "$SHOTS/wake-settled.png" 0 "$band_y" "$SCREEN_W" "$band_h")
    if [[ "$lit" -gt 400 ]]; then
        pass "the moving bar differs from the settled one in $lit px of the band: the wake was drawn"
    else
        fail "only $lit px of the band differ mid-sweep: the wake was not drawn"
    fi
    park_pointer
fi

# --- the canvas -----------------------------------------------------------
#
# The canvas host (docs/canvas.md) as a fixture beside the bar, with its two
# sidecars, on an empty workspace so that the background layer is what a
# screenshot sees below the bar. Last, so that nothing it does reaches the
# bar's steps, and torn down at the end. Its log is a file of its own, read
# through the target like the bar's journal.

canvas_log() { on_target 10 'cat "$T_HOME/canvas.log" 2>/dev/null'; }
canvas_count() { canvas_log | grep -cF -- "$1"; }
canvas_more() { [[ "$(canvas_count "$1")" -gt "$2" ]]; }
# A fade's last line, which is the only one that ends so.
canvas_dones() { canvas_log | grep -cE 'canvas: fade [0-9]+ done$'; }
canvas_last() { canvas_log | grep -F -- "$1" | tail -1; }
# Like wait_for_journal_more, for the canvas's log, polled every tenth of a
# second: the fade's midpoint is a moment, and a screenshot half a second
# after it is a quarter of the fade later.
canvas_wait_more() { # canvas_wait_more <pattern> <seconds> <before>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until canvas_more "$1" "$3"; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "$1" "$2"; return 1; }
        sleep 0.1
    done
    wait_charge "$t0" 0
}
# Solid 64x40 PNGs, written here and copied to the target: the sidecar fits
# nothing up, and the caustic covers the output with them.
canvas_png() { # canvas_png <name> <r> <g> <b>
    python3 - "$SHOTS/$1" "$2" "$3" "$4" <<'PY'
import struct, sys, zlib
path, rgb = sys.argv[1], bytes(int(v) for v in sys.argv[2:5])
raw = b"".join(b"\x00" + rgb * 64 for _ in range(40))
def chunk(tag, body):
    return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body) & 0xffffffff)
open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 40, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
PY
    to_target "$SHOTS/$1" "$1"
}
canvasctl() { on_target 60 "\"\$T_BIN/caustics-canvasctl\" $*"; }
# The rectangle the steps read: below the bar, inside the output.
CANVAS_RECT=(200 200 880 400)
canvas_mean() { png_mean "$SHOTS/$1" "${CANVAS_RECT[@]}"; }
within() { # within <got> <want> <tolerance>
    python3 -c "import sys; sys.exit(0 if abs($1 - $2) <= $3 else 1)"
}

say "27. the canvas comes up on the output and paints its dim colour"
CANVAS_RUN=0
if [[ "$T_NAME" == vm ]]; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "the canvas fixture runs on the nested target only"
    SKIPS=$((SKIPS + 1))
else
    CANVAS_RUN=1
    # An empty workspace: niri keeps one below the last that has a window.
    for _ in 1 2 3 4 5; do
        [[ "$(niri_msg --json focused-window 2>/dev/null)" == null ]] && break
        niri_msg action focus-workspace-down >/dev/null 2>&1
    done
    if [[ "$(niri_msg --json focused-window 2>/dev/null)" == null ]]; then
        pass "an empty workspace is focused; the background shows below the bar"
    else
        fail "no empty workspace to look at the background through"
    fi
    on_target 10 'rm -f "$T_HOME/canvas.log"; cp "$T_CAUSTICS/canvas_tunables.slang" "$T_HOME/canvas_tunables.slang.orig"' >/dev/null
    canvas_png canvas-red.png 255 0 0
    canvas_png canvas-blue.png 0 0 255
    bar_swaps_before=$(journal_count "$SWAPPED")
    # From the home dir, as the bar's supervisor starts the bar: caustics-reload
    # looks for a `caustics/` in its working directory before `$CAUSTICS_DIR`,
    # and from the checkout it would watch the checkout's.
    fixture_start canvas 'bash -c '"'"'cd "$1" && exec env CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload" RUST_LOG=info "$2/caustics-canvas" 2>>"$1/canvas.log"'"'"' canvas "$T_HOME" "$T_BIN"'
    if wait_until "canvas: no picture yet" 60 'canvas_more "canvas: no picture yet" 0'; then
        pass "$(canvas_last 'canvas: no picture yet' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the canvas never drew its first frame: $(canvas_log | tail -5 | tr '\n' '|' | cut -c1-500)"
    fi
    surfaces=$(canvas_count "a background surface")
    if [[ "$surfaces" -eq 1 ]] && canvas_log | jgrep -F "background surface configured"; then
        pass "one background surface, on the one output, configured"
    else
        fail "$surfaces background surfaces"
    fi
    if canvas_log | jgrep -E 'output [^ ]+: 1280x800 px at scale 240/120'; then
        pass "drawn at the output's own pixels: 1280x800 at scale 240/120, through the viewport"
    else
        fail "not drawn at 1280x800, scale 240/120: $(canvas_log | grep -E 'px at scale|scale' | tail -2 | tr '\n' '|')"
    fi
    shot canvas-blank.png
    dim=$(sed -n 's/^public static const float3 DIM_COLOUR = float3(\(.*\));.*/\1/p' \
        "$REPO/caustics/canvas_tunables.slang" | tr -d ' ')
    want=($(python3 -c "print(' '.join(str(round(float(v) * 255)) for v in '$dim'.split(',')))"))
    got=($(canvas_mean canvas-blank.png))
    if [[ ${#want[@]} -eq 3 ]] && within "${got[0]}" "${want[0]}" 6 && within "${got[1]}" "${want[1]}" 6 &&
        within "${got[2]}" "${want[2]}" 6; then
        pass "the canvas is the dim colour: mean ${got[*]}, DIM_COLOUR ${want[*]}"
    else
        fail "the canvas is ${got[*]}, not the dim colour ${want[*]:-?}"
    fi
fi

say "28. a shown picture covers the output"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    shows_before=$(canvas_count "canvas: show ")
    reply=$(canvasctl show '"$T_HOME/canvas-red.png"' 2>&1)
    if [[ $? -eq 0 ]]; then
        pass "caustics-canvasctl show exited 0"
    else
        fail "caustics-canvasctl show: $reply"
    fi
    if wait_until "canvas: show" 30 "canvas_more 'canvas: show ' $shows_before"; then
        pass "$(canvas_last 'canvas: show ' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the caustic never saw the show"
    fi
    shot canvas-red.png
    got=($(canvas_mean canvas-red.png))
    if ((got[0] >= 240 && got[1] <= 15 && got[2] <= 15)); then
        pass "the output is red: mean ${got[*]}"
    else
        fail "the output is ${got[*]}, not red"
    fi
fi

say "29. a four-second fade is purple at its midpoint"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    mids_before=$(canvas_count "midpoint")
    dones_before=$(canvas_dones)
    canvasctl show '"$T_HOME/canvas-blue.png"' fade 4 >/dev/null 2>&1 ||
        fail "caustics-canvasctl show … fade 4 failed"
    if canvas_wait_more midpoint 30 "$mids_before"; then
        shot canvas-mid.png
        pass "$(canvas_last midpoint | sed -n 's/.*INFO [a-z_:]*: //p')"
        got=($(canvas_mean canvas-mid.png))
        if ((got[0] >= 40 && got[0] <= 215 && got[2] >= 40 && got[2] <= 215 && got[1] <= 30)); then
            pass "the output is purple at the midpoint: mean ${got[*]}"
        else
            fail "the output is ${got[*]} at the midpoint, not purple"
        fi
    else
        fail "no midpoint witness within 30 s of a 4 s fade"
    fi
    status=$(canvasctl status 2>&1)
    assert_contains "status names the picture" "$status" "canvas-blue.png"
    if wait_until "canvas: fade done" 30 "[[ \$(canvas_dones) -gt $dones_before ]]"; then
        pass "$(canvas_log | grep -E 'canvas: fade [0-9]+ done$' | tail -1 | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the fade never said it was done"
    fi
    shot canvas-blue.png
    got=($(canvas_mean canvas-blue.png))
    if ((got[2] >= 240 && got[0] <= 15 && got[1] <= 15)); then
        pass "and then blue: mean ${got[*]}"
    else
        fail "the output is ${got[*]} after the fade, not blue"
    fi
fi

say "29b. a JPEG XL picture shows, and a broken one is the client's error"
if [[ "$CANVAS_RUN" -eq 1 ]] && ! command -v magick >/dev/null 2>&1; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "no magick here to write a JPEG XL with"
    SKIPS=$((SKIPS + 1))
elif [[ "$CANVAS_RUN" -eq 1 ]]; then
    # Solid green, lossless, 64x40 like the PNGs; and its first half, which
    # ends before the picture does.
    magick -size 64x40 xc:'#00ff00' -quality 100 "$SHOTS/canvas-green.jxl"
    head -c "$(($(stat -c %s "$SHOTS/canvas-green.jxl") / 2))" "$SHOTS/canvas-green.jxl" \
        >"$SHOTS/canvas-broken.jxl"
    to_target "$SHOTS/canvas-green.jxl" canvas-green.jxl
    to_target "$SHOTS/canvas-broken.jxl" canvas-broken.jxl
    shows_before=$(canvas_count "canvas: show ")
    reply=$(canvasctl show '"$T_HOME/canvas-green.jxl"' 2>&1)
    if [[ $? -eq 0 ]]; then
        pass "caustics-canvasctl show of a JPEG XL exited 0"
    else
        fail "caustics-canvasctl show of a JPEG XL: $reply"
    fi
    if wait_until "canvas: show" 30 "canvas_more 'canvas: show ' $shows_before"; then
        pass "$(canvas_last 'canvas: show ' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the caustic never saw the JPEG XL's show"
    fi
    shot canvas-green.png
    got=($(canvas_mean canvas-green.png))
    if ((got[1] >= 240 && got[0] <= 15 && got[2] <= 15)); then
        pass "the output is the JPEG XL's green: mean ${got[*]}"
    else
        fail "the output is ${got[*]}, not the JPEG XL's green"
    fi
    reply=$(canvasctl show '"$T_HOME/canvas-broken.jxl"' 2>&1)
    if [[ $? -eq 1 && "$reply" == *"JPEG XL"* ]]; then
        pass "a cut-short JPEG XL is refused with the decoder's reason: $reply"
    else
        fail "a cut-short JPEG XL was not refused as the client's error: $reply"
    fi
    # Blue again, cut, as step 29 left it: the steps after read it.
    shows_before=$(canvas_count "canvas: show ")
    canvasctl show '"$T_HOME/canvas-blue.png"' >/dev/null 2>&1 ||
        fail "caustics-canvasctl show of the blue picture again failed"
    if wait_until "canvas: show" 30 "canvas_more 'canvas: show ' $shows_before"; then
        pass "blue again for the steps after"
    else
        fail "the caustic never saw the blue picture shown again"
    fi
fi

say "30. a canvas tunables edit hot-reloads the canvas and not the bar"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    swaps_before=$(canvas_count "modules, swapped")
    on_target 20 'cp "$T_CAUSTICS/canvas_tunables.slang" "$T_CAUSTICS/canvas_tunables.slang.new"
        sed -i -e "s/^public static const float3 DIM_COLOUR = .*/public static const float3 DIM_COLOUR = float3(0.30, 0.10, 0.10);/" \
            "$T_CAUSTICS/canvas_tunables.slang.new"
        mv -f "$T_CAUSTICS/canvas_tunables.slang.new" "$T_CAUSTICS/canvas_tunables.slang"' >/dev/null
    if wait_until "canvas swap" 90 "canvas_more 'modules, swapped' $swaps_before"; then
        pass "$(canvas_last 'modules, swapped' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "no canvas swap within 90 s of saving canvas_tunables.slang"
    fi
    swaps_before=$(canvas_count "modules, swapped")
    on_target 20 'cp "$T_HOME/canvas_tunables.slang.orig" "$T_CAUSTICS/canvas_tunables.slang.new"
        mv -f "$T_CAUSTICS/canvas_tunables.slang.new" "$T_CAUSTICS/canvas_tunables.slang"' >/dev/null
    if wait_until "canvas swap back" 90 "canvas_more 'modules, swapped' $swaps_before"; then
        pass "restoring the shipped canvas tunables swapped again"
    else
        fail "no canvas swap after restoring canvas_tunables.slang"
    fi
    if [[ "$(journal_count "$SWAPPED")" -eq "$bar_swaps_before" ]]; then
        pass "the bar's caustic did not reload for the canvas's files"
    else
        fail "the bar swapped its caustic while only canvas files changed"
    fi
fi

say "31. a killed wallpaper sidecar restarts, and the picture stays"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    canvas_pid=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    wall_pid=$(canvas_last "sidecar caustics-wallpaper: started pid=" | grep -oE '[0-9]+$')
    parent=$(on_target 10 "awk '{print \$4}' /proc/${wall_pid:-0}/stat 2>/dev/null")
    if [[ -n "$wall_pid" && -n "$canvas_pid" && "$parent" == "$canvas_pid" ]]; then
        starts_before=$(canvas_count "sidecar caustics-wallpaper: started pid=")
        on_target 10 "kill $wall_pid" >/dev/null
        if wait_until "wallpaper restart" 30 "canvas_more 'sidecar caustics-wallpaper: started pid=' $starts_before"; then
            pass "$(canvas_last 'sidecar caustics-wallpaper: exited' | sed -n 's/.*INFO [a-z_:]*: //p;s/.*WARN [a-z_:]*: //p')"
            pass "$(canvas_last 'sidecar caustics-wallpaper: started pid=' | sed -n 's/.*INFO [a-z_:]*: //p')"
        else
            fail "the canvas did not restart caustics-wallpaper within 30 s"
        fi
        if wait_until "the session's picture again" 30 "canvas_more 'the session last showed' 0"; then
            pass "the new sidecar shows the session's picture again"
        else
            fail "the restarted sidecar did not show the last picture"
        fi
        reply=$(on_target 20 'for i in $(seq 40); do "$T_BIN/caustics-canvasctl" status && break; sleep 0.25; done' 2>&1)
        assert_contains "its socket answers again" "$reply" "canvas-blue.png"
        shot canvas-after-restart.png
        got=($(canvas_mean canvas-after-restart.png))
        if ((got[2] >= 240 && got[0] <= 15 && got[1] <= 15)); then
            pass "the picture stayed blue: mean ${got[*]}"
        else
            fail "the picture is ${got[*]} after the sidecar's restart"
        fi
    else
        fail "no wallpaper pid that is the canvas's child (pid ${wall_pid:-?}, parent ${parent:-?}, canvas ${canvas_pid:-?})"
    fi
fi

# --- the fade -----------------------------------------------------------------
#
# The dim (docs/canvas.md, *How each job is wired*): an overlay per output over
# every window and the bar, which any input cancels, and a client that waits
# on it with chayang's exit codes. Each step dims only inside itself and ends
# undimmed, with the picture blue as step 31 left it.

# The sidecar's `status` says the dim is at least <fraction> of its way: how a
# step spaces its screenshots across a dim without sleeping a guess.
canvas_dim_past() { # canvas_dim_past <fraction>
    local p
    p=$(on_target 5 '"$T_BIN/caustics-canvasctl" status' 2>/dev/null |
        sed -n 's/.*dim=dimming progress=\([0-9.]*\).*/\1/p')
    [[ -n "$p" ]] && python3 -c "import sys; sys.exit(0 if $p >= $1 else 1)"
}
canvas_wait_dim() { # canvas_wait_dim <fraction> <seconds>
    local deadline=$((SECONDS + $2)) t0
    t0=$(now_us)
    until canvas_dim_past "$1"; do
        ((SECONDS < deadline)) || { wait_charge "$t0" 1 "dim progress $1" "$2"; return 1; }
        sleep 0.05
    done
    wait_charge "$t0" 0
}
# A waited dim in the background: its exit status lands in `$SHOTS/<name>`
# when the client returns.
canvas_dim_wait() { # canvas_dim_wait <name> <alpha> <seconds>
    rm -f "${SHOTS:?}/${1:?}"
    (
        on_target 60 "\"\$T_BIN/caustics-canvasctl\" dim $2 over $3 --wait" >/dev/null 2>&1
        echo $? >"$SHOTS/$1.tmp" && mv -f "$SHOTS/$1.tmp" "$SHOTS/$1"
    ) &
}
# Wait at most <ms> milliseconds, polled every 20 ms, for the client to
# return: its exit status in CANVAS_EXIT_CODE and the milliseconds the wait
# took in CANVAS_EXIT_MS; 1, and an empty code, when it has not.
CANVAS_EXIT_CODE=""
CANVAS_EXIT_MS=0
canvas_exit_within() { # canvas_exit_within <name> <ms>
    local t0 deadline
    t0=$(now_us)
    deadline=$((t0 + $2 * 1000))
    CANVAS_EXIT_CODE=""
    until [[ -s "$SHOTS/$1" ]]; do
        (($(now_us) < deadline)) || { wait_charge "$t0" 1 "$1 exits" "$(($2 / 1000))"; return 1; }
        sleep 0.02
    done
    wait_charge "$t0" 0
    CANVAS_EXIT_MS=$((($(now_us) - t0) / 1000))
    CANVAS_EXIT_CODE=$(<"$SHOTS/$1")
}
# The overlay is up on the one output: its surface configured, after <before>
# lines of it.
canvas_overlay_up() { # canvas_overlay_up <before>
    wait_until "the overlay" 10 "canvas_more 'overlay surface configured' $1"
}
# As many overlays gone as made.
canvas_no_overlay() {
    [[ "$(canvas_count "an overlay surface")" -eq "$(canvas_count "its overlay surface is gone")" ]]
}
# No overlay remains, and the picture is blue again.
canvas_undimmed() { # canvas_undimmed <shot name> <what>
    local made gone got
    wait_until "the overlay gone" 10 canvas_no_overlay
    made=$(canvas_count "an overlay surface")
    gone=$(canvas_count "its overlay surface is gone")
    shot "$1"
    got=($(canvas_mean "$1"))
    if [[ "$made" -eq "$gone" ]] && ((got[2] >= 240 && got[0] <= 15 && got[1] <= 15)); then
        pass "$2: no overlay remains ($made made, $gone gone) and the picture is blue again: mean ${got[*]}"
    else
        fail "$2: $made overlays made, $gone gone, and the output is ${got[*]}"
    fi
}

say "32. a waited dim dissolves into the wallpaper over its time, and its client exits 0"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    # The overlay draws the background's own picture, darkened towards
    # DIM_COLOUR by DIM_DARKEN, at the dim's alpha (docs/canvas.md, *Surfaces*):
    # at its depth the rectangle is the bare wallpaper's, darkened, whatever
    # was over it. The bar is outside CANVAS_RECT, and covered too.
    dim=$(sed -n 's/^public static const float3 DIM_COLOUR = float3(\(.*\));.*/\1/p' \
        "$REPO/caustics/canvas_tunables.slang" | tr -d ' ')
    darken=$(sed -n 's/^public static const float DIM_DARKEN = \([0-9.]*\);.*/\1/p' \
        "$REPO/caustics/canvas_tunables.slang")
    park_pointer
    shot dim-before.png
    before=($(canvas_mean dim-before.png))
    want=($(python3 -c "
d = [float(v) * 255 for v in '$dim'.split(',')]
b = [float(v) for v in '${before[*]}'.split()]
k = float('${darken:-nan}')
print(' '.join(str(round(b[i] + (d[i] - b[i]) * k)) for i in range(3)))" 2>/dev/null))
    overlays_before=$(canvas_count "overlay surface configured")
    reached_before=$(canvas_count "reached 1000/1000")
    dim_t0=$(now_us)
    canvas_dim_wait .dim-done 1 3
    if canvas_overlay_up "$overlays_before"; then
        pass "$(canvas_last 'overlay surface configured' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "no overlay surface within 10 s of a dim"
    fi
    shot_capture dim-early.png
    canvas_wait_dim 0.55 10 && shot_capture dim-middle.png
    canvas_wait_dim 0.9 10 && shot_capture dim-late.png
    canvas_exit_within .dim-done 8000
    code=$CANVAS_EXIT_CODE
    took=$((($(now_us) - dim_t0) / 1000))
    if [[ "$code" == 0 ]] && ((took <= 3000 + 3000)); then
        pass "caustics-canvasctl dim 1 over 3 --wait exited 0, $took ms after it was run"
    else
        fail "caustics-canvasctl dim 1 over 3 --wait: exit '${code:-none}' after $took ms"
    fi
    if canvas_wait_more "reached 1000/1000" 5 "$reached_before"; then
        pass "$(canvas_last 'reached 1000/1000' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the caustic never said the dim reached its depth"
    fi
    shot dim-deep.png
    for name in dim-early.png dim-middle.png dim-late.png; do
        [[ -e "$RUN/.shot-raw-$name" ]] && shot_develop "$name"
    done
    early=($(canvas_mean dim-early.png))
    middle=($(canvas_mean dim-middle.png))
    late=($(canvas_mean dim-late.png))
    # With nothing over the wallpaper here, what moves is the darkening: each
    # shot is at least an eighth of the whole darkening's blue below the one
    # before (the shots are a quarter or more of the dim apart, and a solid
    # picture's mean has no noise). DIM_DARKEN 0 is
    # the windows simply going: then nothing here moves at all.
    step=$(python3 -c "print(max(0, round(0.12 * (${before[2]:-0} - ${want[2]:-0}))))")
    if [[ "$step" -gt 0 ]] && ((early[2] > middle[2] + step && middle[2] > late[2] + step)); then
        pass "the output darkens across the dim: blue ${before[2]}, then ${early[2]}, ${middle[2]}, ${late[2]} (steps over $step)"
    elif [[ "$step" -eq 0 ]] && within "${early[2]}" "${before[2]}" 6 && within "${late[2]}" "${before[2]}" 6; then
        pass "DIM_DARKEN is 0: the bare wallpaper stays as it is across the dim (blue ${early[2]}, ${late[2]})"
    else
        fail "the output does not darken across the dim as DIM_DARKEN $darken says: ${early[*]} | ${middle[*]} | ${late[*]}"
    fi
    deep=($(canvas_mean dim-deep.png))
    if [[ ${#want[@]} -eq 3 ]] && within "${deep[0]}" "${want[0]}" 6 &&
        within "${deep[1]}" "${want[1]}" 6 && within "${deep[2]}" "${want[2]}" 6; then
        pass "at its depth the output is the wallpaper darkened by $darken: mean ${deep[*]}, from ${before[*]}, want ${want[*]}"
    else
        fail "at its depth the output is ${deep[*]}, not the wallpaper ${before[*]} darkened by ${darken:-?} (${want[*]:-?})"
    fi
    status=$(canvasctl status 2>&1)
    assert_contains "a completed dim stays at its depth" "$status" "dim=dimmed"
    canvasctl undim >/dev/null 2>&1 || fail "caustics-canvasctl undim failed"
    canvas_undimmed dim-undone.png "after the undim"
fi

say "33. the pointer moving over a dim cancels it, and its client exits 2"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    pointer_to 640 500
    overlays_before=$(canvas_count "overlay surface configured")
    canvas_dim_wait .dim-pointer 1 6
    canvas_overlay_up "$overlays_before" || fail "no overlay surface within 10 s of a dim"
    if canvas_wait_dim 0.15 10; then
        pointer_to 700 540
        canvas_exit_within .dim-pointer 500
        code=$CANVAS_EXIT_CODE
        if [[ "$code" == 2 ]]; then
            pass "a pointer motion about a second in: the client exited 2, $CANVAS_EXIT_MS ms later"
        else
            fail "a pointer motion over the dim: exit '${code:-none}' within 500 ms"
        fi
    else
        fail "the dim never reached 0.15 of its way"
    fi
    canvas_exit_within .dim-pointer 8000
    if canvas_log | jgrep -F "cancelled by the pointer moving"; then
        pass "$(canvas_last 'cancelled by the pointer moving' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the host never said the pointer cancelled the dim"
    fi
    canvas_undimmed dim-pointer-after.png "after the pointer"
fi

say "34. a key pressed during a dim cancels it, and its client exits 2"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    overlays_before=$(canvas_count "overlay surface configured")
    canvas_dim_wait .dim-key 1 6
    canvas_overlay_up "$overlays_before" || fail "no overlay surface within 10 s of a dim"
    if canvas_wait_dim 0.15 10; then
        keys "x"
        canvas_exit_within .dim-key 500
        code=$CANVAS_EXIT_CODE
        if [[ "$code" == 2 ]]; then
            pass "one key about a second in: the client exited 2, $CANVAS_EXIT_MS ms later"
        else
            fail "a key during the dim: exit '${code:-none}' within 500 ms"
        fi
    else
        fail "the dim never reached 0.15 of its way"
    fi
    canvas_exit_within .dim-key 8000
    if canvas_log | jgrep -F "cancelled by a key"; then
        pass "$(canvas_last 'cancelled by a key' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the host never said a key cancelled the dim"
    fi
    canvas_undimmed dim-key-after.png "after the key"
fi

say "35. a wallpaper sidecar killed mid-dim takes its dim with it, and restarts"
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    canvas_pid=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    wall_pid=$(canvas_last "sidecar caustics-wallpaper: started pid=" | grep -oE '[0-9]+$')
    parent=$(on_target 10 "awk '{print \$4}' /proc/${wall_pid:-0}/stat 2>/dev/null")
    overlays_before=$(canvas_count "overlay surface configured")
    canvas_dim_wait .dim-killed 1 6
    canvas_overlay_up "$overlays_before" || fail "no overlay surface within 10 s of a dim"
    if [[ -n "$wall_pid" && -n "$canvas_pid" && "$parent" == "$canvas_pid" ]] && canvas_wait_dim 0.3 10; then
        starts_before=$(canvas_count "sidecar caustics-wallpaper: started pid=")
        on_target 10 "kill $wall_pid" >/dev/null
        canvas_exit_within .dim-killed 5000
        code=$CANVAS_EXIT_CODE
        if [[ "$code" == 1 ]]; then
            pass "the waiting client got the socket's end and exited 1, $CANVAS_EXIT_MS ms after the kill"
        else
            fail "the waiting client of a killed sidecar: exit '${code:-none}'"
        fi
        if wait_until "the dim gone with its sidecar" 10 "canvas_more 'gone mid-dim' 0"; then
            pass "$(canvas_last 'gone mid-dim' | sed -n 's/.*INFO [a-z_:]*: //p')"
        else
            fail "the host never said the dim went with its sidecar"
        fi
        canvas_undimmed dim-killed-after.png "after the sidecar died"
        if wait_until "wallpaper restart" 30 "canvas_more 'sidecar caustics-wallpaper: started pid=' $starts_before"; then
            pass "$(canvas_last 'sidecar caustics-wallpaper: started pid=' | sed -n 's/.*INFO [a-z_:]*: //p')"
        else
            fail "the canvas did not restart caustics-wallpaper within 30 s"
        fi
        reply=$(on_target 20 'for i in $(seq 40); do "$T_BIN/caustics-canvasctl" status && break; sleep 0.25; done' 2>&1)
        assert_contains "the restarted sidecar has no dim" "$reply" "dim=none"
    else
        fail "no wallpaper pid that is the canvas's child, or the dim never got under way (pid ${wall_pid:-?}, parent ${parent:-?}, canvas ${canvas_pid:-?})"
        canvasctl undim >/dev/null 2>&1
    fi
    canvas_exit_within .dim-killed 8000
    # Torn down: the host, and with it its sidecars and the socket.
    fixture_stop canvas
    if wait_until "the canvas gone" 20 "! on_target 5 '[ -S \"\$XDG_RUNTIME_DIR/caustics-canvas.sock\" ] || kill -0 ${canvas_pid:-0} 2>/dev/null'"; then
        pass "the canvas stopped, and its sidecar took its socket with it"
    else
        fail "the canvas or its socket outlived the fixture"
    fi
    niri_msg action focus-workspace-up >/dev/null 2>&1
fi

say "35b. the polkit sink asks through the wire, and the answer reaches its helper alone"
# `caustics-polkit` in its dry run (polkit/README.md): the agent object on the
# run's private session bus, no system bus at all (its address names a
# socket that does not exist), `polkit/tests/fake-helper.sh` as the helper,
# and `bar/abi/examples/fake_host.rs` on its pipes playing the canvas host,
# which answers the first ask with the secret and dismisses the second. This
# step plays polkitd with `busctl`. The secret is this run's own, and no log
# may hold it.
if [[ "$T_NAME" == vm ]]; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "the polkit dry run and its fake host run on the nested target only"
    SKIPS=$((SKIPS + 1))
else
    polkit_secret="gate-$RANDOM$RANDOM-$RANDOM"
    polkit_log() { on_target 10 'cat "$T_HOME/polkit.log" 2>/dev/null'; }
    to_target "$REPO/polkit/tests/fake-helper.sh" polkit-fake-helper.sh
    on_target 10 'chmod +x "$T_HOME/polkit-fake-helper.sh"
        rm -f "$T_HOME/polkit.log" "$T_HOME/polkit-record" "$T_HOME/polkit-cookies"' >/dev/null
    fixture_start polkit 'bash -c '"'"'cd "$1" && exec env CAUSTICS_POLKIT_DRY_RUN=1 \
        CAUSTICS_POLKIT_HELPER="$1/polkit-fake-helper.sh" \
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=$1/no-system-bus" \
        FAKE_HELPER_SECRET="$3" FAKE_HELPER_RECORD="$1/polkit-record" \
        FAKE_HELPER_COOKIES="$1/polkit-cookies" \
        FAKE_HOST_SECRET="$3" FAKE_HOST_SCRIPT="text cancel" \
        "$2/examples/fake_host" "$2/caustics-polkit" >>"$1/polkit.log" 2>&1'"'"' polkit "$T_HOME" "$T_BIN" '"$polkit_secret"
    polkit_dest=""
    if wait_until "the polkit sink's dry run" 30 'polkit_log | jgrep -F "dry run: the agent is at"'; then
        polkit_dest=$(polkit_log | sed -n 's/.*dry run: the agent is at \([^ ]*\) .*/\1/p' | tail -1)
        pass "$(polkit_log | grep -F 'dry run:' | tail -1 | sed 's/^caustics-polkit: //' | cut -c1-140)"
    else
        fail "caustics-polkit never said where its agent is: $(polkit_log | tail -3 | tr '\n' '|' | cut -c1-300)"
    fi
    # polkit_call <cookie> <message>: BeginAuthentication as polkitd makes
    # it, for this uid; the reply and busctl's exit status.
    polkit_call() {
        on_target 40 "busctl --user call --timeout=30 '$polkit_dest' \
            /org/freedesktop/PolicyKit1/AuthenticationAgent \
            org.freedesktop.PolicyKit1.AuthenticationAgent BeginAuthentication \
            'sssa{ss}sa(sa{sv})' org.caustics.test.polkit '$2' '' 0 '$1' \
            1 unix-user 1 uid u \$(id -u) 2>&1; echo \"exit=\$?\""
    }
    if [[ -n "$polkit_dest" ]]; then
        reply=$(polkit_call cookie-1 "The nested gate asks")
        assert_contains "BeginAuthentication answered with the secret returned" "$reply" "exit=0"
        log=$(polkit_log)
        assert_contains "the title is polkit's message" "$log" \
            'prompt id=1 kind=title arg=1 message="The nested gate asks"'
        assert_contains "a PROMPT_SECRET arrived with the fake helper's message" "$log" \
            'prompt id=1 kind=secret arg=0 message="Password: "'
        assert_contains "the host answered it" "$log" "answered id=1 with text"
        assert_contains "and PROMPT_DONE said success" "$log" 'prompt id=1 kind=done arg=1'
        record=$(on_target 10 'cat "$T_HOME/polkit-record" 2>/dev/null')
        if [[ "$record" == "$polkit_secret" ]]; then
            pass "the fake helper read exactly the secret, and nothing else"
        else
            fail "the fake helper's record is ${#record} bytes, not the ${#polkit_secret}-byte secret"
        fi

        reply=$(polkit_call cookie-2 "The nested gate asks again")
        if [[ "$reply" != *"exit=0"* && "$reply" == *"dismissed"* ]]; then
            pass "a dismissed prompt fails the call: $(printf '%s' "$reply" | head -1 | cut -c1-100)"
        else
            fail "the dismissed call: $(printf '%s' "$reply" | tr '\n' '|' | cut -c1-200)"
        fi
        log=$(polkit_log)
        assert_contains "the host dismissed the second ask" "$log" "answered id=2 with cancel"
        assert_contains "and PROMPT_DONE said failure" "$log" 'prompt id=2 kind=done arg=0'
        record=$(on_target 10 'cat "$T_HOME/polkit-record" 2>/dev/null')
        cookies=$(on_target 10 'cat "$T_HOME/polkit-cookies" 2>/dev/null | tr "\n" " "')
        if [[ "$record" == "$polkit_secret" && "$cookies" == "cookie-1 cookie-2 " ]]; then
            pass "the dismissed helper read no answer; each helper read its own cookie"
        else
            fail "after the dismissal the record is ${#record} bytes and the cookies are '$cookies'"
        fi
    fi
    leaks=$(on_target 10 "grep -cF -- '$polkit_secret' \"\$T_HOME/polkit.log\"")
    if [[ "${leaks:-1}" -eq 0 ]] && ! journal | jgrep -F -- "$polkit_secret"; then
        pass "no witness, no host line and no bar line holds the secret"
    else
        fail "the secret is in ${leaks:-?} lines of the sink's log, or in the bar's journal"
    fi
    if polkit_log | jgrep -iE 'system-bus|system bus: '; then
        fail "the dry run spoke of the system bus: $(polkit_log | grep -iE 'system-bus|system bus: ' | head -1 | cut -c1-160)"
    else
        pass "the dry run never reached for the system bus"
    fi
    # The fake host goes, and the sink leaves with it: stdin's EOF.
    polkit_pid=$(polkit_log | sed -n 's/^fake-host: started .* as pid \([0-9]*\)$/\1/p' | tail -1)
    fixture_stop polkit
    if [[ -n "$polkit_pid" ]] &&
        wait_until "the polkit sink gone" 20 "! on_target 5 'kill -0 $polkit_pid 2>/dev/null'"; then
        pass "the sink exited when its host went: $(polkit_log | grep -F 'stdin closed' | tail -1 | sed 's/^caustics-polkit: //')"
    else
        fail "caustics-polkit (pid ${polkit_pid:-?}) outlived its host"
    fi
fi

say "35c. a polkit prompt rises on the canvas, over the darkened wallpaper, and takes the keyboard"
# The canvas again, now with its glyphs sidecar and `caustics-polkit` in its
# set, the sink in the dry run 35b uses (the agent on the run's session bus,
# no system bus, `polkit/tests/fake-helper.sh` as the helper). The canvas
# host is the one answering now: these steps play polkitd with `busctl` and
# the user with the keyboard. A wrong secret is an error line and a fresh ask
# in the same call (the sink runs its helper again, up to three times), and
# the right one then authorizes it. Both secrets are this run's own, and no
# log may hold either.
PROMPT_RUN=0
if [[ "$CANVAS_RUN" -ne 1 ]]; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "the canvas fixture runs on the nested target only"
    SKIPS=$((SKIPS + 1))
else
    PROMPT_RUN=1
    prompt_secret="gate-$RANDOM$RANDOM-$RANDOM"
    prompt_wrong="nope-$RANDOM$RANDOM-$RANDOM"
    for _ in 1 2 3 4 5; do
        [[ "$(niri_msg --json focused-window 2>/dev/null)" == null ]] && break
        niri_msg action focus-workspace-down >/dev/null 2>&1
    done
    to_target "$REPO/polkit/tests/fake-helper.sh" prompt-fake-helper.sh
    on_target 10 'chmod +x "$T_HOME/prompt-fake-helper.sh"
        rm -f "$T_HOME/canvas.log" "$T_HOME/prompt-record" "$T_HOME/prompt-cookies"' >/dev/null
    fixture_start prompt 'bash -c '"'"'cd "$1" && exec env \
        CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs caustics-polkit" \
        CAUSTICS_POLKIT_DRY_RUN=1 CAUSTICS_POLKIT_HELPER="$1/prompt-fake-helper.sh" \
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=$1/no-system-bus" \
        FAKE_HELPER_SECRET="$3" FAKE_HELPER_RECORD="$1/prompt-record" \
        FAKE_HELPER_COOKIES="$1/prompt-cookies" \
        RUST_LOG=info "$2/caustics-canvas" 2>>"$1/canvas.log"'"'"' prompt "$T_HOME" "$T_BIN" '"$prompt_secret"
    prompt_dest=""
    if wait_until "the canvas's polkit sink" 60 'canvas_log | jgrep -F "dry run: the agent is at"'; then
        prompt_dest=$(canvas_log | sed -n 's/.*dry run: the agent is at \([^ ]*\) .*/\1/p' | tail -1)
        pass "the canvas started caustics-polkit in its dry run, at $prompt_dest"
    else
        fail "the canvas's caustics-polkit never said where its agent is: $(canvas_log | tail -4 | tr '\n' '|' | cut -c1-400)"
        PROMPT_RUN=0
    fi
    if wait_until "the canvas's font" 30 'canvas_log | jgrep -F "font: "'; then
        pass "$(canvas_last 'font: ' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the canvas's caustics-glyphs never sent a font"
    fi
    shows_before=$(canvas_count "canvas: show ")
    canvasctl show '"$T_HOME/canvas-blue.png"' >/dev/null 2>&1 || fail "caustics-canvasctl show failed"
    wait_until "the prompt step's picture" 30 "canvas_more 'canvas: show ' $shows_before" ||
        fail "the canvas never showed the picture"
fi
# prompt_call <n> <message>: BeginAuthentication as polkitd makes it, in the
# background; busctl's output and exit status land in `$SHOTS/prompt-call-<n>`
# when the conversation ends.
prompt_call() {
    rm -f "$SHOTS/prompt-call-$1"
    (
        out=$(on_target 60 "busctl --user call --timeout=45 '$prompt_dest' \
            /org/freedesktop/PolicyKit1/AuthenticationAgent \
            org.freedesktop.PolicyKit1.AuthenticationAgent BeginAuthentication \
            'sssa{ss}sa(sa{sv})' org.caustics.test.prompt '$2' '' 0 'prompt-cookie-$1' \
            1 unix-user 1 uid u \$(id -u) 2>&1; echo \"exit=\$?\"")
        printf '%s\n' "$out" >"$SHOTS/prompt-call-$1.tmp" && mv -f "$SHOTS/prompt-call-$1.tmp" "$SHOTS/prompt-call-$1"
    ) &
}
prompt_reply() { cat "$SHOTS/prompt-call-$1" 2>/dev/null; }
# No prompt pane remains: as many gone as made.
prompt_panes_gone() {
    [[ "$(canvas_count "a prompt surface")" -eq "$(canvas_count "its prompt surface is gone")" ]]
}
# Typing waits for the keyboard to be on the prompt's pane: keys sent before
# would go to whatever had the focus.
prompt_focused() { # prompt_focused <before>
    wait_until "the keyboard on the prompt" 15 "canvas_more 'seat: the keyboard is on the prompt' $1"
}
if [[ "$PROMPT_RUN" -eq 1 ]]; then
    panes_before=$(canvas_count "prompt surface configured")
    focus_before=$(canvas_count "seat: the keyboard is on the prompt")
    prompt_call 1 "The nested gate asks the canvas"
    if wait_until "the prompt asking" 30 'canvas_log | jgrep -F "prompt: conversation 1 from caustics-polkit began"' &&
        wait_until "the prompt's pane" 15 "canvas_more 'prompt surface configured' $panes_before" &&
        wait_until "the prompt drawn" 15 'canvas_log | jgrep -E "canvas[a-z_:]*: prompt: conversation 1 asking \(echo 0, attempts 0\)"'; then
        pass "$(canvas_last 'prompt surface configured' | sed -n 's/.*INFO [a-z_:]*: //p' | cut -c1-120)"
        pass "the caustic drew conversation 1 asking, echo off, no attempts yet"
    else
        fail "no prompt pane for conversation 1: $(canvas_log | grep -F 'prompt' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    # The rectangles: one inside the panel (PROMPT_WIDTH wide, centred, at
    # least a title, a row and a field tall), one in a corner of the output
    # outside it, which is the wallpaper darkened towards DIM_COLOUR by
    # PROMPT_DARKEN.
    sleep 0.5
    shot prompt-up.png
    darken=$(sed -n 's/^public static const float PROMPT_DARKEN = \([0-9.]*\);.*/\1/p' \
        "$REPO/caustics/canvas_tunables.slang")
    dim=$(sed -n 's/^public static const float3 DIM_COLOUR = float3(\(.*\));.*/\1/p' \
        "$REPO/caustics/canvas_tunables.slang" | tr -d ' ')
    want=($(python3 -c "
d = [float(v) * 255 for v in '$dim'.split(',')]
print(' '.join(str(round(p + (q - p) * $darken)) for p, q in zip((0, 0, 255), d)))"))
    outside=($(png_mean "$SHOTS/prompt-up.png" 40 680 160 80))
    inside=($(png_mean "$SHOTS/prompt-up.png" 300 390 680 20))
    if [[ ${#want[@]} -eq 3 ]] && within "${outside[0]}" "${want[0]}" 6 &&
        within "${outside[1]}" "${want[1]}" 6 && within "${outside[2]}" "${want[2]}" 6; then
        pass "outside the panel the output is the wallpaper darkened by $darken: mean ${outside[*]}, want ${want[*]}"
    else
        fail "outside the panel the output is ${outside[*]}, not the wallpaper darkened by ${darken:-?} (${want[*]:-?})"
    fi
    if [[ ${#inside[@]} -eq 3 ]] && ! within "${inside[2]}" "${want[2]}" 30; then
        pass "inside the panel it is not the darkened wallpaper: mean ${inside[*]}"
    else
        fail "inside the panel the output is ${inside[*]}, the darkened wallpaper's ${want[*]:-?}: no panel"
    fi
fi

say "35d. a wrong secret typed into the canvas is an error line and a fresh ask, in the same call"
if [[ "$PROMPT_RUN" -eq 1 ]]; then
    if prompt_focused "$focus_before"; then
        pass "$(canvas_last 'seat: the keyboard is on the prompt' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the keyboard never came to the prompt's pane"
    fi
    keys "$prompt_wrong"
    key Return
    if wait_until "the wrong answer's error line" 20 'canvas_log | jgrep -F "prompt: conversation 1: an error line after the answer (attempts 1)"'; then
        pass "the helper's error line came back after the answer: attempts 1"
    else
        fail "no error line after conversation 1's answer: $(canvas_log | grep -F 'prompt' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    if wait_until "the fresh ask" 20 'canvas_log | jgrep -E "canvas[a-z_:]*: prompt: conversation 1 asking \(echo 0, attempts 1\)"'; then
        pass "the sink ran its helper again: conversation 1 asks again, attempts 1"
    else
        fail "no fresh ask after the wrong secret: $(canvas_log | grep -F 'prompt' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    if [[ ! -s "$SHOTS/prompt-call-1" ]] && ! prompt_panes_gone; then
        pass "the call is still open and the panel still up"
    else
        fail "the wrong secret ended the call ($(prompt_reply 1 | head -1 | cut -c1-80)) or took the panel down"
    fi
fi

say "35e. the right secret typed into the canvas authorizes the same call, and reaches the helper alone"
if [[ "$PROMPT_RUN" -eq 1 ]]; then
    keys "$prompt_secret"
    key Return
    if wait_until "the authorized call" 30 '[[ -s "$SHOTS/prompt-call-1" ]]' && [[ "$(prompt_reply 1)" == *"exit=0"* ]]; then
        pass "BeginAuthentication returned: the canvas's second answer authorized it"
    else
        fail "the right secret's call: $(prompt_reply 1 | tr '\n' '|' | cut -c1-200)"
    fi
    assert_contains "PROMPT_DONE said success" "$(canvas_log)" "prompt: conversation 1 done, success"
    if wait_until "the panel gone" 10 prompt_panes_gone; then
        pass "no prompt pane remains ($(canvas_count 'a prompt surface') made, as many gone)"
    else
        fail "$(canvas_count 'a prompt surface') prompt panes made, $(canvas_count 'its prompt surface is gone') gone"
    fi
    record=$(on_target 10 'cat "$T_HOME/prompt-record" 2>/dev/null')
    if [[ "$record" == "$prompt_wrong"$'\n'"$prompt_secret" ]]; then
        pass "the fake helper read exactly the wrong secret, then the right one, and nothing else"
    else
        fail "the fake helper's record is ${#record} bytes, not the wrong then the right secret"
    fi
    cookies=$(on_target 10 'cat "$T_HOME/prompt-cookies" 2>/dev/null | tr "\n" " "')
    if [[ "$cookies" == "prompt-cookie-1 prompt-cookie-1 " ]]; then
        pass "two helper runs, one cookie: the retry stayed inside the request"
    else
        fail "the helper's cookies are '$cookies', not the one request's twice"
    fi
fi

say "35f. Escape dismisses the canvas's prompt, and the call fails as dismissed"
if [[ "$PROMPT_RUN" -eq 1 ]]; then
    panes_before=$(canvas_count "prompt surface configured")
    focus_before=$(canvas_count "seat: the keyboard is on the prompt")
    prompt_call 2 "The nested gate asks again"
    if wait_until "the second prompt" 30 "canvas_log | jgrep -F 'prompt: conversation 2 asking'" &&
        wait_until "its pane" 15 "canvas_more 'prompt surface configured' $panes_before" &&
        prompt_focused "$focus_before"; then
        keys "x"
        key Escape
    else
        fail "no prompt for conversation 2"
    fi
    if wait_until "the dismissed call" 30 '[[ -s "$SHOTS/prompt-call-2" ]]' &&
        [[ "$(prompt_reply 2)" != *"exit=0"* && "$(prompt_reply 2)" == *"dismissed"* ]]; then
        pass "a dismissed prompt fails the call: $(prompt_reply 2 | head -1 | cut -c1-100)"
    else
        fail "the dismissed call: $(prompt_reply 2 | tr '\n' '|' | cut -c1-200)"
    fi
    assert_contains "the host dismissed it" "$(canvas_log)" "prompt: conversation 2 dismissed; told to caustics-polkit"
    if wait_until "the panel gone" 10 prompt_panes_gone; then
        pass "no prompt pane remains after the dismissal"
    else
        fail "a prompt pane outlived the dismissal"
    fi
    record=$(on_target 10 'cat "$T_HOME/prompt-record" 2>/dev/null')
    [[ "$record" == "$prompt_wrong"$'\n'"$prompt_secret" ]] ||
        fail "the dismissed helper read an answer: the record is ${#record} bytes"
fi

say "35g. neither secret is in the canvas's log, the sink's lines or the journal"
if [[ "$PROMPT_RUN" -eq 1 ]]; then
    leaks=$(on_target 10 "grep -cF -e '$prompt_secret' -e '$prompt_wrong' \"\$T_HOME/canvas.log\"")
    if [[ "${leaks:-1}" -eq 0 ]] && ! journal | jgrep -F -e "$prompt_secret" -e "$prompt_wrong"; then
        pass "no host line, no witness, no sidecar line and no bar line holds either secret"
    else
        fail "a secret is in ${leaks:-?} lines of the canvas's log, or in the bar's journal"
    fi
fi
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    # Torn down, as step 35 left the canvas: gone, with its sidecars.
    prompt_canvas=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    fixture_stop prompt
    if wait_until "the prompt's canvas gone" 20 "! on_target 5 'kill -0 ${prompt_canvas:-0} 2>/dev/null'"; then
        pass "the canvas stopped, and its polkit sink with it"
    else
        fail "the prompt step's canvas outlived its fixture"
    fi
    niri_msg action focus-workspace-up >/dev/null 2>&1
fi

say "35h. the lock sidecar locks on its socket, and only the right answer unlocks"
# `caustics-lock` in its dry run (lock/README.md) with no system bus at all
# (its address names a socket that does not exist), so there is no logind and
# the socket's `lock` is the only trigger; `bar/abi/examples/fake_host.rs` on
# its pipes plays the canvas host, confirms each lock and unlock as the real
# host does once its lock surfaces are up, and answers the first ask with a
# wrong secret and the second with the right one. Both secrets are this run's own,
# and no log may hold either.
if [[ "$T_NAME" == vm ]]; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "the lock dry run and its fake host run on the nested target only"
    SKIPS=$((SKIPS + 1))
else
    lock_right="gate-right-$RANDOM$RANDOM-$RANDOM"
    lock_wrong="gate-wrong-$RANDOM$RANDOM-$RANDOM"
    lock_log() { on_target 10 'cat "$T_HOME/lock.log" 2>/dev/null'; }
    # in_order <text> <fixed pattern>…: each pattern on a line after the
    # last one's; prints the first that is missing.
    in_order() {
        local text="$1" at=0 n p
        shift
        for p in "$@"; do
            n=$(tail -n +$((at + 1)) <<<"$text" | grep -nF -m1 -- "$p" | cut -d: -f1)
            [[ -n "$n" ]] || { printf '%s' "$p"; return 1; }
            at=$((at + n))
        done
    }
    on_target 10 'rm -f "$T_HOME/lock.log" "$T_HOME/lock-wait.out"' >/dev/null
    fixture_start lock 'bash -c '"'"'cd "$1" && exec env CAUSTICS_LOCK_DRY_RUN_SECRET="$3" \
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=$1/no-system-bus" \
        FAKE_HOST_SECRET="$3" FAKE_HOST_WRONG="$4" FAKE_HOST_SCRIPT="text:FAKE_HOST_WRONG text" \
        FAKE_HOST_EVENTS="locked unlocked" \
        "$2/examples/fake_host" "$2/caustics-lock" >>"$1/lock.log" 2>&1'"'"' lock "$T_HOME" "$T_BIN" '"$lock_right $lock_wrong"
    if wait_until "the lock sidecar's socket" 30 'lock_log | jgrep -F "listening on"'; then
        pass "$(lock_log | grep -F 'no logind' | tail -1 | sed 's/^caustics-lock: //' | cut -c1-140)"
    else
        fail "caustics-lock never listened: $(lock_log | tail -3 | tr '\n' '|' | cut -c1-300)"
    fi
    # The before-sleep hold, in the background: it returns when the host
    # says locked, which the fake host does as soon as the lock arrives.
    on_target 40 '"$T_BIN/caustics-canvasctl" lock --wait >"$T_HOME/lock-wait.out" 2>&1
        echo "exit=$?" >>"$T_HOME/lock-wait.out"' >/dev/null 2>&1 &
    lock_waiter=$!
    if wait_until "lock --wait returned" 30 'on_target 5 "grep -q ^exit= \"\$T_HOME/lock-wait.out\""'; then
        waited=$(on_target 5 'cat "$T_HOME/lock-wait.out"')
        if [[ "$waited" == *"locked"* && "$waited" == *"exit=0"* ]]; then
            pass "caustics-canvasctl lock --wait returned 0 with 'locked'"
        else
            fail "caustics-canvasctl lock --wait: $(tr '\n' '|' <<<"$waited" | cut -c1-200)"
        fi
    else
        fail "caustics-canvasctl lock --wait never returned"
    fi
    wait "$lock_waiter" 2>/dev/null
    if wait_until "the lock's unlock" 30 'lock_log | jgrep -F "fake-host: sent unlocked"'; then
        log=$(lock_log)
        missing=$(in_order "$log" \
            "fake-host: lock command=lock" "fake-host: sent locked" \
            'prompt id=1 kind=title arg=1' 'prompt id=1 kind=secret arg=0 message="Password: "' \
            "answered id=1 with text:FAKE_HOST_WRONG" \
            'prompt id=1 kind=error arg=0 message="Authentication failure"' \
            'prompt id=1 kind=done arg=0' \
            'prompt id=2 kind=title arg=1' 'prompt id=2 kind=secret arg=0 message="Password: "' \
            "answered id=2 with text" 'prompt id=2 kind=done arg=1' \
            "fake-host: lock command=unlock" "fake-host: sent unlocked")
        if [[ -z "$missing" ]]; then
            pass "lock, locked, a title and an ask, the wrong secret's error and failed end, a fresh ask, success, unlock, unlocked: in that order"
        else
            fail "the lock's frames are not in order, from: $missing"
        fi
        if [[ "$(grep -c 'lock command=unlock' <<<"$log")" -eq 1 ]]; then
            pass "one unlock, after the right answer and only then"
        else
            fail "$(grep -c 'lock command=unlock' <<<"$log") unlocks"
        fi
    else
        fail "the fake host never confirmed an unlock: $(lock_log | tail -4 | tr '\n' '|' | cut -c1-300)"
    fi
    # `status` asks the wallpaper (gone with the canvas fixture, so the exit
    # is 1) and the lock, whose line is printed either way.
    lock_status() { on_target 5 '"$T_BIN/caustics-canvasctl" status 2>/dev/null'; }
    if wait_until "the lock's status unlocked" 20 '[[ "$(lock_status)" == *"locked=no attempts=1"* ]]'; then
        pass "caustics-canvasctl status: $(grep -F 'locked=' <<<"$(lock_status)")"
    else
        fail "the lock's status: $(on_target 5 '"$T_BIN/caustics-canvasctl" status 2>&1' | tr '\n' '|' | cut -c1-200)"
    fi
    if lock_log | jgrep -F "LockedHint of session"; then
        fail "the dry run with no system bus spoke to logind"
    else
        pass "no logind: no signals and no hint"
    fi
    leaks=0
    for secret in "$lock_right" "$lock_wrong"; do
        n=$(on_target 10 "cat \"\$T_HOME/lock.log\" \"\$T_HOME/lock-wait.out\" 2>/dev/null | grep -cF -- '$secret'")
        leaks=$((leaks + ${n:-1}))
        journal | jgrep -F -- "$secret" && leaks=$((leaks + 1))
        grep -qF -- "$secret" "$RUN/fixture-lock.log" 2>/dev/null && leaks=$((leaks + 1))
    done
    if [[ "$leaks" -eq 0 ]]; then
        pass "no witness, no host line, no client line and no bar line holds either secret"
    else
        fail "a secret is in $leaks lines of the lock's logs or the bar's journal"
    fi
    # The fake host goes, and the sidecar leaves with it, taking its socket.
    lock_pid=$(lock_log | sed -n 's/^fake-host: started .* as pid \([0-9]*\)$/\1/p' | tail -1)
    fixture_stop lock
    if [[ -n "$lock_pid" ]] &&
        wait_until "the lock sidecar gone" 20 "! on_target 5 'kill -0 $lock_pid 2>/dev/null || [ -S \"\$XDG_RUNTIME_DIR/caustics-lock.sock\" ]'"; then
        pass "the sidecar exited when its host went, and took its socket: $(lock_log | grep -F 'stdin closed' | tail -1 | sed 's/^caustics-lock: //')"
    else
        fail "caustics-lock (pid ${lock_pid:-?}) or its socket outlived its host"
    fi
fi

say "35i. the canvas locks the session on its lock surfaces, and only the right answer unlocks it"
# The canvas with `caustics-lock` in its set, the sidecar in the dry run 35h
# uses (no system bus, so no logind, and the socket's `lock` the only
# trigger): now the canvas host is what locks the nested niri, through
# `ext_session_lock_v1`, and the harness's keyboard types into the lock's
# prompt. **niri refuses every action over its socket while locked, a
# screenshot included** (its `do_action` returns early unless an action is
# allowed when locked, and IPC never allows one): the request is answered
# and no file is written. So the lock screen's look is the caustic's witness
# and the oracle's, and a screenshot that never arrives is the proof that
# niri is locked; one that does, after the unlock, is the proof it is not.
# Both secrets are this run's own, and no log may hold either.
LOCK_RUN=0
if [[ "$CANVAS_RUN" -ne 1 ]]; then
    printf '  SKIP  %s  %s\n' "$(stamp)" "the canvas fixture runs on the nested target only"
    SKIPS=$((SKIPS + 1))
else
    LOCK_RUN=1
    canvas_right="gate-locked-$RANDOM$RANDOM-$RANDOM"
    canvas_wrong="gate-astray-$RANDOM$RANDOM-$RANDOM"
    # niri_shot_works <name>: 0 when niri wrote a screenshot within 3 s.
    niri_shot_works() {
        local raw="$RUN/.probe-$1" i
        rm -f "$raw"
        niri_msg action screenshot-screen --show-pointer false --path "$raw" >/dev/null 2>&1
        for i in $(seq 30); do
            [[ -s "$raw" ]] && { rm -f "$raw"; return 0; }
            sleep 0.1
        done
        rm -f "$raw"
        return 1
    }
    # The fixture: the canvas, its text and its lock sidecar, started the
    # same way twice (the second start is the restarted host of 35j).
    canvas_lock_start() {
        fixture_start locked 'bash -c '"'"'cd "$1" && exec env \
            CAUSTICS_SIDECARS="caustics-wallpaper caustics-reload caustics-glyphs caustics-lock" \
            CAUSTICS_LOCK_DRY_RUN_SECRET="$3" \
            DBUS_SYSTEM_BUS_ADDRESS="unix:path=$1/no-system-bus" \
            RUST_LOG=info "$2/caustics-canvas" 2>>"$1/canvas.log"'"'"' locked "$T_HOME" "$T_BIN" '"$canvas_right"
    }
    # canvas_lock_wait <name>: `lock --wait` in the background, its output
    # and exit in `$T_HOME/<name>.out`; 0 when it returned 0 with `locked`.
    canvas_lock_wait() {
        on_target 10 "rm -f \"\$T_HOME/$1.out\"" >/dev/null
        on_target 40 '"$T_BIN/caustics-canvasctl" lock --wait >"$T_HOME/'"$1"'.out" 2>&1
            echo "exit=$?" >>"$T_HOME/'"$1"'.out"' >/dev/null 2>&1 &
        local waiter=$! out
        wait_until "lock --wait returned" 30 'on_target 5 "grep -q ^exit= \"\$T_HOME/'"$1"'.out\""' || {
            wait "$waiter" 2>/dev/null
            return 1
        }
        wait "$waiter" 2>/dev/null
        out=$(on_target 5 "cat \"\$T_HOME/$1.out\"")
        [[ "$out" == *"locked"* && "$out" == *"exit=0"* ]]
    }
    # No lock pane remains: as many gone as made.
    lock_panes_gone() {
        [[ "$(canvas_count "a lock surface")" -eq "$(canvas_count "its lock surface is gone")" ]]
    }
    for _ in 1 2 3 4 5; do
        [[ "$(niri_msg --json focused-window 2>/dev/null)" == null ]] && break
        niri_msg action focus-workspace-down >/dev/null 2>&1
    done
    on_target 10 'rm -f "$T_HOME/canvas.log"' >/dev/null
    canvas_lock_start
    if wait_until "the canvas's lock sidecar" 60 'canvas_log | jgrep -F "caustics-lock: listening on"' &&
        wait_until "the canvas's font" 30 'canvas_log | jgrep -F "font: "'; then
        pass "the canvas started caustics-lock in its dry run: $(canvas_last 'caustics-lock: no logind' | sed 's/^caustics-lock: //' | cut -c1-100)"
    else
        fail "the canvas's caustics-lock never listened: $(canvas_log | tail -4 | tr '\n' '|' | cut -c1-400)"
        LOCK_RUN=0
    fi
    if canvas_log | jgrep -F "lock: ext_session_lock_manager_v1 is bound"; then
        pass "the host bound ext_session_lock_manager_v1"
    else
        fail "the host did not bind the lock manager: $(canvas_log | grep -F 'lock:' | head -2 | tr '\n' '|')"
        LOCK_RUN=0
    fi
    shows_before=$(canvas_count "canvas: show ")
    canvasctl show '"$T_HOME/canvas-blue.png"' >/dev/null 2>&1 || fail "caustics-canvasctl show failed"
    wait_until "the lock step's picture" 30 "canvas_more 'canvas: show ' $shows_before" ||
        fail "the canvas never showed the picture"
    sleep 0.5
    shot lock-before.png
fi
if [[ "$LOCK_RUN" -eq 1 ]]; then
    focus_before=$(canvas_count "seat: the keyboard is on the lock")
    if canvas_lock_wait lock-wait-canvas; then
        pass "caustics-canvasctl lock --wait returned 0 with 'locked', from the canvas host"
    else
        fail "lock --wait against the canvas: $(on_target 5 'cat "$T_HOME/lock-wait-canvas.out"' | tr '\n' '|' | cut -c1-200)"
    fi
    surfaces=$(canvas_count "a lock surface")
    if [[ "$surfaces" -eq 1 ]] && canvas_log | jgrep -F "lock surface configured" &&
        canvas_log | jgrep -F "lock: ext_session_lock_v1 asked for, with 1 lock surfaces"; then
        pass "one lock surface, on the one output, configured: $(canvas_last 'lock surface configured' | sed -n 's/.*INFO [a-z_:]*: //p' | cut -c1-100)"
    else
        fail "$surfaces lock surfaces configured: $(canvas_log | grep -F 'lock' | head -4 | tr '\n' '|' | cut -c1-300)"
    fi
    missing=$(in_order "$(canvas_log)" "lock: caustics-lock asked to lock; locking" \
        "lock: the compositor locked the session; told to caustics-lock" "caustics-lock: locked")
    if [[ -z "$missing" ]]; then
        pass "the lock asked, the compositor locked, and the sidecar was told: in that order"
    else
        fail "the lock's lines are not in order, from: $missing"
    fi
    if ! niri_shot_works locked; then
        pass "niri is locked: it writes no screenshot"
    else
        fail "niri wrote a screenshot while the canvas held the lock"
    fi
    if wait_until "the lock's prompt" 20 'canvas_log | jgrep -F "prompt: conversation 1 from caustics-lock began"' &&
        wait_until "the lock's prompt drawn" 15 'canvas_log | jgrep -E "canvas[a-z_:]*: prompt: conversation 1 asking \(echo 0, attempts 0\)"'; then
        pass "the sidecar's title and ask reached the host, and the caustic drew conversation 1 asking on the lock"
    else
        fail "no lock prompt drawn: $(canvas_log | grep -F 'prompt' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    if canvas_log | jgrep -F "prompt surface"; then
        fail "a top-layer prompt pane was made while locked"
    else
        pass "no top-layer prompt pane: the lock surface carries the prompt"
    fi
    if wait_until "the keyboard on the lock" 15 "canvas_more 'seat: the keyboard is on the lock' $focus_before"; then
        pass "$(canvas_last 'seat: the keyboard is on the lock' | sed -n 's/.*INFO [a-z_:]*: //p')"
    else
        fail "the keyboard never came to the lock surface"
    fi
    keys "$canvas_wrong"
    key Return
    if wait_until "the wrong answer's error line" 20 'canvas_log | jgrep -F "prompt: conversation 1: an error line after the answer (attempts 1)"' &&
        wait_until "a fresh conversation" 20 'canvas_log | jgrep -E "canvas[a-z_:]*: prompt: conversation 2 asking \(echo 0, attempts 0\)"'; then
        pass "the wrong secret: an error line, attempts 1, conversation 1 failed, and conversation 2 asking"
    else
        fail "after the wrong secret: $(canvas_log | grep -F 'prompt' | tail -5 | tr '\n' '|' | cut -c1-400)"
    fi
    assert_contains "conversation 1 ended as a failure" "$(canvas_log)" "prompt: conversation 1 done, failure"
    if ! niri_shot_works wrong; then
        pass "and niri is still locked"
    else
        fail "niri wrote a screenshot after a wrong secret"
    fi
    keys "$canvas_right"
    key Return
    if wait_until "the unlock" 20 'canvas_log | jgrep -F "caustics-lock: unlocked"'; then
        missing=$(in_order "$(canvas_log)" "prompt: conversation 2 done, success" \
            "lock: caustics-lock asked to unlock; unlocking" "its lock surface is gone" \
            "lock: the session is unlocked; told to caustics-lock" "caustics-lock: unlocked")
        if [[ -z "$missing" ]]; then
            pass "the right secret: success, the unlock asked, the lock surface gone, unlocked told: in that order"
        else
            fail "the unlock's lines are not in order, from: $missing"
        fi
    else
        fail "the session never unlocked: $(canvas_log | grep -E 'lock|prompt' | tail -5 | tr '\n' '|' | cut -c1-400)"
    fi
    if lock_panes_gone; then
        pass "no lock surface remains ($(canvas_count 'a lock surface') made, as many gone)"
    else
        fail "$(canvas_count 'a lock surface') lock surfaces made, $(canvas_count 'its lock surface is gone') gone"
    fi
    if wait_until "niri unlocked" 10 'niri_shot_works unlocked'; then
        pass "niri is unlocked: it writes a screenshot again"
    else
        fail "niri still refuses a screenshot after the unlock"
    fi
    sleep 0.5
    shot lock-after.png
    moved=$(png_pixel_diff "$SHOTS/lock-before.png" "$SHOTS/lock-after.png" 0 0 "$SCREEN_W" "$SCREEN_H")
    corner=($(png_mean "$SHOTS/lock-after.png" 40 680 160 80))
    if [[ "$moved" -ge 0 && "$moved" -lt 12800 ]] && ((corner[2] >= 240 && corner[0] <= 15 && corner[1] <= 15)); then
        pass "the output after the unlock is the output before the lock ($moved px differ), the wallpaper undarkened: mean ${corner[*]}"
    else
        fail "after the unlock $moved px differ from before the lock, and the corner is ${corner[*]}"
    fi
    lock_status() { on_target 5 '"$T_BIN/caustics-canvasctl" status 2>/dev/null'; }
    if wait_until "the lock's status" 20 '[[ "$(lock_status)" == *"locked=no attempts=1"* ]]'; then
        pass "caustics-canvasctl status: $(grep -F 'locked=' <<<"$(lock_status)")"
    else
        fail "the lock's status: $(on_target 5 '"$T_BIN/caustics-canvasctl" status 2>&1' | tr '\n' '|' | cut -c1-200)"
    fi
    leaks=0
    for secret in "$canvas_right" "$canvas_wrong"; do
        n=$(on_target 10 "cat \"\$T_HOME/canvas.log\" \"\$T_HOME/lock-wait-canvas.out\" 2>/dev/null | grep -cF -- '$secret'")
        leaks=$((leaks + ${n:-1}))
        journal | jgrep -F -- "$secret" && leaks=$((leaks + 1))
        grep -qF -- "$secret" "$RUN/fixture-locked.log" 2>/dev/null && leaks=$((leaks + 1))
    done
    if [[ "$leaks" -eq 0 ]]; then
        pass "neither secret is in the canvas's log, the client's output, the fixture's or the journal"
    else
        fail "a secret is in $leaks of the canvas's, the client's, the fixture's or the bar's lines"
    fi
fi

say "35j. a canvas killed while locked leaves the session locked, and its restart takes the lock over"
# What niri does when its lock client dies (src/niri.rs, `lock`): the
# session stays locked, and a new client's lock replaces the dead one at
# once (`locked` straight away, no `finished`), since the outputs are blank
# already. With no logind there is no `LockedHint` for the restarted lock
# sidecar to read, so the restart is driven as the hint would drive it: the
# fixture again, then `lock` on the new sidecar's socket.
if [[ "$LOCK_RUN" -eq 1 ]]; then
    locked_before=$(canvas_count "lock: the compositor locked the session")
    if canvas_lock_wait lock-wait-again; then
        pass "locked again: lock --wait returned 0"
    else
        fail "the second lock: $(on_target 5 'cat "$T_HOME/lock-wait-again.out"' | tr '\n' '|' | cut -c1-200)"
    fi
    dead_canvas=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    niri_lines=$(wc -l <"$RUN/niri.log" 2>/dev/null || echo 0)
    fixture_stop locked
    if [[ -n "$dead_canvas" ]] &&
        wait_until "the locked canvas gone" 20 "! on_target 5 'kill -0 $dead_canvas 2>/dev/null'"; then
        pass "the canvas host (pid $dead_canvas) was killed while locked"
    else
        fail "the locked canvas (pid ${dead_canvas:-?}) outlived its kill"
    fi
    sleep 1
    if ! niri_shot_works dead; then
        pass "niri stays locked with its lock client dead: it writes no screenshot"
    else
        fail "niri unlocked when its lock client died"
    fi
    wait_until "the old lock sidecar's socket gone" 20 "! on_target 5 '[ -S \"\$XDG_RUNTIME_DIR/caustics-lock.sock\" ]'" ||
        note "the old lock sidecar's socket outlived it"
    listening_before=$(canvas_count "caustics-lock: listening on")
    # The killed host never said its lock surface was gone: the restarted
    # host's surfaces are counted from here.
    made_before=$(canvas_count "a lock surface")
    gone_before=$(canvas_count "its lock surface is gone")
    canvas_lock_start
    if wait_until "the restarted lock sidecar" 60 "canvas_more 'caustics-lock: listening on' $listening_before"; then
        pass "the canvas restarted, and its lock sidecar listens"
    else
        fail "the restarted canvas's lock sidecar never listened: $(canvas_log | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    new_canvas=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    focus_before=$(canvas_count "seat: the keyboard is on the lock")
    if canvas_lock_wait lock-wait-taken; then
        pass "the restarted host locked the still-locked session: lock --wait returned 0"
    else
        fail "the restarted host's lock: $(on_target 5 'cat "$T_HOME/lock-wait-taken.out"' | tr '\n' '|' | cut -c1-200)"
    fi
    if canvas_more "lock: the compositor locked the session" "$((locked_before + 1))" &&
        ! canvas_log | jgrep -F "the compositor ended" &&
        on_target 5 "kill -0 ${new_canvas:-0} 2>/dev/null"; then
        pass "the compositor said locked to the new host, not finished, and the host runs (pid $new_canvas)"
    else
        fail "the new host's lock: $(canvas_log | grep -E 'lock: |exit:' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    if tail -n +$((niri_lines + 1)) "$RUN/niri.log" 2>/dev/null | jgrep -F "replacing existing dead lock"; then
        pass "niri: locking session (replacing existing dead lock)"
    else
        note "niri's log does not say it replaced a dead lock"
    fi
    if wait_until "the restarted lock's prompt" 20 '[[ $(canvas_count "prompt: conversation 1 from caustics-lock began") -ge 2 ]]' &&
        wait_until "the keyboard on the new lock" 15 "canvas_more 'seat: the keyboard is on the lock' $focus_before"; then
        keys "$canvas_right"
        key Return
    else
        fail "the restarted lock never asked: $(canvas_log | grep -F 'prompt' | tail -3 | tr '\n' '|' | cut -c1-300)"
    fi
    if wait_until "the restarted lock's unlock" 20 "[[ \$(canvas_count 'caustics-lock: unlocked') -ge 2 ]]" &&
        wait_until "niri unlocked again" 10 'niri_shot_works unlocked-again'; then
        pass "the right secret on the restarted host unlocked the session, and niri writes a screenshot again"
    else
        fail "the session did not unlock after the restart: $(canvas_log | grep -E 'lock|prompt' | tail -4 | tr '\n' '|' | cut -c1-400)"
    fi
    made=$(($(canvas_count "a lock surface") - made_before))
    gone=$(($(canvas_count "its lock surface is gone") - gone_before))
    if [[ "$made" -eq 1 && "$gone" -eq 1 ]]; then
        pass "the restarted host made one lock surface, and it went with the unlock"
    else
        fail "the restarted host made $made lock surfaces and $gone went"
    fi
    leaks=0
    for secret in "$canvas_right" "$canvas_wrong"; do
        n=$(on_target 10 "cat \"\$T_HOME/canvas.log\" \"\$T_HOME\"/lock-wait-*.out 2>/dev/null | grep -cF -- '$secret'")
        leaks=$((leaks + ${n:-1}))
    done
    [[ "$leaks" -eq 0 ]] || fail "a secret reached the canvas's log after the restart"
fi
if [[ "$CANVAS_RUN" -eq 1 ]]; then
    # Torn down, as step 35 left the canvas: gone, with its sidecars, and
    # the session unlocked.
    last_canvas=$(canvas_last "set: " | sed -n 's/.*, pid \([0-9]*\)$/\1/p')
    fixture_stop locked
    if wait_until "the lock step's canvas gone" 20 "! on_target 5 'kill -0 ${last_canvas:-0} 2>/dev/null || [ -S \"\$XDG_RUNTIME_DIR/caustics-lock.sock\" ]'"; then
        pass "the canvas stopped, and its lock sidecar with it"
    else
        fail "the lock step's canvas or its lock sidecar outlived the fixture"
    fi
    niri_msg action focus-workspace-up >/dev/null 2>&1
fi

say "36. verdict"
journal > "$SHOTS/journal-full.txt"
note "full journal: $SHOTS/journal-full.txt"
ls -1 "$SHOTS"/*.png 2>/dev/null | while read -r png; do note "$png"; done
# The tally names the target: `VM run:` (bar/vm-run.sh greps it) or
# `nested run:` (docs/testing.md quotes it).
tally="$PASSES passed, $FAILURES failed, $SKIPS skipped"
if [[ "$FAILURES" -eq 0 ]]; then
    printf '\n%s run: all checks passed (%s)\n' "${T_NAME/vm/VM}" "$tally"
else
    printf '\n%s run: %d check(s) failed (%s)\n' "${T_NAME/vm/VM}" "$FAILURES" "$tally"
fi
exit $((FAILURES > 0))
