#!/usr/bin/env bash
# Install the bar and its sidecars as one **set** (docs/architecture.md, *The ABI*).
#
# A set is a directory: `<prefix>/lib/caustics-bar/<abi-hash>/` holds the
# fifteen binaries, and `<prefix>/bin/caustics-bar` is a symlink into it, as
# are `caustics-canvas` and `caustics-canvasctl`. The bar resolves
# its own path (`current_exe`) and looks for its sidecars beside itself, so a
# bar started through that link runs its own set's sidecars — and a sidecar
# that crashes respawns out of the same set even after a newer one is
# installed. The directory is named after `ABI_HASH` and not after a commit:
# two commits with the same contract are interchangeable sets, and the name
# says so.
#
# Why any of this. `cargo build --release -p caustics-bar` builds the bar crate and
# nothing else, and a bar installed that way beside an older sidecar can agree
# with it on every version word and disagree on what a buffer number means:
# well-formed bytes, read as the wrong table. No version word can catch that. A bar refuses a sidecar from another
# set on its first frame (docs/abi.md, *Hello*); this script is the other
# half: there is no way to install half a set.
#
#   bar/install.sh                     # build, install, restart what changed
#   bar/install.sh --prefix /tmp/try    # somewhere else, and no restart
#   bar/install.sh --from target/release  # install what is already built
#   bar/install.sh --dropin bar/systemd/dropins/igpu-only.conf
#                                       # and a unit drop-in beside the unit
#
# Drop-ins: `--dropin <file>` copies a file into
# `~/.config/systemd/user/caustics-bar.service.d/`. Without it the script keeps
# whatever drop-ins are there and says which pins `VK_DRIVER_FILES`.
#
# **Never a build beside a restart**: the build finishes, every binary is
# checked to be from *this* build, and only then does the symlink move.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="$HOME/.local"
FROM=""
RESTART=""
DROPIN=""
UNIT=caustics-bar
DROPIN_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$UNIT.service.d"

# The fifteen, in the order the hosts name them: the bar, then `SIDECARS` in
# `bar/src/feeders/pipe.rs`; then the canvas host, its own sidecar (`SIDECARS`
# in `canvas/src/sidecars.rs`, whose other is `caustics-reload`, above), its
# client, and `caustics-polkit`, the canvas's polkit sink, which the canvas
# does not start until it can show a prompt (polkit/README.md). A name here
# that is not in a host's list is a binary nothing starts; a name in a list
# that is not here is a sidecar its host reports absent.
BINARIES=(
    caustics-bar
    caustics-sysmon
    caustics-tray
    caustics-power
    caustics-audio
    caustics-network
    caustics-bluetooth
    caustics-notify
    caustics-apps
    caustics-glyphs
    caustics-reload
    caustics-canvas
    caustics-wallpaper
    caustics-canvasctl
    caustics-polkit
)

# The binaries something outside the set starts by name: the bar's unit, the
# canvas's unit, and the scripts that drive the canvas. Each is a symlink
# into the set, moved with the bar's.
LINKED=(caustics-bar caustics-canvas caustics-canvasctl)

die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
say() { printf '  %s\n' "$*"; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        --from) FROM="${2:?--from needs a directory}"; shift 2 ;;
        --restart) RESTART=yes; shift ;;
        --no-restart) RESTART=no; shift ;;
        --dropin) DROPIN="${2:?--dropin needs a file}"; shift 2 ;;
        -h|--help) /usr/bin/sed -n "2,/^set -uo/p" "${BASH_SOURCE[0]}" | /usr/bin/sed '$d'; exit 0 ;;
        *) die "unknown argument $1" ;;
    esac
done
[[ -z "$DROPIN" || -f "$DROPIN" ]] || die "--dropin names no file: $DROPIN"


# The unit's ExecStart is `%h/.local/bin/caustics-bar`, so only the default prefix
# is the one systemd starts. Anywhere else is a trial install and restarting
# the running bar would say nothing about it.
if [[ -z "$RESTART" ]]; then
    if [[ "$PREFIX" == "$HOME/.local" ]]; then RESTART=yes; else RESTART=no; fi
fi

# --- 1. the build -----------------------------------------------------------

