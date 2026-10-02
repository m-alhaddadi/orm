"""``python -m orm``: schema and migration commands.

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
``ORM_DATABASE_URL``. Everything but the database commands is also available without
Python as the ``orm`` binary (``core/``).
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
from pathlib import Path
from typing import Any

from . import _native
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
    p = argparse.ArgumentParser(prog="python -m orm", description="Schema and migrations.")
    p.add_argument("--schema", help="schema file (default: [tool.orm] schema, else schema.prisma)")
    p.add_argument("--dir", help="migrations directory (default: migrations)")
    p.add_argument("--url", help="database URL (default: $ORM_DATABASE_URL)")
    sub = p.add_subparsers(dest="command", required=True)

    sub.add_parser("check", help="compile the schema and report errors")
    gen = sub.add_parser("generate", help="write models.py and models.pyi from the schema")
    gen.add_argument("-o", "--out", help="output module (default: models.py next to the schema)")

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
    schema = Path(args.schema or cfg.get("schema") or "schema.prisma")
    migrations = Migrations(args.dir or cfg.get("migrations") or "migrations", schema)
    try:
        if args.command == "check":
            _native.compile_schema_file(str(schema))
            print(f"{schema}: ok")
            return 0
        if args.command == "generate":
            module, stub = _native.generate_python(str(schema))
            out = Path(args.out) if args.out else schema.with_name("models.py")
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(module)
            out.with_suffix(".pyi").write_text(stub)
            print(f"wrote {out} and {out.with_suffix('.pyi')}")
            return 0
        if args.command == "makemigrations":
            return _make(migrations, args)
        if args.command == "sqlmigrate":
            m = migrations.get(args.migration)
            print(m.down_sql if args.down else m.up_sql, end="")
            return 0
        return asyncio.run(_db_command(migrations, args))
    except (MigrationError, _native.SchemaError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1


def _make(migrations: Migrations, args: argparse.Namespace) -> int:
    if args.check:
        plan = migrations.plan()
        for s in plan.up:
            print(f"  {s.summary}")
        if plan:
            print("the schema has changes without a migration", file=sys.stderr)
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
    from .model import Registry, define

    registry = Registry()
    define(_native.compile_schema_file(str(migrations._schema)), registry=registry)
    db = await connect(url, max_connections=1, default=False, registry=registry)
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
