# caustics-pairing

The canvas's pairing sink (docs/canvas.md, *Sinks*): a Bluetooth PIN or
passkey that must be **typed** on this machine while a device pairs, asked
on the canvas's prompt as a **conversation** with the canvas host over the
wire (docs/abi.md, *Prompt*, *Answer*), and handed back over a socket to
the one process that asked.

It is not BlueZ's agent. The bar's Bluetooth sidecar is
(`bluetooth/src/agent.rs`): BlueZ sends every question of one pairing to
one agent, and that sidecar routes them.

| BlueZ asks | where it is asked |
|---|---|
| `RequestConfirmation(device, passkey)` | the bar: "Confirm passkey 123 456 for Pixel 9", Confirm, Reject, as a toast |
| `RequestAuthorization(device)` | the bar: "Allow Pixel 9 to pair", Confirm, Reject |
| `AuthorizeService(device, uuid)` | the bar: "Allow Audio Sink for Pixel 9" (the service's well-known name, else the UUID), Confirm, Reject |
| `DisplayPasskey(device, passkey, entered)`, `DisplayPinCode(device, pin)` | the bar: "Type 123 456 on Pixel 9 (2 of 6 typed)", a toast until the device pairs, BlueZ cancels, a minute passes or it is clicked |
| `RequestPinCode(device)` | **this sink**: the ask "PIN", 1 to 16 characters |
| `RequestPasskey(device)` | **this sink**: the ask "Passkey", six digits |

A yes or a no needs no keyboard, so it stays on the bar; only the two that
need typing come here. Most devices never ask either: they compare a
passkey. Older keyboards and some legacy devices do.

```
caustics-pairing
caustics-pairing --ask <command…>
```

No arguments as a sink. The first frame on stdout is `Hello`; then `Prompt`
frames, `Answer` frames are read from stdin, every witness is one line on
stderr, and the process exits when stdin closes, removing its socket.
`--ask` is a client instead: the words as one line to a running sink's
socket and its reply on stdout, for trying the sink by hand and for the
nested gate.

## The socket

`$XDG_RUNTIME_DIR/caustics-pairing.sock`, one command a line and one reply
a line, as the lock's and the wallpaper's sockets are:

```
pin <title>        → text <PIN>        | cancelled | error <why>
passkey <title>    → text <six digits> | cancelled | error <why>
cancel             → ok                 (BlueZ's Cancel reached the agent)
status             → ok asking=yes|no
```

The title is the device's alias, or its address, and is the prompt's title;
a NUL or a carriage return in it, or none at all, is an `error`. A socket
another sink still answers after a two-second window is left alone, as the
lock leaves another lock's; a stale one is taken over.

The Bluetooth sidecar looks at `status` when it registers with BlueZ and
each time its menu opens: while a sink answers, it registers as
`KeyboardDisplay`; while none does, as `DisplayYesNo`, and a device that
still asks for a typed PIN is refused with one witness that names
canvas/README.md, *Enabling the pairing agent*.

## Behaviour

- **One conversation at a time.** A `pin` or a `passkey` while one is asked
  is answered `error busy` at once and shows nothing: BlueZ holds one
  request per agent, so a second means the asker has lost track.
- **A conversation** is a `PROMPT_INFO` title, a `PROMPT_ECHO` "PIN" or
  "Passkey", and one end. An answer BlueZ would refuse (a PIN past 16
  bytes or not text, a passkey that is not six digits) is a `PROMPT_ERROR`
  line and the ask again, before anything reaches BlueZ.
- **The reply** is `text <what was typed>` and `PROMPT_DONE` 1; a dismissal
  (`ANSWER_CANCELLED`) is `cancelled` and `PROMPT_DONE` 0; a `cancel` is
  `PROMPT_CANCEL`, `ok` to the canceller and `cancelled` to the asker. The
  host going is `error the pairing sink is leaving`.
- **No PIN or passkey is written anywhere but the reply line**, and no
  witness carries one. The line is built once, at its full size, in a
  buffer zeroed when it drops, and the host's stdin is read unbuffered;
  `tests/zeroing.rs` watches every block the conductor frees. The kernel's
  socket buffer is outside this process's reach. On the other end, the
  Bluetooth sidecar reads the line into a zeroing buffer too and zeroes the
  PIN once zbus has sent BlueZ's reply (`bluetooth/tests/zeroing.rs`).
- **While the session is locked**, the prompt is the lock's alone, so an
  ask then is dismissed at once and BlueZ hears `Rejected`.

## In the canvas's set

`caustics-pairing` is installed with the rest of the set by
`bar/install.sh`. The canvas host answers its prompts (docs/canvas.md, *The
prompt core*), but **does not start it by default**: it is taken on by
choice, through `$CAUSTICS_SIDECARS` in a drop-in on the canvas's unit
(canvas/README.md, *Enabling the pairing agent*).

The nested gate runs it under `bar/abi/examples/fake_host.rs`, which plays
the canvas host and answers each ask from a script, and asks it both
directly with `--ask` and through the bar's Bluetooth agent in its dry run
(docs/testing.md).

## Witness lines

| line | means |
|---|---|
| `serving /run/user/N/caustics-pairing.sock; asked only for a PIN or a passkey to type` | up |
| `absent: runtime-dir` | no `XDG_RUNTIME_DIR`; a wait for stdin to close follows |
| `…/caustics-pairing.sock: … is another pairing sink's (…)` | another sink answers; a wait follows |
| `conversation N: "<device>" asks a PIN` | a conversation began (`a passkey`) |
| `conversation N: answered; the PIN sent` | the reply went (`the passkey`) |
| `conversation N: dismissed (M unanswered since start)` | the person dismissed it |
| `conversation N: cancelled by BlueZ (M unanswered since start)` | a `cancel` took it down |
| `conversation N: an answer BlueZ would refuse; asked again` | an error line and the ask again |
| `"<device>" asks a PIN: busy with conversation N; refused` | `error busy` |

## Exit codes

- **0**: the host closed stdin. A refusal (no runtime directory, a socket
  another sink holds) is one witness, then a wait for stdin to close, so
  the host does not restart the sidecar into the same refusal every
  second. With `--ask`, a reply came.
- **1**: it could not run at all (no stdin); with `--ask`, nothing
  answered on the socket.
