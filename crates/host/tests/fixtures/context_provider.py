#!/usr/bin/env python3
"""Language-neutral context provider fixture: public framed protocol only.

Search and preview echo owner-defined selectors and revalidate their version.
The single Operation records its original request so a retry can prove that
the Host returned the retained result instead of executing it again.
"""
import json
import hashlib
from pathlib import Path
import struct
import sys
import time

sequence = 0
executions = []
notes = {"alpha": {"version": 1, "text": "首行 alpha\n"}, "beta": {"version": 3, "text": "beta 🧬\n"}}
cached_observation = None


def read():
    size = sys.stdin.buffer.read(4)
    return json.loads(sys.stdin.buffer.read(struct.unpack(">I", size)[0])) if size else None


def send(request, kind, data=None):
    global sequence
    sequence += 1
    body = {"type": kind}
    if data is not None:
        body["data"] = data
    encoded = json.dumps({"protocol_version": 1, "connection": connection,
        "instance": identity["instance"], "sequence": sequence,
        "request": request, "body": body}).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(encoded)) + encoded)
    sys.stdout.buffer.flush()


def result(request, data):
    send(request, "query_result", {"data": data, "completeness": "complete", "source": None})


def reference(name):
    return {"provider": identity, "contribution": "notes",
            "selector": {"note": name, "version": notes[name]["version"]}}


def item(name):
    return {"reference": reference(name), "title": name, "description": "fixture note", "kind": "text"}


frame = read()
connection = frame["connection"]
identity = frame["body"]["data"]["instance"]["identity"]
configuration = frame["body"]["data"]["instance"]["configuration"]
project = Path(frame["body"]["data"]["environment"]["project_root"])
with (project / "provider-starts.log").open("ab") as started:
    started.write(b"start\n")
send(frame["request"], "ready", {"revision": identity["revision"], "artifact": identity["artifact"]})
while (frame := read()) is not None:
    request, body = frame["request"], frame["body"]
    kind, data = body["type"], body.get("data")
    if kind == "release":
        send(request, "released")
        break
    if kind == "operation_settled":
        send(request, "settlement_acknowledged", data)
        continue
    if kind == "cancel":
        send(request, "cancel_acknowledged", {"operation_id": data["operation_id"], "confirmed": False})
        continue
    capability = data["binding"]["capability"]["id"]
    args = data["arguments"]
    if kind == "query" and capability == "fixture.notes.observe":
        mode = args["mode"]
        if mode not in ("cached", "unavailable"):
            content = (project / "external.txt").read_bytes()
            cached_observation = {"data": {"text": content.decode(),
                "digest": hashlib.sha256(content).hexdigest()},
                "observed_at_ms": time.time_ns() // 1_000_000,
                "completeness": "complete", "source": None, "notices": []}
        observation = dict(cached_observation)
        if mode == "cached":
            observation.update(completeness="cached", notices=["Retained bytes; disk has not been read again."])
        elif mode == "unknown_time":
            observation.update(observed_at_ms=None, notices=["The observation time is unknown."])
        elif mode == "partial":
            observation.update(completeness="partial", notices=["Only external.txt was inspected; its producer is unknown."])
        elif mode == "unavailable":
            observation.update(data={}, observed_at_ms=None, completeness="unavailable",
                               notices=["No native observation is available."])
        send(request, "query_result", observation)
    elif kind == "query" and capability == "fixture.notes.search":
        names = [name for name in sorted(notes) if args["text"] in name][: args["limit"]]
        result(request, {"items": [item(name) for name in names], "next": None, "notices": []})
    elif kind == "query" and capability == "fixture.notes.preview":
        selector = args["reference"]["selector"]
        note = notes.get(selector.get("note"))
        if args["reference"]["provider"] != identity or note is None or note["version"] != selector.get("version"):
            send(request, "error", {"code": "context_changed", "message": "Search again before previewing.", "recovery": None})
        else:
            text = note["text"][: args["max_bytes"]]
            result(request, {"item": item(selector["note"]), "text": text, "truncated": False,
                             "data": {"executions": len(executions)}, "resources": []})
    elif kind == "invoke" and capability == "fixture.notes.append":
        name = args["note"]
        note = notes[name]
        if note["version"] != args["expected_version"]:
            send(request, "commit_plan", {"outcome": "failed", "output": None, "error": "note version changed",
                "recovery": None, "facts": [], "evidence": [], "cancellation_confirmed": False})
            continue
        executions.append(data["operation_id"])
        note["text"] += args["text"]
        note["version"] += 1
        uncertain = configuration.get("uncertain_note") == name
        send(request, "commit_plan", {"outcome": "uncertain" if uncertain else "succeeded",
            "output": None if uncertain else {"note": name, "version": note["version"], "executions": len(executions),
                       "scopes": sorted(data["scopes"])},
            "error": "Execution confirmation was lost" if uncertain else None,
            "recovery": {"automatic_reexecution": False} if uncertain else None,
            "facts": [], "evidence": [], "cancellation_confirmed": False})
    else:
        send(request, "error", {"code": "unsupported", "message": "unsupported fixture call", "recovery": None})
