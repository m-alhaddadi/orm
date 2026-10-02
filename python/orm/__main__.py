"""``python -m orm``: migration commands.

    python -m orm makemigrations [name] [--empty] [--check]
    python -m orm sqlmigrate <migration> [--down]
    python -m orm migrate [target]
    python -m orm rollback [--steps N | --to <migration>|zero]
    python -m orm showmigrations

Models and the migrations directory come from ``--models`` / ``--dir`` or from
``[tool.orm]`` in ``pyproject.toml`` (``models = ["blog.models"]``,
``migrations = "migrations"``, ``pythonpath = ["src"]``). The database URL comes from
``--url`` or ``ORM_DATABASE_URL``.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib
import os
import sys
from pathlib import Path
from typing import Any

from .migrations import MigrationError, Migrations, Migrator


def _config() -> dict[str, Any]:
    path = Path("pyproject.toml")
    if not path.is_file():
        return {}
    try:
        import tomllib
    except ImportError:  # Python 3.10
        return {}
    return dict(tomllib.loads(path.read_text()).get("tool", {}).get("orm", {}))


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="python -m orm", description="Schema migrations.")
    p.add_argument("--models", action="append", help="module defining models (repeatable)")
    p.add_argument("--dir", help="migrations directory (default: migrations)")
    p.add_argument("--url", help="database URL (default: $ORM_DATABASE_URL)")
    p.add_argument("--pythonpath", action="append", help="extra import path (repeatable)")
    sub = p.add_subparsers(dest="command", required=True)

    mk = sub.add_parser("makemigrations", help="write the next migration from model changes")
    mk.add_argument("name", nargs="?")
    mk.add_argument("--empty", action="store_true", help="write a migration even without changes")
    mk.add_argument("--check", action="store_true", help="exit 1 if a migration is needed, write nothing")

    sq = sub.add_parser("sqlmigrate", help="print a migration's SQL")
    sq.add_argument("migration")
    sq.add_argument("--down", action="store_true")

    mg = sub.add_parser("migrate", help="apply pending migrations")
    mg.add_argument("target", nargs="?")

    rb = sub.add_parser("rollback", help="revert applied migrations")
    group = rb.add_mutually_exclusive_group()
    group.add_argument("--steps", type=int, default=1)
    group.add_argument("--to", help="revert every migration after this one ('zero' for all)")

    sub.add_parser("showmigrations", help="list migrations and whether they are applied")
    return p


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    cfg = _config()
    for path in [*(cfg.get("pythonpath") or []), *(args.pythonpath or []), "."]:
        if path not in sys.path:
            sys.path.insert(0, path)
    modules = args.models or cfg.get("models") or []
    if isinstance(modules, str):
        modules = [modules]
    if not modules and args.command != "sqlmigrate":
        print("error: no models; pass --models <module> or set [tool.orm] models", file=sys.stderr)
        return 2
    for m in modules:
        importlib.import_module(m)
    migrations = Migrations(args.dir or cfg.get("migrations") or "migrations")
    try:
        if args.command == "makemigrations":
            return _make(migrations, args)
        if args.command == "sqlmigrate":
            m = migrations.get(args.migration)
            print(m.down_sql if args.down else m.up_sql, end="")
            return 0
        return asyncio.run(_db_command(migrations, args))
    except MigrationError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1


def _make(migrations: Migrations, args: argparse.Namespace) -> int:
    if args.check:
        plan = migrations.plan()
        for s in plan.up:
            print(f"  {s.summary}")
        if plan:
            print("models have changes without a migration", file=sys.stderr)
        return 1 if plan else 0
    plan = migrations.plan()
    m = migrations.make(args.name, empty=args.empty)
    if m is None:
        print("No changes.")
        return 0
    print(f"Created {m.path}")
    for s in plan.up:
        print(f"  - {s.summary}")
    for w in plan.warnings:
        print(f"  ! {w}")
    return 0


async def _db_command(migrations: Migrations, args: argparse.Namespace) -> int:
    from .db import connect

    url = args.url or os.environ.get("ORM_DATABASE_URL")
    if not url:
        print("error: no database; pass --url or set ORM_DATABASE_URL", file=sys.stderr)
        return 2
    db = await connect(url, max_connections=1, default=False)
    try:
        migrator = Migrator(db, migrations)
        if args.command == "migrate":
            done = await migrator.upgrade(args.target)
            for m in done:
                print(f"Applied {m}")
            if not done:
                print("Nothing to apply.")
        elif args.command == "rollback":
            done = await migrator.downgrade(args.steps, target=args.to)
            for m in done:
                print(f"Reverted {m}")
            if not done:
                print("Nothing to revert.")
        else:
            for s in await migrator.status():
                mark = "x" if s.applied else " "
                print(f"[{mark}] {s.migration}" + (f"  ({s.applied_at})" if s.applied_at else ""))
    finally:
        await db.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
