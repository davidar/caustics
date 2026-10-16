#!/usr/bin/env python3
"""caustics-notify-watch: a bug report when a notification's click goes nowhere.

A standing check, for a session that runs the bar day to day: it watches
for a click on a notification that fails to take you to the app's window,
and collects what a bug report needs when it does. What should happen: a
click on a notification row sends the app `ActivationToken` then
`ActionInvoked`, both addressed to the app's own connection, and the app
uses the token to bring its window forward — niri reports that as a
`WindowFocusChanged` for a window of that app.

This watches two public, read-only streams and correlates them:

* `busctl --user monitor` on `org.freedesktop.Notifications` — the `Notify`
  calls (which app, under what id, with which `desktop-entry`), and the
  daemon's three signals, whoever the daemon is (caustics-notify or
  cosmic-notifications: the checks are the specification's, not caustics-notify's);
* `niri msg -j event-stream` — the window list, focus and urgency.

**The rule.** After `ActionInvoked` for an app that has a window, a
`WindowFocusChanged` to one of that app's windows must follow within
`GRACE` seconds, unless one was focused already. If none does, a report
directory is written under `$XDG_STATE_HOME/caustics/notify-watch/` with the
last `KEEP` seconds of both streams, the window list, the two journals and
the daemon's and the app's bus identities, plus `REPORT.md` saying which
check failed: no focus; only an urgency change (niri judged the token
stale: a pointer or keyboard enter reached niri after the click's serial);
no `ActivationToken` before the action; a signal broadcast rather than
addressed. One `notify-send` at low urgency names the directory, and the
journal gets a `FAIL` line. An app
with no window (notify-send, a script) is not judged.

Matching an app to its windows is a heuristic: the `desktop-entry` hint
and the app name against niri's `app_id`, case-insensitively, by equality,
containment, or the last dotted component. A wrong match is a false report,
which is cheap; a missed match is a silent miss, which `REPORT.md` for a
true failure would show as "no window".

Run it as `notify/tools/notify-watch.py`, or copy it to `~/.local/bin/caustics-notify-watch` for the user unit beside it in `notify/tools/` (nothing installs it). `--self-test` runs the correlation logic
against synthetic lines and writes nothing.
"""

import collections
import datetime
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time

GRACE = 3.0  # seconds a click has to produce a focus change
KEEP = 90.0  # seconds of each stream kept for a report
MAX_REPORTS = 40  # oldest pruned beyond this

NOTIFICATIONS = "org.freedesktop.Notifications"


def log(line):
    sys.stderr.write(f"notify-watch: {line}\n")
    sys.stderr.flush()


def state_dir():
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state")
    return os.path.join(base, "caustics", "notify-watch")


def app_matches(app_id, names):
    """Does niri's `app_id` belong to an app called any of `names`?"""
    if not app_id:
        return False
    a = app_id.lower()
    tail = a.rsplit(".", 1)[-1]
    for name in names:
        if not name:
            continue
        n = name.lower()
        if n == a or n == tail or (len(n) >= 4 and (n in a or a in n)):
            return True
    return False


