#!/usr/bin/env python3
"""Evaluation-only public CLI syntax guard; actual clients run in the controller."""
import argparse
import json
from pathlib import Path
import socket
import sys


def request_from_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list")
    cap = sub.add_parser("capability")
    cap.add_argument("--contract", action="store_true")
    for command in ["submit", "lookup", "wait", "read", "cancel"]:
        p = sub.add_parser(command)
        p.add_argument("--id", required=True)
        if command == "submit":
            p.add_argument("--executor", choices=["sh"], required=True)
            p.add_argument("--workdir", choices=["analysis"], required=True)
            p.add_argument("--code-file", choices=["jobs/sum.sh", "jobs/loud.sh", "jobs/hold.sh"], required=True)
        elif command == "lookup":
            p.add_argument("--record", action="store_true")
        elif command == "wait":
            p.add_argument("--timeout", type=float, default=1)
        elif command == "read":
            p.add_argument("--stream", choices=["stdout", "stderr"], default="stdout")
            p.add_argument("--offset", type=int, default=0)
            p.add_argument("--max-bytes", choices=[4096], type=int, default=4096)
    args = parser.parse_args(argv)
    request = {"op": {"list": "list_operations", "read": "read_output"}.get(args.command, args.command)}
    if args.command == "capability" and args.contract:
        request["contract"] = True
    if hasattr(args, "id"):
        request["request_id"] = args.id
    if args.command == "submit":
        request["job"] = Path(args.code_file).stem
    if args.command == "wait":
        if not 0 <= args.timeout <= 5:
            parser.error("evaluation wait timeout must be 0..5 seconds")
        request["timeout_ms"] = int(args.timeout * 1000)
    if args.command == "read":
        if args.offset < 0:
            parser.error("offset must be nonnegative")
        request.update(stream=args.stream, offset=args.offset)
    return request


def main():
    request = request_from_args(sys.argv[1:])
    data = (json.dumps(request) + "\n").encode()
    if len(data) > 32768:
        raise SystemExit("request exceeds 32 KiB")
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(20)
        sock.connect(str(Path(__file__).parent / "bridge.sock"))
        sock.sendall(data)
        raw = sock.makefile("rb").readline(1024 * 1024)
    reply = json.loads(raw)
    if request.get("contract") and reply.get("ok"):
        print(reply["value"], end="")
    else:
        print(json.dumps(reply, ensure_ascii=False))
    raise SystemExit(reply.get("cli_exit", 0 if reply.get("ok") else 1))


if __name__ == "__main__":
    main()
