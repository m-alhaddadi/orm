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
    with pytest.raises(orm.SchemaError, match="missing.*after extension lowering"):
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
    finally:
        await db.drop_tables()
        await db.close()
