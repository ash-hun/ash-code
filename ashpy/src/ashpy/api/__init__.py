"""ashpy FastAPI layer — the public HTTP API (M4).

This package exposes a FastAPI application that runs inside the same
Python process as the gRPC sidecar. It calls back into the Rust
``QueryHost`` gRPC server (:50052) for turn execution, and accesses
the provider / skill / command registries in-process for everything
else.
"""

from .app import create_app

__all__ = ["create_app"]
