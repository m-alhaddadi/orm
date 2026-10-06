"""Run against installed wheels in a fresh environment, not the source checkout."""
import asyncio
import json
import os
import orm
from orm import _native
from orm._capabilities import ADAPTERS

metadata = json.loads(_native.profile_metadata())
assert metadata["profile"] == os.environ["ORM_PROFILE"]
assert not ADAPTERS
_selector = os.environ["ORM_PROFILE"]
os.environ["ORM_PROFILE"] = "invalid-after-initialization"
from orm import _native as _again
assert _again is _native
os.environ["ORM_PROFILE"] = _selector
assert hasattr(_native, "cli") == metadata["capabilities"]["cli"]
assert hasattr(_native, "generate_python") == metadata["capabilities"]["generate-python"]
assert hasattr(_native, "generate_typescript") is False  # only Node exposes its generator


def source(backend):
    return f'''datasource db {{ provider = "{backend}" }}
model Probe {{
  id BigInt @id @default(autoincrement())
  name String @unique
}}'''


async def run():
    for backend in ("postgres", "sqlite"):
        provider = "postgresql" if backend == "postgres" else backend
        registry = orm.Registry()
        if backend not in metadata["backends"]:
            try:
                orm.loads(source(provider), registry=registry)
            except (orm.DatabaseError, orm.SchemaError) as error:
                assert "not compiled" in str(error)
            else:
                raise AssertionError("excluded backend accepted at definition")
            continue
        if backend == "postgres" and not os.environ.get("ORM_TEST_DATABASE_URL"):
            continue
        Probe = orm.loads(source(provider), registry=registry)["Probe"]
        url = os.environ["ORM_TEST_DATABASE_URL"] if backend == "postgres" else "sqlite://:memory:"
        db = await orm.connect(url, registry=registry, default=False)
        try:
            await db.create_tables()
            first = await Probe.objects.using(db).insert(name="first")
            assert (await Probe.objects.using(db).get(Probe.id == first.id)).name == "first"
            async with db.transaction():
                await Probe.objects.using(db).insert(name="second")
            assert await Probe.objects.using(db).count() == 2
            await first.update(name="changed")
            assert (await Probe.objects.using(db).get(Probe.id == first.id)).name == "changed"
            await db.drop_tables()
        finally:
            await db.close()

asyncio.run(run())
print(json.dumps(metadata, sort_keys=True))
