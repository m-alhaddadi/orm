"""Public identity/ContentType seam; full generic execution is tested separately."""
import json
import os

import pytest
import orm
from orm.__main__ import main as cli


def generate(path):
    assert cli(["--schema", str(path), "identities"]) == 0


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
        assert cli(["--schema", str(schema), "generate", "python", "-o", str(out)]) == 0
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
        # The migration ledger outlives drop_tables on a shared database.
        await db.execute("DROP TABLE IF EXISTS orm_migrations")
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
        await db.execute("DROP TABLE IF EXISTS orm_migrations")
        await db.close()


async def test_generic_relation_schema_follows_the_artifact_capability(tmp_path):
    from orm import _native

    schema = tmp_path / "schema.prisma"
    schema.write_text('datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\n@@map("generic08_posts")\n}\n'
                      'model Tag {\nid Int @id\ncontent_type ContentType?\nobject_id Int?\n'
                      '@@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post"])\n@@map("generic08_tags")\n}')
    generate(schema)
    if "generic-relations" not in json.loads(_native.native_artifact())["capabilities"]:
        with pytest.raises(orm.SchemaError, match="rebuild"):
            orm.load(schema, registry=orm.Registry())
        return
    registry = orm.Registry()
    models = orm.load(schema, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await models["Post"].objects.using(db).insert(id=7)
        await models["Tag"].objects.using(db).insert(id=1, content_type=models["ContentType"].Post, object_id=7)
        tag = await models["Tag"].objects.using(db).get()
        assert tag.content_type is models["ContentType"].Post and tag.object_id == 7
    finally:
        await db.close()


GENERIC_HEADER = 'datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\ntags Tag[] @generic.reverse\n@@map("generic_field_posts")\n}\nmodel Photo {\nid Int @id\n@@map("generic_field_photos")\n}\n'


async def test_generic_field_form_creates_the_pair_and_the_reverse(tmp_path, capfd):
    from orm import _native

    schema = tmp_path / "schema.prisma"
    schema.write_text(GENERIC_HEADER + 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nnote String\n@@map("generic_field_tags")\n}')
    if "generic-relations" not in json.loads(_native.native_artifact())["capabilities"]:
        assert cli(["--schema", str(schema), "identities"]) != 0
        assert "unknown type Generic; a Generic field needs @generic.relation" in capfd.readouterr().err
        return
    generate(schema)
    registry = orm.Registry()
    models = orm.load(schema, registry=registry)
    tag = next(m for m in registry.ir()["models"] if m["name"] == "Tag")
    assert [(f["name"], f.get("nullable", False), f.get("enum")) for f in tag["fields"]] == [
        ("id", False, None), ("target_type", True, "ContentType"), ("target_id", True, None), ("note", False, None)]
    assert tag["indexes"] == [{"columns": [{"field": "target_type"}, {"field": "target_id"}]}]
    behavior = registry.ir()["behavior"]
    assert behavior["generic_relations"] == [{"model": "Tag", "name": "target", "type_field": "target_type", "key_field": "target_id", "targets": ["Photo", "Post"]}]
    assert behavior["generic_reverse"] == [{"model": "Post", "name": "tags", "source": "Tag", "relation": "target"}]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await models["Tag"].objects.using(db).insert(id=1, target_type=models["ContentType"].Post, target_id=7, note="n")
        tag = await models["Tag"].objects.using(db).get()
        assert tag.target_type is models["ContentType"].Post and tag.target_id == 7
    finally:
        await db.close()


def test_moving_to_the_generic_field_form_needs_no_migration(tmp_path):
    from orm import _native
    from orm.migrations import Migrations

    if "generic-relations" not in json.loads(_native.native_artifact())["capabilities"]:
        pytest.skip("needs an artifact with generic-relations")
    schema = tmp_path / "schema.prisma"
    explicit = ('model Tag {\nid Int @id\ntarget_type ContentType?\ntarget_id Int?\nkind ContentType\nobject_id Int\n'
                '@@generic.relation("target", type: "target_type", key: "target_id", targets: ["Post", "Photo"])\n'
                '@@generic.relation("owner", type: "kind", key: "object_id", targets: ["Post"])\n'
                '@@index([target_type, target_id])\n@@map("generic_field_tags")\n}')
    field = ('model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nkind ContentType\nobject_id Int\n'
             'owner Generic @generic.relation(type: "kind", key: "object_id", targets: ["Post"], index: false)\n@@map("generic_field_tags")\n}')
    directory = str(tmp_path / "migrations")
    for i, model in enumerate([explicit, field]):
        schema.write_text(GENERIC_HEADER.replace("tags Tag[] @generic.reverse", 'tags Tag[] @generic.reverse(relation: "target")') + model)
        generate(schema)
        registry = orm.Registry()
        orm.load(schema, registry=registry)
        made = Migrations(directory, registry).make("initial")
        assert (made is None) == (i == 1)


async def test_a_proxy_that_keeps_a_generic_field_fails_and_one_without_it_loads(tmp_path):
    from orm import _native

    capabilities = json.loads(_native.native_artifact())["capabilities"]
    if "generic-relations" not in capabilities or "proxy-models" not in capabilities:
        pytest.skip("needs an artifact with generic-relations and proxy-models")
    schema = tmp_path / "schema.prisma"
    tag = 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post"])\nnote String\n@@map("generic_field_tags")\n}\n'
    for fields in ['include: ["id", "note"]', 'exclude: ["target"]']:
        schema.write_text(GENERIC_HEADER + tag + f"model NotedTag {{\n@@proxy.of(Tag)\n@@proxy.fields({fields})\n}}")
        generate(schema)
        registry = orm.Registry()
        models = orm.load(schema, registry=registry)
        db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
        try:
            await db.create_tables()
            await models["Tag"].objects.using(db).insert(id=1, target_type=models["ContentType"].Post, target_id=7, note="n")
            assert [(t.id, t.note) for t in await models["NotedTag"].objects.using(db)] == [(1, "n")]
        finally:
            await db.close()
    # The identity manifest of the same models stays; a full proxy keeps the Generic field.
    schema.write_text(GENERIC_HEADER + tag + "model NotedTag {\n@@proxy.of(Tag)\n}")
    with pytest.raises(orm.SchemaError, match="Tag.target: NotedTag keeps the Generic field"):
        orm.load(schema, registry=orm.Registry())
