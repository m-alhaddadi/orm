"""`@@query.defaults` from the schema language to a query; needs a query-defaults artifact."""
import asyncio
import json
import os
from pathlib import Path

import orm
from orm import _native

FIXTURE = Path(__file__).parent.parent.joinpath("fixtures/schema.prisma").read_text()
assert "query-defaults" in json.loads(_native.native_artifact())["capabilities"], "select orm-query-defaults in the build"


async def run(provider: str, url: str) -> None:
    registry = orm.Registry()
    Policy = orm.loads(FIXTURE.replace('"sqlite"', f'"{provider}"'), registry=registry)["Policy"]
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        await Policy.objects.using(db).insert_many([
            {"name": "Alice", "bio": "large"}, {"name": "hidden", "active": False, "bio": "large"}])
        assert await Policy.objects.using(db).count() == 1
        assert await Policy.objects.using(db).without_defaults().count() == 2
        row = await Policy.objects.using(db).get()
        assert row.to_dict() == {"id": row.pk, "name": "Alice"}, row.to_dict()
        try:
            row.bio
        except orm.NotLoaded:
            pass
        else:
            raise AssertionError("a selected-out field was loaded")
        full = await Policy.objects.using(db).without_defaults().get(Policy.name == "hidden")
        assert full.bio == "large" and full.active is False
    finally:
        await db.drop_tables()
        await db.close()


async def main() -> None:
    await run("sqlite", "sqlite://:memory:")
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        await run("postgresql", os.environ["ORM_TEST_DATABASE_URL"])
    print("Python compiled query defaults: passed")


asyncio.run(main())