class Watch:
    """The correlation. Pure: lines in, verdicts out; `collect` and `now`
    are parameters so the self-test can drive it."""

    def __init__(self, now=time.monotonic, collect=None, grace=GRACE, keep=KEEP):
        self.now = now
        self.collect = collect or (lambda report: None)
        self.grace = grace
        self.keep = keep
        self.bus = collections.deque()  # (t, line)
        self.niri = collections.deque()
        self.calls = {}  # cookie -> Notify call facts, until its reply
        self.notes = {}  # notification id -> facts
        self.tokens = {}  # notification id -> (t, directed)
        self.pending = []  # expectations waiting for a focus change
        self.windows = {}  # niri window id -> window
        self.focused = None
        self.daemon = None  # the daemon's unique name, from its signals

    # --- the bus -------------------------------------------------------

    def bus_line(self, line, t=None):
        t = self.now() if t is None else t
        self._keep(self.bus, (t, line))
        try:
            m = json.loads(line)
        except ValueError:
            return
        if m.get("interface") != NOTIFICATIONS and m.get("type") != "method_return":
            return
        kind, member = m.get("type"), m.get("member")
        data = (m.get("payload") or {}).get("data") or []
        if kind == "method_call" and member == "Notify" and len(data) >= 7:
            hints = data[6] if isinstance(data[6], dict) else {}
            entry = hints.get("desktop-entry", {})
            self.calls[m.get("cookie")] = {
                "sender": m.get("sender"),
                "app": data[0],
                "replaces": data[1],
                "desktop_entry": entry.get("data") if isinstance(entry, dict) else None,
                "actions": data[5] if isinstance(data[5], list) else [],
                "summary": data[3],
            }
        elif kind == "method_return":
            call = self.calls.pop(m.get("reply_cookie"), None)
            if call is not None and data:
                call["t"] = t
                call["id"] = data[0]
                self.notes[data[0]] = call
        elif kind == "signal":
            self.daemon = m.get("sender")
            if member == "ActivationToken" and data:
                self.tokens[data[0]] = (t, m.get("destination") is not None)
            elif member == "ActionInvoked" and len(data) >= 2:
                self._action(m, data[0], data[1], t)

    def _action(self, m, nid, key, t):
        note = self.notes.get(nid)
        names = [note["app"], note.get("desktop_entry")] if note else []
        token = self.tokens.pop(nid, None)
        matching = [w for w in self.windows.values() if app_matches(w.get("app_id"), names)]
        expectation = {
            "t": t,
            "id": nid,
            "key": key,
            "app": note["app"] if note else None,
            "desktop_entry": note.get("desktop_entry") if note else None,
            "summary": note.get("summary") if note else None,
            "sender": note.get("sender") if note else None,
            "destination": m.get("destination"),
            "daemon": m.get("sender"),
            "token_before": token is not None and token[0] <= t,
            "token_directed": bool(token and token[1]),
            "windows": [w.get("id") for w in matching],
            "focused_before": any(w.get("id") == self.focused for w in matching),
            "urgent": [],
        }
        if note is None:
            log(f"action id={nid} key={key!r}: its Notify predates this watch; not judged")
            return
        if not matching:
            log(f"action id={nid} key={key!r} app={note['app']!r}: no window of that app; not judged")
            return
        if expectation["focused_before"]:
            log(f"action id={nid} app={note['app']!r}: its window was focused already; ok")
            self._signals_ok(expectation)
            return
        self.pending.append(expectation)

    def _signals_ok(self, e):
        """The signal-shape checks, reported even when focus arrived."""
        problems = self.signal_problems(e)
        if problems:
            e["reason"] = "; ".join(problems)
            self.collect(self.report(e))

    @staticmethod
    def signal_problems(e):
        problems = []
        if not e["token_before"]:
            problems.append("no ActivationToken before ActionInvoked")
        elif not e["token_directed"]:
            problems.append("ActivationToken was broadcast, not addressed")
        if e["destination"] is None:
            problems.append("ActionInvoked was broadcast, not addressed")
        return problems

    # --- niri ----------------------------------------------------------

    def niri_line(self, line, t=None):
        t = self.now() if t is None else t
        self._keep(self.niri, (t, line))
        try:
            ev = json.loads(line)
        except ValueError:
            return
        if "WindowsChanged" in ev:
            self.windows = {w["id"]: w for w in ev["WindowsChanged"]["windows"]}
            for w in self.windows.values():
                if w.get("is_focused"):
                    self.focused = w["id"]
        elif "WindowOpenedOrChanged" in ev:
            w = ev["WindowOpenedOrChanged"]["window"]
            self.windows[w["id"]] = w
            if w.get("is_focused"):
                self.focused = w["id"]
        elif "WindowClosed" in ev:
            self.windows.pop(ev["WindowClosed"]["id"], None)
        elif "WindowFocusChanged" in ev:
            wid = ev["WindowFocusChanged"]["id"]
            self.focused = wid
            still = []
            for e in self.pending:
                if wid in e["windows"]:
                    log(f"action id={e['id']} app={e['app']!r}: focus reached window {wid} after {t - e['t']:.3f} s; ok")
                    self._signals_ok(e)
                else:
                    still.append(e)
            self.pending = still
        elif "WindowUrgencyChanged" in ev:
            u = ev["WindowUrgencyChanged"]
            for e in self.pending:
                if u.get("id") in e["windows"] and u.get("urgent"):
                    e["urgent"].append(u["id"])

    # --- time ----------------------------------------------------------

    def tick(self, t=None):
        t = self.now() if t is None else t
        still = []
        for e in self.pending:
            if t - e["t"] < self.grace:
                still.append(e)
                continue
            reasons = []
            if e["urgent"]:
                reasons.append(
                    f"no focus within {self.grace:.0f} s; niri set window(s) {e['urgent']} urgent instead, "
                    "which is what it does with a token whose serial it judges stale"
                )
            else:
                reasons.append(
                    f"no focus change and no urgency change within {self.grace:.0f} s: "
                    "either the app never asked niri to activate, or asked with a token niri had already dropped"
                )
            reasons.extend(self.signal_problems(e))
            e["reason"] = "; ".join(reasons)
            self.collect(self.report(e))
        self.pending = still
        for ring in (self.bus, self.niri):
            self._trim(ring, t)

    def _keep(self, ring, item):
        ring.append(item)
        self._trim(ring, item[0])

    def _trim(self, ring, t):
        while ring and t - ring[0][0] > self.keep:
            ring.popleft()

    # --- the report ----------------------------------------------------

    def report(self, e):
        return {
            "expectation": e,
            "daemon": self.daemon,
            "windows": list(self.windows.values()),
            "bus": [line for _, line in self.bus],
            "niri": [line for _, line in self.niri],
        }


