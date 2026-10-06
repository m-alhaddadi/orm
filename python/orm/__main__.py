"""``python -m orm``: the ``orm`` command line, the same Rust code as the standalone
``orm`` binary and ``npx orm`` (``cli/``). See ``python -m orm --help``.

    python -m orm check
    python -m orm generate [-o models.py]           # models.py + models.pyi from the schema
    python -m orm makemigrations [name] [--empty] [--check]
    python -m orm sqlmigrate <migration> [--down]
    python -m orm migrate [target]
    python -m orm rollback [--steps N | --to <migration>|zero]
    python -m orm showmigrations

The schema file and migrations directory come from ``--schema`` / ``--dir`` or from
``[tool.orm]`` in ``pyproject.toml`` (``schema = "schema.prisma"``,
``migrations = "migrations"``). The database URL comes from ``--url`` or
``ORM_DATABASE_URL``.
"""

from __future__ import annotations

import sys

from . import _native


def main(argv: list[str] | None = None) -> int:
    """Runs the command line; gives the exit code."""
    sys.stdout.flush()
    sys.stderr.flush()
    if not hasattr(_native, "cli"):
        raise RuntimeError("CLI is unavailable in this runtime profile; install orm[tooling] and set ORM_PROFILE=tooling")
    return _native.cli(sys.argv[1:] if argv is None else list(argv))


if __name__ == "__main__":
    sys.exit(main())
