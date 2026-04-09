"""M0 smoke tests for ashpy."""

from __future__ import annotations

import ashpy
from ashpy.__main__ import main
from ashpy.server import DEFAULT_BIND


def test_version_is_non_empty() -> None:
    assert ashpy.__version__
    assert isinstance(ashpy.__version__, str)


def test_default_bind_is_loopback() -> None:
    assert DEFAULT_BIND.startswith("127.0.0.1:")


def test_cli_version_flag(capsys) -> None:
    rc = main(["--version"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "ashpy" in out
    assert ashpy.__version__ in out


def test_cli_help_runs(capsys) -> None:
    rc = main([])
    assert rc == 0
    out = capsys.readouterr().out
    assert "ashpy" in out.lower()