if [[ -z "$FROM" ]]; then
    # One cargo at a time in this `target/`. Two builds sharing it leave a
    # directory holding binaries from two trees — the shape of the failure
    # this script exists to stop. Cargo holds the build directory's lock for
    # the whole build, so probing that lock finds a build into this tree's
    # `target/` wherever it runs (another shell, a container with the tree
    # mounted) and ignores a cargo building some other tree, which a scan of
    # the process list does not.
    lock="$REPO/target/release/.cargo-lock"
    if [[ -e "$lock" ]] && ! flock -n "$lock" true 2>/dev/null; then
        die "another cargo is building into target/; a second build would share it"
    fi

    FROM="$REPO/target/release"
    say "building the workspace (release)"
    # `--workspace`, never `-p caustics-bar`: the sidecars are their own crates.
    # `nice` because this runs on a machine somebody is using.
    if ! (cd "$REPO" && nice -n 19 cargo build --release --workspace); then
        die "the build failed; nothing was installed"
    fi

    # Every binary is from *this* tree. `cargo build --workspace` is the
    # guarantee: a crate that fails to build fails the whole command (and the
    # `die` above fires), and a crate whose sources did not change keeps its
    # binary because that binary *is* current. So there is no mtime check
    # against the build's start: a change to three crates relinks three, and
    # the untouched sidecars are older than the build and just as current.
    # A mixed set comes from `-p caustics-bar`, which this script never runs.
    for name in "${BINARIES[@]}"; do
        [[ -x "$FROM/$name" ]] || die "$FROM/$name was not built"
    done
    say "${#BINARIES[@]} binaries, all from this workspace build"
else
    FROM="$(cd "$FROM" && pwd)" || die "--from names no directory"
    for name in "${BINARIES[@]}"; do
        [[ -x "$FROM/$name" ]] || die "$FROM/$name is not there"
    done
    say "installing what is in $FROM (no build)"
fi

# --- 2. the set's name ------------------------------------------------------

HASH="$("$FROM/caustics-bar" --abi-hash)" || die "the built bar would not print its ABI hash"
[[ "$HASH" =~ ^[0-9a-f]{16}$ ]] || die "the bar printed $HASH, which is no ABI hash"
SET="$PREFIX/lib/caustics-bar/$HASH"
say "set $HASH"

# --- 3. install, then move the link -----------------------------------------

# Only a binary that differs is written. An unchanged one is left as the
# inode it is, which is what lets the running bar keep it (below): a process
# whose binary was replaced underneath it shows `/proc/<pid>/exe` as deleted,
# and that is the whole test for "needs restarting".
set_existed=""
[[ -d "$SET" ]] && set_existed=yes
mkdir -p "$SET" "$PREFIX/bin" || die "could not make $SET"
changed=()
for name in "${BINARIES[@]}"; do
    if [[ -f "$SET/$name" ]] && cmp -s "$FROM/$name" "$SET/$name"; then
        continue
    fi
    install -Dm755 "$FROM/$name" "$SET/$name" || die "could not install $name"
    changed+=("$name")
