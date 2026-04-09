"""ashpy gRPC server skeleton.

M0 scaffold: the real gRPC services (LlmProvider / SkillRegistry /
CommandRegistry) are wired up in M1 once the proto file is compiled.
For now this module exposes a ``serve`` entry point that blocks and logs,
so that ``docker compose up`` keeps the sidecar alive while Rust is
built out around it.
"""

from __future__ import annotations

import signal
import sys
import time

DEFAULT_BIND = "127.0.0.1:50051"


def _log(msg: str) -> None:
    print(f"[ashpy] {msg}", flush=True)


def serve(bind: str = DEFAULT_BIND) -> int:
    """Block until SIGTERM/SIGINT. Returns a process exit code."""
    _log(f"scaffold sidecar starting (bind={bind}) — real gRPC wiring lands in M1")
    stop = {"flag": False}

    def _handle(signum, _frame):
        _log(f"received signal {signum}, shutting down")
        stop["flag"] = True

    signal.signal(signal.SIGTERM, _handle)
    signal.signal(signal.SIGINT, _handle)

    try:
        while not stop["flag"]:
            time.sleep(1.0)
    except KeyboardInterrupt:
        pass
    _log("sidecar stopped")
    return 0


if __name__ == "__main__":
    sys.exit(serve())
