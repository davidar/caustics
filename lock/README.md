# caustics-lock

The session's lock, as a sink sidecar of the canvas (docs/canvas.md,
*Sinks*, *How each job is wired*). It owns logind (this session's `Lock`
signal and its `LockedHint`), the PAM conversation and the fingerprint
reader (fprintd, on the system bus); the canvas host owns
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

The host's half is the canvas's lock surfaces (`canvas/src/lock.rs`): it
takes `ext_session_lock_v1` on this sidecar's `LOCK_LOCK`, draws the
conversation on a lock surface per output, and says `locked` and
`unlocked`. This sidecar is not in the canvas's default set; a drop-in
adds it (*In the canvas's set*).

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
- **The fingerprint reader** is a second way in, beside the password. PAM
  asks one thing at a time (pam_fprintd's manual says, under its
  limitations, that one stack cannot offer a password and a fingerprint at
  once), so the reader is not in the PAM stack: while the session is
  locked the sidecar claims fprintd's default reader for its user and keeps
  a verification going (`VerifyStart` with `any`) as the password
  conversation runs. The conversation on screen says so under its title
  (`Touch the fingerprint reader`, or `Swipe a finger across the reader`),
  and every fresh conversation says it again. A finger that does not match
  is an error line (`Fingerprint not recognised; try again`) and a fresh
  verify after half a second, never a counted failure; a scan to try again
  (too short, off centre) is a line and the same verify. **A match is a
  success**: the conversation on screen ends with `PROMPT_DONE` 1, the ask
  PAM is waiting on is cancelled, and `LOCK_UNLOCK` follows. The password
  keeps working throughout. A line that comes while an answer is with PAM
  waits until PAM speaks, so it never reads as the answer's failure.
- **The reader is released when the lock is over** (the unlock, by either
  way, or the host going): the verify is stopped and the device released,
  within two seconds. While the session is locked the reader is held, and a
  polkit request waits for the unlock anyway (the canvas gives the locked
  prompt to the lock alone); once unlocked, a polkit helper's
  `pam_fprintd` finds the reader free.
- **A reader that goes** while locked (a resume from suspend re-enumerates
  it; another client holds it; fprintd says `verify-disconnected`, or
  leaves the bus) is released and claimed again after a pause that doubles
  from 1 s to 30 s, for as long as the session stays locked; no reader, or
  no fprintd, after one that went is taken as one on its way back. **A reader that cannot verify**
  for this lock (no reader, no enrolled prints, no permission, no system
  bus, no fprintd) is one witness line, and the password alone, until the
  next lock.
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
status` prints the lock's reply, without its `ok `, as a second line after
the wallpaper's (`locked=no attempts=0`) when this socket answers; without
a lock sidecar there is no line, and a lock that refused to run says why on
stderr.

## Settings

From the sidecar's own environment:

| variable | default | |
|---|---|---|
| `CAUSTICS_LOCK_PAM_SERVICE` | `swaylock` | the PAM service (`/etc/pam.d/<service>`); `swaylock` is the one a session built on the sway tools already has. Keeping the fingerprint module off the unlock path is that service file's business, and this is how a different one is chosen: the reader is spoken to directly, so a stack that also waits on it only delays the password ask by its timeout |
| `CAUSTICS_LOCK_FINGERPRINT` | `1` | `0` is no reader: no claim, the password alone. Unset or anything else is the reader verifying while locked |
| `CAUSTICS_LOCK_DRY_RUN_SECRET` | unset | the **dry run**: PAM is not touched, and a conversation is one echo-off ask, `Password: `, compared with these bytes (not in constant time: they are in the environment, which is no secret). Empty is no dry run |

The dry run still speaks to logind and fprintd if a system bus answers, so
a test points `DBUS_SYSTEM_BUS_ADDRESS` at a socket that does not exist, or
at a private bus with `examples/fake_fprintd.rs` on it, which the nested
gate runs beside the dry run's password. Without a system
bus, or without a session on it, there is **no logind**: one witness, no
signals, no hint, and the socket's `lock` as the only trigger. The tests
(`tests/dry_run.rs`) and the nested gate run exactly that, under
`bar/abi/examples/fake_host.rs` in the host's place, and the nested gate
again under the canvas host itself, locking the nested compositor
(docs/testing.md).

## The witness lines

Each on stderr as `caustics-lock: …`; the fingerprint's are the ones other
tooling reads. None carries a secret, or which finger matched.

```
fingerprint: reader <name> claimed for <user>; verifying
fingerprint: not recognised
fingerprint: <a scan to try again: Swipe too short; try again, …>
fingerprint matched; unlocking
fingerprint: reader gone (<why>); again in <n> s
fingerprint: no reader; password only           or no enrolled prints for <user>, no system bus (…), …
fingerprint: released                           the lock is over, the reader let go
fingerprint: off (CAUSTICS_LOCK_FINGERPRINT=0)
```

## In the canvas's set

`caustics-lock` is installed with the rest of the set by `bar/install.sh`.
The canvas host starts the sidecars `$CAUSTICS_SIDECARS` names on its unit,
else its compiled set (`canvas/src/sidecars.rs`). **It is not started by
default**: two lockers on one session fight for the lock (the compositor
gives it to one and tells the other `finished`, which the canvas host exits
on), and a session usually has one hooked to logind's `Lock` signal
already. A unit drop-in that adds `caustics-lock` to the list is how it is
taken on, after the previous locker is unhooked from that signal, and with
the previous locker kept installed until this one has locked and unlocked
through a suspend (canvas/README.md, *Enabling the lock*; docs/canvas.md,
*Order*).

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
