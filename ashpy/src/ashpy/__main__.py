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

    serve_cmd = sub.add_parser("serve", help="run the gRPC sidecar server")
    serve_cmd.add_argument("--bind", default=DEFAULT_BIND, help=f"bind address (default: {DEFAULT_BIND})")

    args = parser.parse_args(argv)
    if args.version:
        print(f"ashpy {__version__}")
        return 0
    if args.command == "serve":
        return serve(args.bind)
    parser.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())
