"""Lazy proto code generation for ashpy.

The Python sidecar compiles ``proto/ash.proto`` into ``ashpy._generated``
on first import (or ``ashpy gen-proto``). Generated files are written to
``ashpy/src/ashpy/_generated/`` and are NOT committed — they live next to
the package so ``grpc_tools.protoc`` generates absolute imports that work
without patching.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
from typing import Iterable

PKG_ROOT = pathlib.Path(__file__).resolve().parent
GENERATED_DIR = PKG_ROOT / "_generated"

# The proto file lives outside the Python package, at <repo>/proto/ash.proto.
# Resolution order:
#   1) ASH_PROTO_DIR environment variable (wins if set)
#   2) repo layout: five levels up from this file → <repo>/proto
#   3) Docker image layout: /build/proto (copied by rust-builder stage)
_CANDIDATE_PROTO_DIRS: tuple[pathlib.Path, ...] = (
    pathlib.Path(os.environ.get("ASH_PROTO_DIR", "")) if os.environ.get("ASH_PROTO_DIR") else None,  # type: ignore[arg-type]
    PKG_ROOT.parents[3] / "proto",
    pathlib.Path("/build/proto"),
    pathlib.Path("/opt/ashpy/proto"),
)  # type: ignore[assignment]


def _resolve_proto_dir() -> pathlib.Path:
    for candidate in _CANDIDATE_PROTO_DIRS:
        if candidate is None:
            continue
        proto = candidate / "ash.proto"
        if proto.is_file():
            return candidate
    raise FileNotFoundError(
        "ash.proto not found; set ASH_PROTO_DIR to the directory containing ash.proto"
    )


def _already_generated() -> bool:
    return (GENERATED_DIR / "ash_pb2.py").is_file() and (
        GENERATED_DIR / "ash_pb2_grpc.py"
    ).is_file()


def generate(force: bool = False) -> pathlib.Path:
    """Compile ``ash.proto`` into ``ashpy._generated``. Returns the output dir."""
    if _already_generated() and not force:
        return GENERATED_DIR

    proto_dir = _resolve_proto_dir()
    GENERATED_DIR.mkdir(parents=True, exist_ok=True)
    (GENERATED_DIR / "__init__.py").write_text(
        '"""Auto-generated gRPC stubs. Do not edit."""\n'
    )

    from grpc_tools import protoc  # lazy import — only needed when regenerating

    args: Iterable[str] = (
        "grpc_tools.protoc",
        f"--proto_path={proto_dir}",
        f"--python_out={GENERATED_DIR}",
        f"--grpc_python_out={GENERATED_DIR}",
        str(proto_dir / "ash.proto"),
    )
    rc = protoc.main(list(args))
    if rc != 0:
        raise RuntimeError(f"grpc_tools.protoc failed with exit code {rc}")

    # Rewrite the generated grpc module so that `import ash_pb2` becomes
    # a relative import within the ashpy._generated package.
    grpc_file = GENERATED_DIR / "ash_pb2_grpc.py"
    content = grpc_file.read_text()
    content = content.replace(
        "import ash_pb2 as ash__pb2",
        "from . import ash_pb2 as ash__pb2",
    )
    grpc_file.write_text(content)

    # Make the fresh module importable in an already-running interpreter.
    for mod in ("ashpy._generated.ash_pb2", "ashpy._generated.ash_pb2_grpc"):
        sys.modules.pop(mod, None)

    return GENERATED_DIR


def ensure_generated() -> None:
    """Compile if not already compiled. Cheap no-op on subsequent calls."""
    if not _already_generated():
        generate()


def _module_available(name: str) -> bool:
    return importlib.util.find_spec(name) is not None
