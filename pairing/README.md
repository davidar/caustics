# caustics-pairing

The session's BlueZ pairing agent, as a sink sidecar of the canvas
(docs/canvas.md, *Sinks*). It registers with BlueZ's agent manager as the
default agent, turns each question BlueZ puts while a device pairs (confirm
a passkey, type a PIN or a passkey, allow a pairing or a service) into a
**prompt conversation** with the canvas host over the wire (docs/abi.md,
*Prompt*, *Answer*), and hands the answer to BlueZ in the method's reply,
and nowhere else. Most often the pairing is one the bar's Bluetooth
sidecar has just started with a click on a device in range
(`bluetooth/src/actions.rs`).

```
caustics-pairing
```

No arguments. The first frame on stdout is `Hello`; then `Prompt` frames,
`Answer` frames are read from stdin, every witness is one line on stderr, and
the process unregisters and exits when stdin closes.

## Behaviour

- **Registration**: `RegisterAgent("/caustics/pairing", "KeyboardDisplay")`
  then `RequestDefaultAgent` on `org.bluez.AgentManager1`, from the
  system-bus connection that serves `org.bluez.Agent1` there, because BlueZ
  calls an agent back on the connection that registered it. The default
  agent is the one BlueZ asks for a pairing started by a connection with no
  agent of its own, which is how the bar's `Pair()` reaches this one. Only
  BlueZ's own connection may call the agent. BlueZ not running yet is waited
  for, and BlueZ restarting is answered by registering again. A
  registration BlueZ refuses is one witness naming its error and a wait.
- **What each method is on the canvas**, under the device's `Alias` as the
  title (its address when the alias cannot be read):

  | method | the conversation | the reply |
  |---|---|---|
  | `RequestConfirmation(device, passkey)` | line "Confirm passkey 123456", ask "Enter to confirm, Escape to reject" | returns on Enter |
  | `RequestAuthorization(device)` | line "Allow pairing", the same ask | returns on Enter |
  | `AuthorizeService(device, uuid)` | line "Allow Audio Sink" (the service's well-known name, else the UUID), the same ask | returns on Enter |
  | `RequestPinCode(device)` | ask "PIN" | the PIN typed, 1 to 16 characters |
  | `RequestPasskey(device)` | ask "Passkey" | the passkey typed, six digits |
  | `DisplayPinCode(device, pincode)` | line "Type 1234 on the device", no ask | returns at once |
  | `DisplayPasskey(device, passkey, entered)` | line "Type 123456 on the device (3 typed)", no ask; a call for the device on screen updates the line | returns at once |

  A confirmation's ask is echoed and confirmed by Enter on the empty field,
  which the canvas host sends as an empty answer; text typed into it is an
  error line and the ask again. A PIN or a passkey BlueZ would refuse is an
  error line and the ask again, before BlueZ sees it.
- **A code to type on the device** is held on the canvas until the device's
  `Paired` turns true (one `PropertiesChanged` from BlueZ on that device,
  watched only while the code is shown), BlueZ cancels or releases the
  agent, the user dismisses it, or a minute passes. BlueZ owes no reply to a
  display and sends no `Cancel` for one, so the watch is what ends it.
- **One conversation at a time.** Requests that arrive meanwhile queue in
  arrival order. A request that arrives while a code is shown ends the code
  first: BlueZ holds one request per agent, so it has moved on.
- **`Cancel` and `Release`** end everything this agent holds: the
  conversation on screen with `PROMPT_CANCEL`, and every pending method with
  `org.bluez.Error.Canceled`. A conversation ends with `PROMPT_DONE` 1 when
  it was confirmed, answered or its device paired, and 0 otherwise.
- **A dismissal** (`ANSWER_CANCELLED`) returns `org.bluez.Error.Rejected`.
- **No PIN or passkey is written anywhere but the reply**, and no witness
  carries one, nor a passkey BlueZ shows (that one is on the other device's
  screen too, and is in the prompt's line). The PIN's copy in the reply is
  zeroed once zbus has sent it, and the host's stdin is read unbuffered. A
  passkey goes back as a number with nothing behind it to zero. The method
  return's own buffer is zbus's, and the copies in the bus and in BlueZ are
  outside this process's reach. `tests/zeroing.rs` watches every block the
  conductor and the reply free.

Another default agent (a desktop's Bluetooth applet) is replaced by this
one when it registers, and takes the default back if it registers later:
BlueZ keeps the last one that asked. Stopping the other makes this one the
agent for good.

## Settings

From the sidecar's own environment:

| variable | default | |
|---|---|---|
| `CAUSTICS_PAIRING_DRY_RUN` | unset | `1`: touch no system bus at all, and serve the agent on the session bus for any caller, so a test can play BlueZ |

In the dry run the agent is at the unique name its witness gives, and a
test calls it with `busctl --user call … RequestConfirmation`;
`bar/abi/examples/fake_host.rs` runs the sidecar on its pipes the way the
canvas host does and answers each ask from a script. The nested gate runs
the two together (docs/testing.md).

## Witness lines

| line | means |
|---|---|
| `the pairing agent /caustics/pairing (KeyboardDisplay), registered with org.bluez as the default` | registered |
| `absent: bluez` | BlueZ is not on the bus yet; registering waits for it |
| `BlueZ refused the registration: org.bluez.Error.AlreadyExists (…)` | another agent holds this path, or BlueZ said no; a wait follows |
| `BlueZ is :1.N now; registering again` | BlueZ restarted |
| `conversation N: "<device>" asks a confirmation` | a conversation began (`a PIN`, `a passkey`) |
| `conversation N: "<device>" shows a code to type on the device` | a display began |
| `conversation N: confirmed` / `answered; the PIN sent` / `answered; the passkey sent` | the reply went |
| `conversation N: rejected (M unanswered since start)` | the user dismissed it |
| `conversation N: cancelled by BlueZ (M unanswered since start)` | BlueZ withdrew it |
| `conversation N: the code is gone (paired)` | a display ended (`timed out`, `dismissed`, `superseded by a request`) |
| `dry run: the agent is at :1.N /caustics/pairing on the session bus; …` | the dry run is up |

## In the canvas's set

`caustics-pairing` is installed with the rest of the set by
`bar/install.sh`. The canvas host answers its prompts (docs/canvas.md, *The
prompt core*), but **does not start it by default**: it is taken on by
choice, through `$CAUSTICS_SIDECARS` in a drop-in on the canvas's unit
(canvas/README.md, *Enabling the pairing agent*). While the session is
locked, the prompt is the lock's alone, so a request then is dismissed at
once and BlueZ hears `Rejected`.

## Exit codes

- **0**: the host closed stdin. A refusal (no system bus, a registration
  BlueZ refused) is one witness, then a wait for stdin to close, so the host
  does not restart the sidecar into the same refusal every second.
- **1**: it could not run at all (no runtime, no stdin).
