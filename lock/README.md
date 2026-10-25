# caustics-lock

The session's lock, as a sink sidecar of the canvas (docs/canvas.md,
*Sinks*, *How each job is wired*). It owns logind (this session's `Lock`
signal and its `LockedHint`) and the PAM conversation; the canvas host owns
the lock surfaces and the prompt drawn on them. It asks the host to lock and
unlock over the wire (docs/abi.md, *Lock*), hears back when the compositor
confirmed each (*Canvas event*), and holds each attempt as a **prompt
conversation** (*Prompt*, *Answer*).

```
caustics-lock
```

No arguments. The first frame on stdout is `Hello`; then `Lock` and `Prompt`
frames, `CanvasEvent` and `Answer` frames are read from stdin, every witness
is one line on stderr, and the process exits when stdin closes.

**The host's half is not built**: the canvas does not take
`ext_session_lock_v1` or say `locked` yet, so this sidecar is not in its
default set (*In the canvas's set*).

## Behaviour

- **Locking**: logind's `Lock` signal (`loginctl lock-session`, an idle
  timer, the lid) or `lock` on the socket writes `LOCK_LOCK`. When the host
  says `locked`, `LockedHint` is set and a conversation begins.
- **On start**, a `LockedHint` that is already set writes `LOCK_LOCK`
  before anything else, so a restarted host re-locks before it paints
  (docs/canvas.md, *Failure model*).
- **A conversation** is one PAM transaction as this process's user: a
  `PROMPT_INFO` title (the user's name), one `Prompt` per PAM message
  (`PAM_PROMPT_ECHO_OFF` is `PROMPT_SECRET`, `PAM_PROMPT_ECHO_ON` is
  `PROMPT_ECHO`, `PAM_TEXT_INFO` and `PAM_ERROR_MSG` are `PROMPT_INFO` and
  `PROMPT_ERROR`), and one `PROMPT_DONE`. On success, `PROMPT_DONE` 1 and
  `LOCK_UNLOCK`; the hint is cleared when the host says `unlocked`.
- **A failure** is counted, never described: PAM's message as a
  `PROMPT_ERROR`, `PROMPT_DONE` 0, and a fresh conversation at once (PAM's
  own fail delay applies; nothing is added). **A dismissal** (Escape) ends
  the attempt the same way, uncounted: a lock screen is never dismissed. A
  PAM that fails without asking anything (a service that denies outright) is
  retried after a pause that doubles from 1 s to 30 s, so it cannot spin.
- **Nothing unlocks but a success**: logind's `Unlock` signal is witnessed
  and ignored.
- A transaction is `pam_start`, `pam_authenticate` and, on success,
  `pam_setcred(PAM_REFRESH_CRED)`, then `pam_end`. There is no account check
  (`pam_acct_mgmt`): the session already exists, and an account that has
  just expired must not lock its own user out of it.
- **Nothing typed is written anywhere but to PAM**, and no witness carries
  an answer. Answers are read from an unbuffered duplicate of stdin, and a
  response PAM is handed back and then refused is zeroed before it is freed.

## The socket

`$XDG_RUNTIME_DIR/caustics-lock.sock`, one command a line, one reply a line,
as the wallpaper's:

| line | reply | |
|---|---|---|
| `lock` | `ok` | the lock is asked for, or already held |
| `lock wait` | `locked` | once the host has said locked; `error …` if it goes first |
| `status` | `ok locked=yes\|no attempts=N` | the failed conversations of the lock now held, or of the last |

`caustics-canvasctl lock [--wait]` speaks it, exiting 0 on `ok` or `locked`
and 1 otherwise; `lock --wait` is the before-sleep hold, a sleep hook's
`caustics-canvasctl lock --wait && systemctl suspend`. `caustics-canvasctl
status` prints the lock's line after the wallpaper's when this socket
answers.

## Settings

From the sidecar's own environment:

| variable | default | |
|---|---|---|
| `CAUSTICS_LOCK_PAM_SERVICE` | `swaylock` | the PAM service (`/etc/pam.d/<service>`); `swaylock` is the one a session built on the sway tools already has. Keeping the fingerprint module off the unlock path is that service file's business, and this is how a different one is chosen |
| `CAUSTICS_LOCK_DRY_RUN_SECRET` | unset | the **dry run**: PAM is not touched, and a conversation is one echo-off ask, `Password: `, compared with these bytes (not in constant time: they are in the environment, which is no secret). Empty is no dry run |

The dry run still speaks to logind if a system bus answers, so a test points
`DBUS_SYSTEM_BUS_ADDRESS` at a socket that does not exist. Without a system
bus, or without a session on it, there is **no logind**: one witness, no
signals, no hint, and the socket's `lock` as the only trigger. The tests
(`tests/dry_run.rs`) and the nested gate run exactly that, under
`bar/abi/examples/fake_host.rs` in the host's place (docs/testing.md).

## In the canvas's set

`caustics-lock` is installed with the rest of the set by `bar/install.sh`.
The canvas host starts the sidecars `$CAUSTICS_SIDECARS` names on its unit,
else its compiled set (`canvas/src/sidecars.rs`); once the host has its lock surfaces, a unit drop-in that
adds `caustics-lock` to that list is how it is enabled. **It is not started
by default yet**: until the host can lock, a `LOCK_LOCK` is a frame with no
consumer and a `lock --wait` never returns `locked`, so the session's
previous locker stays its locker (docs/canvas.md, *Order*).

## Building

`pam-sys` links `-lpam` with prebuilt bindings, so no PAM header is needed.
A system with the library but not its development package has only
`libpam.so.0`; `build.rs` then gives the linker `libpam.so` as a link to it
in the build directory. The binary needs `libpam.so.0` at run time, which
every Linux system with PAM has.

## Exit codes

- **0**: the host closed stdin. A refusal (neither logind nor the socket,
  so nothing could ask for a lock; no name for this uid, so PAM has nobody
  to authenticate) is one witness, then a wait for stdin to close, so the
  host does not restart the sidecar into the same refusal every second.
- **1**: it could not run at all (no runtime, no stdin).
