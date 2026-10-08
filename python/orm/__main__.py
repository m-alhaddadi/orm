"""``python -m orm``: the ``orm`` command line, the same Rust code as the standalone
``orm`` binary and ``npx orm`` (``cli/``). See ``python -m orm --help``.

    python -m orm check
    python -m orm generate [-o models.py] [--query-set Model=module:Class]  # models.py + .pyi
    python -m orm makemigrations [name] [--empty] [--check]
    python -m orm sqlmigrate <migration> [--down]
    python -m orm migrate [target]                  # runs data.py steps through Migrator
    python -m orm rollback [--steps N | --to <migration>|zero]
    python -m orm showmigrations
    python -m orm pull [-o schema.prisma] [--force]
    python -m orm baseline
    python -m orm drift

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
    args = sys.argv[1:] if argv is None else list(argv)
    found = _native.cli_migrate_args(args)
    if found is not None:
        schema, directory, url, target = found
        from .migrations import has_data_steps, migrate_command

        if url:
            import asyncio

            try:
                # data.py runs in Python, so this migrator applies the directory
                if has_data_steps(directory):
                    return asyncio.run(migrate_command(schema, directory, url, target))
            except Exception as e:  # the CLI reports errors, not tracebacks
                print(f"error: {e}", file=sys.stderr)
                return 1
    return _native.cli(args)


if __name__ == "__main__":
    sys.exit(main())
