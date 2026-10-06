import os
import asyncio
import importlib.util
import json
from pathlib import Path
import sys

native_path = Path(sys.argv[1]).resolve()
schema_path = Path(sys.argv[2]).resolve()
spec = importlib.util.spec_from_file_location("orm._native", native_path)
module = importlib.util.module_from_spec(spec)
sys.modules["orm._native"] = module
spec.loader.exec_module(module)
import orm

async def main():
    reg = orm.Registry()
    schema = json.loads(schema_path.read_text())
    Policy = orm.define(schema, registry=reg)["Policy"]
    db = await orm.connect(os.environ.get("ORM_TEST_DATABASE_URL", "sqlite://:memory:"), registry=reg, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        await db.execute("INSERT INTO policy (id, active, name, bio) VALUES (1, TRUE, 'Alice', 'large'), (2, TRUE, 'unrequested', 'large')")
        assert [row.name for row in await Policy.objects.using(db).order_by(Policy.id)] == ["Alice", "unrequested"]
        row = await Policy.objects.using(db).only(Policy.display).get(Policy.id == 1)
        assert row.display == "Hello, Alice!"
        assert row.to_dict() == {"display": "Hello, Alice!"}
        assert row.pk == 1
        for name in ("id", "name", "bio"):
            try: getattr(row, name)
            except orm.NotLoaded: pass
            else: raise AssertionError(f"{name} exposed a helper")
        await row.refresh()
        assert row.to_dict() == {"display": "Hello, Alice!"}
        await row.update(name="Bob")
        assert row.to_dict() == {"display": "Hello, Bob!"}
        print("Python compiled-policy/native-computed shape: passed")
    finally:
        await db.drop_tables()
        await db.close()
asyncio.run(main())
