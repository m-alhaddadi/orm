"""Public identity/ContentType seam; full generic execution is tested separately."""
import json
import os
import subprocess
from pathlib import Path

import pytest
import orm

ROOT = Path(__file__).resolve().parents[1]


def generate(path):
    subprocess.run([str(ROOT / "target/debug/orm"), "--schema", str(path), "identities"], check=True, capture_output=True)


@pytest.mark.parametrize("url,provider", [
    ("sqlite://:memory:", "sqlite"),
    pytest.param(os.environ.get("ORM_TEST_DATABASE_URL", ""), "postgresql", marks=pytest.mark.skipif(not os.environ.get("ORM_TEST_DATABASE_URL"), reason="set ORM_TEST_DATABASE_URL for PostgreSQL acceptance")),
])
async def test_runtime_and_generated_identities_roundtrip(tmp_path, url, provider):
    schema = tmp_path / "schema.prisma"
    schema.write_text(f'datasource db {{\nprovider = "{provider}"\n}}\nmodel Post {{\nid Int @id\n@@map("identity08_posts")\n}}\nmodel Tag {{\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_tags")\n}}')
    generate(schema)
    frozen = schema.with_suffix(".identities.json").read_bytes()
    registry = orm.Registry()
    models = orm.load(schema, registry=registry)
    assert registry.ir()["identities"] == json.loads(frozen)
    assert models["ContentType"].Post.value == 1
    assert models["ContentType"].Tag.value == 2
    db = await orm.connect(url, registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        post = await models["Post"].objects.using(db).insert(id=7)
        tag = await models["Tag"].objects.using(db).insert(id=1, kind=models["ContentType"].Post, object_id=post.id)
        assert tag.kind is models["ContentType"].Post
        assert tag.object_id == 7
        # Check constraints are the core integer-enum constraint, no registry table.
        with pytest.raises(orm.IntegrityError):
            await db.execute('INSERT INTO identity08_tags (id, kind, object_id) VALUES (2, 99, 7)')
        await post.delete()
        assert (await models["Tag"].objects.using(db).get()).object_id == 7
        # Generated Python embeds the same immutable mapping, runtime needs no file.
        out = tmp_path / "models.py"
        subprocess.run([str(ROOT / "target/debug/orm"), "--schema", str(schema), "generate", "python", "-o", str(out)], check=True, capture_output=True)
        payload = out.read_text().split('_SCHEMA = r"""\n', 1)[1].split('\n"""', 1)[0]
        copied = orm.Registry()
        generated = orm.define(payload, registry=copied)
        assert copied.ir()["identities"] == registry.ir()["identities"]
        assert generated["ContentType"].Post == models["ContentType"].Post
        assert schema.with_suffix(".identities.json").read_bytes() == frozen
    finally:
        await db.drop_tables()
        await db.close()


def test_missing_stale_or_conflicting_manifest_fails_before_definition(tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text('model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\n}')
    with pytest.raises(orm.SchemaError, match="orm identities"):
        orm.load(schema, registry=orm.Registry())
    generate(schema)
    manifest_path = schema.with_suffix(".identities.json")
    manifest = json.loads(manifest_path.read_text())
    manifest["entries"][1]["id"] = manifest["entries"][0]["id"]
    manifest_path.write_text(json.dumps(manifest))
    with pytest.raises(orm.SchemaError, match="duplicate ContentType ID"):
        orm.load(schema, registry=orm.Registry())


@pytest.mark.parametrize("url,provider", [
    ("sqlite://:memory:", "sqlite"),
    pytest.param(os.environ.get("ORM_TEST_DATABASE_URL", ""), "postgresql", marks=pytest.mark.skipif(not os.environ.get("ORM_TEST_DATABASE_URL"), reason="set ORM_TEST_DATABASE_URL for PostgreSQL acceptance")),
])
async def test_retirement_migration_requires_reference_cleanup(tmp_path, url, provider):
    from orm.migrations import Migrations, Migrator

    schema = tmp_path / "schema.prisma"
    header = f'datasource db {{\nprovider = "{provider}"\n}}\n'
    post = 'model Post {\nid Int @id\n@@map("identity08_migration_posts")\n}\n'
    tag = 'model Tag {\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_migration_tags")\n}'
    schema.write_text(header + post + tag)
    generate(schema)
    old = orm.Registry()
    models = orm.load(schema, registry=old)
    directory = str(tmp_path / "migrations")
    Migrations(directory, old).make("initial")
    db = await orm.connect(url, registry=old, default=False)
    runner = Migrator(db, Migrations(directory, old))
    try:
        await db.drop_tables()
        await runner.upgrade("1")
        await models["Tag"].objects.using(db).insert(id=1, kind=models["ContentType"].Post, object_id=9)
        schema.write_text(header + tag)
        generate(schema)
        current = orm.Registry()
        orm.load(schema, registry=current)
        Migrations(directory, current).make("retire_post")
        with pytest.raises(orm.DatabaseError):
            await runner.upgrade()
        assert await models["Tag"].objects.using(db).count() == 1
        await models["Tag"].objects.using(db).delete()
        await runner.upgrade()
        with pytest.raises(orm.IntegrityError):
            await db.execute('INSERT INTO identity08_migration_tags (id, kind, object_id) VALUES (2, 1, 9)')
    finally:
        await db.drop_tables()
        await db.close()