done
if [[ ${#changed[@]} -eq 0 ]]; then
    say "every binary in $SET is already this build's"
else
    say "installed ${#changed[@]} of ${#BINARIES[@]} into $SET: ${changed[*]}"
fi

# The links move last, and atomically: `ln -sfn` on to an existing symlink is
# two syscalls and a window in which `~/.local/bin/caustics-bar` is a link into a
# directory that does not exist yet. A temporary name and a rename is one.
for name in "${LINKED[@]}"; do
    ln -sfn "$SET/$name" "$PREFIX/bin/$name.installing" ||
        die "could not make the link for $name"
    mv -Tf "$PREFIX/bin/$name.installing" "$PREFIX/bin/$name" ||
        die "could not move the link for $name"
    say "$PREFIX/bin/$name -> $(readlink "$PREFIX/bin/$name")"
done

# Older sets are left alone, deliberately: a bar that is still running holds
# its own set's path, and a sidecar of its that crashes respawns out of it. A
# set is small (fifteen binaries) and removing one by hand is `rm -r` on a
# directory named after a hash nothing is running.
kept=$(find "$PREFIX/lib/caustics-bar" -mindepth 1 -maxdepth 1 -type d | wc -l)
[[ "$kept" -gt 1 ]] && say "$((kept - 1)) older set(s) kept; a running bar holds its own"

# --- 4. the unit's drop-ins ------------------------------------------------

# Every drop-in that sets VK_DRIVER_FILES, one "file: value" per line.
pinning_dropins() {
    local f
    for f in "$DROPIN_DIR"/*.conf; do
        [[ -f "$f" ]] || continue
        /usr/bin/sed -n "s|^Environment=.*VK_DRIVER_FILES=\([^ \"]*\).*|$(basename "$f"): \1|p" "$f"
    done
}

reload=""
if [[ -n "$DROPIN" ]]; then
    mkdir -p "$DROPIN_DIR" || die "could not make $DROPIN_DIR"
    install -m644 "$DROPIN" "$DROPIN_DIR/$(basename "$DROPIN")" || die "could not install $DROPIN"
    say "drop-in $DROPIN_DIR/$(basename "$DROPIN") installed"
    reload=yes
elif [[ "$PREFIX" == "$HOME/.local" ]] && systemctl --user cat "$UNIT" >/dev/null 2>&1; then
    pins="$(pinning_dropins)"
    if [[ -n "$pins" ]]; then
        while IFS= read -r line; do say "keeping drop-in $line"; done <<<"$pins"
    else
        gpus=$(find /sys/class/drm -maxdepth 1 -name 'renderD*' 2>/dev/null | wc -l)
        if [[ "$gpus" -gt 1 ]]; then
            say "$gpus GPUs and no VK_DRIVER_FILES pin: on a laptop with a discrete GPU, add"
            say "  --dropin $REPO/bar/systemd/dropins/igpu-only.conf (read it first)"
        fi
    fi
fi
# A changed drop-in, or a unit file that changed under systemd (the unit is
# often a symlink into this checkout), is only read on a reload.
if [[ -n "$reload" ]] ||
    [[ "$(systemctl --user show "$UNIT" -p NeedDaemonReload --value 2>/dev/null)" == yes ]]; then
    systemctl --user daemon-reload || die "systemctl --user daemon-reload failed"
    say "systemd reloaded the unit and its drop-ins"
fi

# --- 5. the unit ------------------------------------------------------------

if [[ "$RESTART" != yes ]]; then
    say "not restarting $UNIT (prefix is not $HOME/.local)"
    exit 0
fi
if ! systemctl --user cat "$UNIT" >/dev/null 2>&1; then
    say "no $UNIT unit installed; see bar/systemd/README.md"
    exit 0
fi

# The bar supervises its sidecars and respawns one that exits, from the path
# beside its own binary. So when the running bar is this set's and its own
# binary did not change, the unit stays up and only the sidecars whose
# binaries did change are ended: each comes back on the new code, and the
# panel never blinks. The unit restarts when the bar's binary changed, when
# the set is a new one, when a drop-in changed, or when the running bar is
# not this set's at all.
main=$(systemctl --user show "$UNIT" -p MainPID --value 2>/dev/null || echo 0)
running_bar=$(readlink "/proc/${main:-0}/exe" 2>/dev/null || true)
if [[ -n "$set_existed" && -z "$reload" && "${main:-0}" -gt 0 && "$running_bar" == "$SET/caustics-bar" ]]; then
    ended=0
    for pid in $(pgrep -P "$main" 2>/dev/null); do
        exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || continue
        [[ "$exe" == *" (deleted)" ]] || continue
        kill -TERM "$pid" 2>/dev/null || continue
        say "ended $(basename "${exe% (deleted)}") (pid $pid); the bar respawns it on the new binary"
        ended=$((ended + 1))
    done
    if [[ "$ended" -eq 0 ]]; then
        say "the running bar is this set's and nothing it runs changed; not restarting $UNIT"
    else
        say "the bar itself is unchanged; not restarting $UNIT"
    fi
    exit 0
fi

cursor=$(journalctl --user -u "$UNIT" --no-pager -n 0 --show-cursor 2>/dev/null |
    /usr/bin/sed -n 's/^-- cursor: //p')
systemctl --user restart "$UNIT" || die "the unit would not restart"
say "restarted $UNIT"

# The first thing the new bar says about itself. `set:` names the directory it
# resolved to, which is the one thing worth reading back: a bar that is still
# running an older set has a stale link or a systemd that did not re-exec.
for _ in $(seq 40); do
    line=$(journalctl --user -u "$UNIT" --no-pager -o cat \
        ${cursor:+--after-cursor "$cursor"} 2>/dev/null | grep -F 'set: ' | head -1)
    [[ -n "$line" ]] && break
    sleep 0.5
done
if [[ -n "${line:-}" ]]; then
    say "$line"
else
    say "no 'set:' line yet; journalctl --user -u $UNIT"
fi

# ...and the first health line, which is the bar naming every sidecar it
# started — `running pid=`, `absent`, or `refused`. The bar writes one
# ten seconds after it starts and hourly thereafter (`health.rs`), so this is a
# short wait and not a vigil.
for _ in $(seq 40); do
    health=$(journalctl --user -u "$UNIT" --no-pager -o cat \
        ${cursor:+--after-cursor "$cursor"} 2>/dev/null | grep -F 'health (' | head -1)
    [[ -n "$health" ]] && break
    sleep 0.5
done
if [[ -n "${health:-}" ]]; then
    say "$health"
else
    say "no health line yet; journalctl --user -u $UNIT -f"
fi
