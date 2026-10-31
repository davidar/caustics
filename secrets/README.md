# caustics-secrets

The session's NetworkManager secret agent, as a sink sidecar of the canvas
(docs/canvas.md, *Sinks*). It registers with NetworkManager's agent
manager, turns each request for a secret NetworkManager has not got into a
**prompt conversation** with the canvas host over the wire (docs/abi.md,
*Prompt*, *Answer*), and hands what the user typed to NetworkManager in the
method's reply, and nowhere else. Most often the request is the passphrase
of a Wi-Fi network the bar's network sidecar has just asked
NetworkManager to join (`network/src/actions.rs`), or of one whose password
changed.

```
caustics-secrets
```

No arguments. The first frame on stdout is `Hello`; then `Prompt` frames,
`Answer` frames are read from stdin, every witness is one line on stderr, and
the process unregisters and exits when stdin closes.

## Behaviour

- **Registration**: `RegisterWithCapabilities("caustics.secrets", 0)` on
  `org.freedesktop.NetworkManager.AgentManager`, from the system-bus
  connection that serves the agent at
  `/org/freedesktop/NetworkManager/SecretAgent`, because NetworkManager
  calls an agent back on the connection that registered it. Only
  NetworkManager's own connection may call the agent. NetworkManager not
  running yet is waited for, and NetworkManager restarting is answered by
  registering again.
- **One conversation at a time.** Requests that arrive meanwhile queue in
  arrival order. `CancelGetSecrets` takes a queued request out silently and
  ends the one on screen with `PROMPT_CANCEL`; its call returns
  `org.freedesktop.NetworkManager.SecretAgent.Error.AgentCanceled`.
- **A conversation** is a `PROMPT_INFO` title (the connection's
  `connection.id`, which for a network the bar joined is its SSID), a
  `PROMPT_ERROR` "Wrong password" when NetworkManager says the last answer
  was refused (`REQUEST_NEW`), one `PROMPT_SECRET`, and one end:
  `PROMPT_DONE` 1 when the answer went into the reply, 0 otherwise.
- **What is asked**: for `802-11-wireless-security` with WPA-PSK or WPA3
  (SAE) key management, the `psk`, as "Password"; for `802-1x`, the key the
  hint names (`password` unless one is named); for any other setting, the
  key the hint names. A setting with nothing to ask for returns
  `….Error.NoSecrets` with a witness naming it. A WPA-PSK passphrase that is
  not 8 to 63 characters (or 64 hexadecimal digits), or an answer that is
  not text, is an error line and the ask again, before NetworkManager sees
  it.
- **No interaction, no prompt**: a call without `ALLOW_INTERACTION` returns
  `….Error.NoSecrets` at once, and nothing is shown.
- **A dismissal** (`ANSWER_CANCELLED`) returns `….Error.UserCanceled`.
- **The reply** is `{ setting: { key: <the text> } }` and nothing else.
  `SaveSecrets` and `DeleteSecrets` return at once: this agent stores
  nothing. NetworkManager keeps a system-owned secret in the profile
  itself.
- **Nothing typed is written anywhere but the reply**, and no witness
  carries an answer; an SSID is witnessed. The reply's copy of the text is
  zeroed once zbus has sent it, and the host's stdin is read unbuffered.
  The method return's own buffer is zbus's, and the copies in the bus and
  in NetworkManager are outside this process's reach. `tests/zeroing.rs`
  watches every block the conductor and the reply free.

Another agent of the same user already registered is fine: NetworkManager
asks the requesting user's agents one at a time until one gives the
secret, and the first answer wins, so a desktop's own agent and this one
can both be running. Which one asks first is NetworkManager's choice, and a
dismissal in either ends the request; stopping the other makes it this
one.

## Settings

From the sidecar's own environment:

| variable | default | |
|---|---|---|
| `CAUSTICS_SECRETS_DRY_RUN` | unset | `1`: touch no system bus at all, and serve the agent on the session bus for any caller, so a test can play NetworkManager |

In the dry run the agent is at the unique name its witness gives, and a
test calls it with `busctl --user call … GetSecrets`;
`bar/abi/examples/fake_host.rs` runs the sidecar on its pipes the way the
canvas host does and answers each ask from a script. The nested gate runs
the two together (docs/testing.md).

## Witness lines

| line | means |
|---|---|
| `the secret agent caustics.secrets of this user, registered with org.freedesktop.NetworkManager` | registered |
| `absent: networkmanager` | NetworkManager is not on the bus yet; registering waits for it |
| `NetworkManager is :1.N now; registered again` | NetworkManager restarted |
| `conversation N: "<connection>" <setting>.<key>` | a conversation began |
| `conversation N: answered; <setting>.<key> sent` | the reply went |
| `conversation N: dismissed (M unanswered since start)` | the user dismissed it |
| `"<connection>" <setting>: no interaction allowed (flags 0x…); no secrets` | answered NoSecrets with no prompt |
| `dry run: the agent is at :1.N /org/freedesktop/NetworkManager/SecretAgent on the session bus; …` | the dry run is up |

## In the canvas's set

`caustics-secrets` is installed with the rest of the set by
`bar/install.sh`. The canvas host answers its prompts (docs/canvas.md, *The
prompt core*), but **does not start it by default**: it is taken on by
choice, through `$CAUSTICS_SIDECARS` in a drop-in on the canvas's unit
(canvas/README.md, *Enabling the secret agent*). While the session is
locked, the prompt is the lock's alone, so a request then is dismissed at
once and NetworkManager hears `UserCanceled`.

## Exit codes

- **0**: the host closed stdin. A refusal (no system bus, a registration
  NetworkManager refused) is one witness, then a wait for stdin to close, so
  the host does not restart the sidecar into the same refusal every second.
- **1**: it could not run at all (no runtime, no stdin).
