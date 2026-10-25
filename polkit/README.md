# caustics-polkit

The session's polkit authentication agent, as a sink sidecar of the canvas
(docs/canvas.md, *Sinks*). It registers with polkitd for this session, turns
each request into a **prompt conversation** with the canvas host over the
wire (docs/abi.md, *Prompt*, *Answer*), drives
`polkit-agent-helper-1` with what the user typed, and reports the outcome.
The helper runs PAM and tells polkitd itself; this process learns success or
failure. `src/helper.rs` has the helper's line protocol as it was verified
against polkit's sources.

```
caustics-polkit
```

No arguments. The first frame on stdout is `Hello`; then `Prompt` frames,
`Answer` frames are read from stdin, every witness is one line on stderr, and
the process unregisters and exits when stdin closes.

## Behaviour

- **One conversation at a time.** Requests that arrive meanwhile queue in
  arrival order. polkitd cancelling a queued request takes it out silently;
  cancelling the one on screen kills its helper and sends `PROMPT_CANCEL`.
- **A conversation** is a `PROMPT_INFO` title (polkit's message for the
  action), one `Prompt` per line the helper writes (`PAM_PROMPT_ECHO_OFF`
  is `PROMPT_SECRET`, `PAM_PROMPT_ECHO_ON` is `PROMPT_ECHO`, `PAM_TEXT_INFO`
  and `PAM_ERROR_MSG` are `PROMPT_INFO` and `PROMPT_ERROR`), and one
  `PROMPT_DONE`, success only when the helper said `SUCCESS` and exited 0.
- **Identity**: the first `unix-user` offered whose uid is this process's,
  else the first `unix-user` offered. There is no picker; a group is passed
  over.
- **A dismissal** (`ANSWER_CANCELLED`) ends the conversation as a failure,
  and polkitd's call gets `org.freedesktop.PolicyKit1.Error.Cancelled`. Any
  other failure is `….Error.Failed`. One helper run per request: a wrong
  password fails the request.
- **Nothing typed is written anywhere but the helper's stdin**, and no
  witness carries an answer, a cookie or a helper line. Failed conversations
  are counted in the witness, never described.
- polkitd restarting is answered by registering again. Only polkitd's own
  connection may call the agent.

## Settings

From the sidecar's own environment:

| variable | default | |
|---|---|---|
| `CAUSTICS_POLKIT_HELPER` | `/usr/lib/polkit-1/polkit-agent-helper-1` | the helper to spawn |
| `CAUSTICS_POLKIT_DRY_RUN` | unset | `1`: touch no system bus at all, and serve the agent on the session bus for any caller, so a test can play polkitd |

`tests/fake-helper.sh` is the helper the tests use: it speaks the line
protocol and authenticates nobody. `examples/fake_host.rs` runs a sink
sidecar on its pipes the way the canvas host will, answering each ask from a
script; the nested gate runs the two together (docs/testing.md).

## In the canvas's set

`caustics-polkit` is installed with the rest of the set by `bar/install.sh`.
The canvas host starts its sidecars from `$CAUSTICS_SIDECARS` on its unit
(canvas/README.md), and this one belongs there once the host can show a
prompt. **It is not started by the canvas yet**: until the host reads
`Prompt` frames, a registered agent would take the session's requests and
nobody could answer them, so the desktop's own agent stays.

## Exit codes

- **0**: the host closed stdin. A refusal (no system bus, no session to be
  the agent of, another agent already registered for the session) is one
  witness, then a wait for stdin to close, so the host does not restart the
  sidecar into the same refusal every second.
- **1**: it could not run at all (no runtime, no stdin).
