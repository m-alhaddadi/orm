"""Run with the isolated native artifact containing the selected proxy compiler."""
import json
import os
from pathlib import Path

import pytest
import orm
from orm import _native

SOURCE = Path(__file__).parent.joinpath("fixtures/proxy.prisma").read_text()
pytestmark = pytest.mark.skipif("proxy-models" not in json.loads(_native.native_artifact())["capabilities"], reason="requires selected proxy native artifact")


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_proxy_warning_rows_defaults_writes_and_relation_targets(dialect, capfd):
    source = SOURCE.replace("proxy_05_", "proxy_05_python_")
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    with pytest.raises(orm.SchemaError, match=r":\d+:\d+: relation \w+\.\w+: \w+ has no field missing after extension lowering"):
        orm.loads(source.replace("references: [id]", "references: [missing]"), registry=orm.Registry())
    User, Active, Post, Status = (models[k] for k in ("User", "Active", "Post", "Status"))
    assert not Active._meta.fields["name"].has_server_value
    assert Active._meta.fields["name"].has_insert_default
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.create_tables()
    try:
        await User.objects.using(db).insert_many([
            {"id": 1, "name": None, "status": Status.OLD},
            {"id": 2, "name": None, "status": Status.OLD},
            {"id": 3, "name": "ok", "status": Status.ACTIVE},
        ])
        capfd.readouterr()
        rows = await Active.objects.using(db).order_by(Active.id)
        assert len(rows) == 3 and rows[0].name is None and rows[0].status is Status.OLD
        warnings = [json.loads(line) for line in capfd.readouterr().err.splitlines() if line.startswith('{"code":"orm.proxy.shape"')]
        assert len(warnings) == 2 and all(w["occurrence_count"] == 2 for w in warnings)
        assert all(w["model"] == "Active" for w in warnings)
        assert await Active.objects.using(db).count() == 3
        assert not capfd.readouterr().err
        new = await Active.objects.using(db).insert(id=4)
        assert new.name == "client" and new.status is Status.ACTIVE
        assert new.settings == {"labels": ["proxy", None], "limit": 3}
        assert new.active is True and new.volume == 7
        changed = await Active.objects.using(db).filter(Active.id == 4).update(name=None, status=Status.OLD).returning()
        assert changed[0].name is None and changed[0].status is Status.OLD
        physical = await User.objects.using(db).insert(id=5)
        assert physical.name is None and physical.status is Status.OLD
        await Post.objects.using(db).insert(id=1, user_id=1)
        capfd.readouterr()
        posts = await Post.objects.using(db).select_related(Post.user)
        assert type(posts[0].user) is Active and posts[0].user.status is Status.OLD
        assert len([line for line in capfd.readouterr().err.splitlines() if '"code":"orm.proxy.shape"' in line]) == 2
        await Active.objects.using(db).filter(Active.id == 4).delete()
        assert await User.objects.using(db).filter(User.id == 4).count() == 0
    finally:
        await db.drop_tables()
        await db.close()


DEFAULTS_SOURCE = """
model Member {
  id     Int     @id
  name   String
  status String
  bio    String?
  @@map("proxy_defaults_members")
}
model ActiveMember {
  @@proxy.of(Member)
  @@proxy.default("status", "active")
  @@query.defaults(filter: "status == \\"active\\"", fields: ["id", "name"])
}
"""


@pytest.mark.skipif("query-defaults" not in json.loads(_native.native_artifact())["capabilities"], reason="requires proxy and query-defaults in one artifact")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_proxy_with_query_defaults_is_a_filtered_view(dialect):
    source = DEFAULTS_SOURCE if dialect == "postgres" else 'datasource db { provider = "sqlite" }\n' + DEFAULTS_SOURCE
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Member, Active = models["Member"], models["ActiveMember"]
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.create_tables()
    try:
        await Member.objects.using(db).insert_many([
            {"id": 1, "name": "ann", "status": "active", "bio": "a"},
            {"id": 2, "name": "bob", "status": "old", "bio": "b"},
        ])
        # the proxy policy filters and narrows; the parent model is unchanged
        rows = await Active.objects.using(db).order_by(Active.id)
        assert [type(r) for r in rows] == [Active] and rows[0].to_dict() == {"id": 1, "name": "ann"}
        with pytest.raises(orm.NotLoaded):
            rows[0].bio
        assert await Active.objects.using(db).count() == 1
        assert await Member.objects.using(db).count() == 2
        assert (await Member.objects.using(db).get(Member.id == 2)).bio == "b"
        assert await Active.objects.using(db).without_defaults().count() == 2
        # an insert through the proxy uses its client default and is in its view
        await Active.objects.using(db).insert(id=3, name="cat")
        assert [r.name for r in await Active.objects.using(db).order_by(Active.id)] == ["ann", "cat"]
        # query set writes select through the policy filter
        assert await Active.objects.using(db).update(bio="x") == 2
        assert (await Member.objects.using(db).get(Member.id == 2)).bio == "b"
        assert await Active.objects.using(db).delete() == 2
        assert [m.id for m in await Member.objects.using(db)] == [2]
    finally:
        await db.drop_tables()
        await db.close()