# --- the real world ----------------------------------------------------


def run(cmd):
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        return out.stdout + (out.stderr if out.returncode else "")
    except (OSError, subprocess.SubprocessError) as err:
        return f"({' '.join(cmd)} failed: {err})\n"


def collect(report):
    e = report["expectation"]
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    root = state_dir()
    path = os.path.join(root, f"{stamp}-id{e['id']}")
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, "bus.jsonl"), "w") as f:
        f.write("\n".join(report["bus"]) + "\n")
    with open(os.path.join(path, "niri.jsonl"), "w") as f:
        f.write("\n".join(report["niri"]) + "\n")
    with open(os.path.join(path, "windows.json"), "w") as f:
        json.dump(report["windows"], f, indent=1)
    with open(os.path.join(path, "windows-now.json"), "w") as f:
        f.write(run(["niri", "msg", "-j", "windows"]))
    with open(os.path.join(path, "journal-caustics-bar.txt"), "w") as f:
        f.write(run(["journalctl", "--user", "-u", "caustics-bar.service", "--since", "-3min", "--no-pager"]))
    with open(os.path.join(path, "journal-niri.txt"), "w") as f:
        f.write(run(["journalctl", "--user", "-t", "niri", "--since", "-3min", "--no-pager"]))
    with open(os.path.join(path, "bus-names.txt"), "w") as f:
        f.write("# the daemon\n" + run(["busctl", "--user", "status", NOTIFICATIONS]))
        if e.get("sender"):
            f.write(f"\n# the app's connection {e['sender']}\n" + run(["busctl", "--user", "status", e["sender"]]))
    lines = [
        "# notify-watch: a notification's click went nowhere",
        "",
        f"- when: {stamp}",
        f"- notification: id {e['id']}, app {e['app']!r}, desktop-entry {e['desktop_entry']!r}, summary {e['summary']!r}",
        f"- action: key {e['key']!r}, from daemon {e['daemon']} to {e['destination']} (the app's connection: {e['sender']})",
        f"- the app's windows (niri ids): {e['windows']}; focused before the click: {e['focused_before']}",
        f"- ActivationToken before the action: {e['token_before']}; addressed: {e['token_directed']}",
        f"- urgent set on: {e['urgent']}",
        "",
        f"**What failed:** {e['reason']}",
        "",
        "Files: `bus.jsonl` (the last 90 s on org.freedesktop.Notifications), `niri.jsonl` (niri's event stream),",
        "`windows.json` (as the watch knew them), `windows-now.json`, `journal-caustics-bar.txt`, `journal-niri.txt`,",
        "`bus-names.txt` (who owned the name, and the app's pid/cgroup).",
        "",
        "Context: the daemon sends ActivationToken then ActionInvoked to the app's own connection; a token whose serial niri judges stale sets urgent only;",
        "niri's rule is `token_created` in src/handlers/mod.rs; its `debug { honor-xdg-activation-with-invalid-serial }`",
        "makes a stale token count.",
    ]
    with open(os.path.join(path, "REPORT.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    log(f"FAIL id={e['id']} app={e['app']!r}: {e['reason']} — report at {path}")
    subprocess.Popen(
        ["notify-send", "-u", "low", "-a", "caustics-notify-watch", "-t", "8000",
         f"A click on {e['app']} went nowhere", f"Report at {path}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    prune(root)


def prune(root):
    try:
        dirs = sorted(d for d in os.listdir(root) if os.path.isdir(os.path.join(root, d)))
    except OSError:
        return
    for old in dirs[:-MAX_REPORTS] if len(dirs) > MAX_REPORTS else []:
        shutil.rmtree(os.path.join(root, old), ignore_errors=True)


def pump(cmd, tag, q):
    """One stream into the queue, line by line; the watch exits when either ends."""
    try:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    except OSError as err:
        q.put((tag, None, f"{cmd[0]}: {err}"))
        return
    for line in proc.stdout:
        q.put((tag, line.rstrip("\n"), None))
    q.put((tag, None, f"{' '.join(cmd)} ended (exit {proc.wait()})"))


def main():
    if "--self-test" in sys.argv:
        return self_test()
    q = queue.Queue()
    bus_cmd = [
        "busctl", "--user", "monitor", "--json=short",
        "--match", f"type='signal',interface='{NOTIFICATIONS}'",
        "--match", f"type='method_call',interface='{NOTIFICATIONS}',member='Notify'",
        "--match", "type='method_return'",
    ]
    niri_cmd = ["niri", "msg", "-j", "event-stream"]
    threading.Thread(target=pump, args=(bus_cmd, "bus", q), daemon=True).start()
    threading.Thread(target=pump, args=(niri_cmd, "niri", q), daemon=True).start()
    watch = Watch(collect=collect)
    log(f"watching {NOTIFICATIONS} and niri; reports under {state_dir()}")
    while True:
        try:
            tag, line, end = q.get(timeout=0.5)
        except queue.Empty:
            watch.tick()
            continue
        if line is None:
            log(f"{end}; exiting")
            return 1
        (watch.bus_line if tag == "bus" else watch.niri_line)(line)
        watch.tick()


# --- the self-test -----------------------------------------------------


def self_test():
    def bus(kind, member, data, cookie=None, reply=None, sender=":1.9", dest=":1.5"):
        m = {"type": kind, "sender": sender, "destination": dest, "interface": NOTIFICATIONS,
             "member": member, "payload": {"data": data}}
        if cookie is not None:
            m["cookie"] = cookie
        if reply is not None:
            m["reply_cookie"] = reply
            m["interface"] = None
        return json.dumps(m)

    notify = bus("method_call", "Notify", ["discord", 0, "", "hi", "body", ["default", "View"],
                                           {"desktop-entry": {"type": "s", "data": "discord"}}, -1],
                 cookie=7, sender=":1.5", dest=":1.9")
    reply = bus("method_return", None, [61], reply=7, sender=":1.9", dest=":1.5")
    token = bus("signal", "ActivationToken", [61, "tok"])
    action = bus("signal", "ActionInvoked", [61, "default"])
    windows = json.dumps({"WindowsChanged": {"windows": [
        {"id": 144, "app_id": "discord", "is_focused": False},
        {"id": 2, "app_id": "kitty", "is_focused": True}]}})
    focus = json.dumps({"WindowFocusChanged": {"id": 144}})
    urgent = json.dumps({"WindowUrgencyChanged": {"id": 144, "urgent": True}})

    def scenario(lines, until):
        reports = []
        w = Watch(now=lambda: 0.0, collect=reports.append, grace=3.0)
        for t, tag, line in lines:
            (w.bus_line if tag == "bus" else w.niri_line)(line, t)
            w.tick(t)
        w.tick(until)
        return reports

    base = [(0.0, "niri", windows), (1.0, "bus", notify), (1.0, "bus", reply), (5.0, "bus", token), (5.0, "bus", action)]
    # 1. focus arrives: no report.
    assert scenario(base + [(5.1, "niri", focus)], 10.0) == []
    # 2. nothing arrives: a report naming the silence.
    r = scenario(base, 10.0)
    assert len(r) == 1 and "no focus change and no urgency" in r[0]["expectation"]["reason"], r
    assert r[0]["expectation"]["windows"] == [144] and r[0]["expectation"]["token_before"]
    # 3. urgent only: a report naming the stale token.
    r = scenario(base + [(5.2, "niri", urgent)], 10.0)
    assert len(r) == 1 and "urgent instead" in r[0]["expectation"]["reason"], r
    # 4. no token, broadcast action: the signal checks fail even though focus arrived.
    broadcast = bus("signal", "ActionInvoked", [61, "default"], dest=None)
    r = scenario([(0.0, "niri", windows), (1.0, "bus", notify), (1.0, "bus", reply), (5.0, "bus", broadcast), (5.1, "niri", focus)], 10.0)
    assert len(r) == 1 and "no ActivationToken" in r[0]["expectation"]["reason"] and "broadcast" in r[0]["expectation"]["reason"], r
    # 5. an app with no window is not judged.
    nowin = bus("method_call", "Notify", ["notify-send", 0, "", "x", "", ["default", "Open"], {}, -1], cookie=8, sender=":1.7", dest=":1.9")
    r = scenario([(0.0, "niri", windows), (1.0, "bus", nowin), (1.0, "bus", bus("method_return", None, [62], reply=8, sender=":1.9", dest=":1.7")),
                  (5.0, "bus", bus("signal", "ActionInvoked", [62, "default"], dest=":1.7"))], 10.0)
    assert r == [], r
    # 6. the window was focused already: ok, no report.
    focused_windows = json.dumps({"WindowsChanged": {"windows": [{"id": 144, "app_id": "discord", "is_focused": True}]}})
    assert scenario([(0.0, "niri", focused_windows)] + base[1:], 10.0) == []
    # 7. matching: desktop-entry and dotted ids.
    assert app_matches("com.discordapp.Discord", ["discord"]) and app_matches("org.kde.konsole", [None, "konsole"])
    assert not app_matches("kitty", ["discord", None]) and not app_matches(None, ["x"])
    print("notify-watch self-test: 7 scenarios ok")
    return 0


if __name__ == "__main__":
    sys.exit(main() or 0)
