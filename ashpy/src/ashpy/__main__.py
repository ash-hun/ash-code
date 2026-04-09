"""ashpy CLI entry point."""

from __future__ import annotations

import argparse
import sys

from . import __version__
from .server import DEFAULT_BIND, serve


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ashpy", description="ash-code Python sidecar")
    parser.add_argument("--version", action="store_true", help="print version and exit")
    sub = parser.add_subparsers(dest="command")

    serve_cmd = sub.add_parser("serve", help="run the gRPC sidecar + FastAPI server")
    serve_cmd.add_argument("--bind", default=DEFAULT_BIND, help=f"gRPC bind (default: {DEFAULT_BIND})")
    serve_cmd.add_argument("--http-host", default="0.0.0.0", help="FastAPI bind host")
    serve_cmd.add_argument("--http-port", type=int, default=8080, help="FastAPI bind port (0 disables)")

    args = parser.parse_args(argv)
    if args.version:
        print(f"ashpy {__version__}")
        return 0
    if args.command == "serve":
        return serve(args.bind, args.http_host, args.http_port)
    parser.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())
