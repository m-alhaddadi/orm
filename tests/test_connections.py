"""Commit callbacks, session advisory locks, read replicas and tenants."""
import asyncio
import json
import time

import pytest

import orm
from orm import _native
from orm.hooks import prepare_update
from blog.models import User

SQLITE = 'datasource db { provider = "sqlite" }\nmodel Item {\n  id Int @id @default(autoincrement())\n}\n'


async def sqlite_db():
    registry = orm.Registry()
    orm.loads(SQLITE, registry=registry)
    return await orm.connect("sqlite://:memory:", registry=registry, default=False)

async def test_on_commit_runs_after_the_outer_commit(clean):
    db = clean
    other = await orm.connect(db.url, default=False, max_connections=1)
    seen = []

    async def check():
        seen.append(await User.objects.using(other).filter(User.name == "Ann").count())

    try:
        async with db.transaction():
            await User.objects.insert(email="ann@example.com", name="Ann")
            await db.on_commit(check)
            async with db.transaction():
                await db.on_commit(lambda: seen.append("released"))
            with pytest.raises(ZeroDivisionError):
                async with db.transaction():
                    await db.on_commit(lambda: seen.append("rolled back"))
                    1 / 0
            assert seen == []
        assert seen == [1, "released"]
    finally:
        await other.close()


async def test_on_commit_is_dropped_on_rollback_and_runs_at_once_outside(clean):
    db = clean
    seen = []
    with pytest.raises(ZeroDivisionError):
        async with db.transaction():
            await db.on_commit(lambda: seen.append("dropped"))
            1 / 0
    await db.on_commit(lambda: seen.append("now"))
    assert seen == ["now"]
    other = await orm.connect(db.url, default=False, max_connections=1)
    try:
        async with db.transaction():
            # A callback on a database with no open transaction runs at once.
            await other.on_commit(lambda: seen.append("other"))
            assert seen == ["now", "other"]
    finally:
        await other.close()


async def test_on_commit_errors_reach_the_caller_after_the_commit(clean):
    db = clean
    with pytest.raises(RuntimeError, match="boom"):
        async with db.transaction():
            await User.objects.insert(email="b@example.com", name="B")

            def boom():
                raise RuntimeError("boom")

            await db.on_commit(boom)
    assert await User.objects.count() == 1


async def test_on_commit_on_sqlite():
    db = await sqlite_db()
    seen = []
    try:
        async with db.transaction():
            await db.on_commit(lambda: seen.append(1))
            assert seen == []
        assert seen == [1]
    finally:
        await db.close()


async def test_session_lock_outside_a_transaction_with_a_timeout(db):
    other = await orm.connect(db.url, default=False, max_connections=2)
    try:
        async with db.lock("import", session=True, timeout=5):
            assert db._tx() is None
            started = time.monotonic()
            with pytest.raises(orm.LockNotAvailable, match="after 0.2s"):
                async with other.lock("import", session=True, timeout=0.2):
                    pytest.fail("the lock is held")
            assert 0.15 < time.monotonic() - started < 3
            with pytest.raises(orm.LockNotAvailable):
                async with other.lock("import", session=True, nowait=True):
                    pass
            # The transaction-scoped form sees the session lock too.
            async with other.transaction():
                assert not await other.lock("import", nowait=True)
        async with other.lock("import", session=True, nowait=True):
            pass
    finally:
        await other.close()


async def test_session_lock_is_released_on_error_and_shared_locks_share(db):
    other = await orm.connect(db.url, default=False, max_connections=2)
    try:
        with pytest.raises(ZeroDivisionError):
            async with db.lock(7, session=True):
                1 / 0
        async with other.lock(7, session=True, nowait=True):
            pass
        async with db.lock(8, False, session=True):
            async with other.lock(8, exclusive=False, session=True, nowait=True):
                with pytest.raises(orm.LockNotAvailable):
                    async with other.lock(8, session=True, timeout=0):
                        pass
    finally:
        await other.close()


async def test_session_lock_survives_a_cancelled_waiter(db):
    other = await orm.connect(db.url, default=False, max_connections=2)
    try:
        async with db.lock(9, session=True):
            waiter = asyncio.create_task(other.lock(9, session=True).__aenter__())
            await asyncio.sleep(0.2)
            waiter.cancel()
            with pytest.raises(asyncio.CancelledError):
                await waiter
        # The cancelled waiter's connection is closed, so it does not keep the lock.
        await asyncio.sleep(0.2)
        async with db.lock(9, session=True, nowait=True):
            pass
    finally:
        await other.close()


async def test_session_lock_validation():
    db = await sqlite_db()
    try:
        with pytest.raises(orm.QueryError, match="sqlite does not support advisory locks"):
            async with db.lock(1, session=True):
                pass
    finally:
        await db.close()


async def replica_url(db):
    """A second database with the same tables, standing in for a replica."""
    url = db.url.rsplit("/", 1)[0] + "/orm_s6_replica"
    if not await db._fetch_text("SELECT 1 FROM pg_database WHERE datname = 'orm_s6_replica'"):
        await db.execute("CREATE DATABASE orm_s6_replica")
    replica = await orm.connect(url, default=False, max_connections=1)
    try:
        await replica.drop_tables()
        await replica.create_tables()
        await User.objects.using(replica).insert(email="r@example.com", name="Replica")
    finally:
        await replica.close()
    return url


async def test_replicas_answer_reads_outside_a_transaction(clean):
    db = clean
    url = await replica_url(db)
    routed = await orm.connect(db.url, replicas=[url, url], default=False, max_connections=2)
    try:
        await User.objects.using(routed).insert(email="p@example.com", name="Primary")
        users = User.objects.using(routed)
        assert [u.name for u in await users] == ["Replica"]
        assert [u.name for u in await users.filter(User.name == "Replica")] == ["Replica"]
        assert await users.count() == 1
        assert [u.name for u in await users.filter(User.name == orm.param("n")).prepare()(n="Replica")] == ["Replica"]
        assert [u.name for u in await users.using("primary")] == ["Primary"]
        assert [u.name for u in await User.objects.using(routed.primary)] == ["Primary"]
        async with routed.transaction():
            # A fresh query set: an awaited one keeps its rows.
            assert [u.name for u in await User.objects.using(routed)] == ["Primary"]
            # A primary view shares the transaction.
            assert routed.primary._tx() is routed._tx() is not None
        # Writes go to the primary.
        assert await users.filter(User.name == "Primary").update(name="P2") == 1
        assert [u.name for u in await User.objects.using(db)] == ["P2"]
    finally:
        await routed.close()


async def test_using_primary_resolves_the_default_database(clean):
    db = clean
    url = await replica_url(db)
    routed = await orm.connect(db.url, replicas=[url], max_connections=2)
    try:
        assert [u.name for u in await User.objects] == ["Replica"]
        assert [u.name for u in await User.objects.using("primary")] == []
        with pytest.raises(ValueError, match="primary"):
            User.objects.using("replica")  # type: ignore[arg-type]
    finally:
        await routed.close()
        orm.db._default = db


NOTES = """datasource db { provider = "postgresql" }
model Note {
  id     Int    @id @default(autoincrement())
  tenant String
  body   String
  @@map("s6_notes")
}
"""

RLS = """
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'orm_s6_app') THEN
    CREATE ROLE orm_s6_app LOGIN PASSWORD 'app';
  END IF;
END $$;
GRANT SELECT, INSERT, UPDATE, DELETE ON s6_notes TO orm_s6_app;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO orm_s6_app;
ALTER TABLE s6_notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON s6_notes
  USING (tenant = current_setting('app.tenant', true))
  WITH CHECK (tenant = current_setting('app.tenant', true));
"""


async def test_tenant_sets_app_tenant_for_row_level_security(db):
    registry = orm.Registry()
    Note = orm.loads(NOTES, registry=registry)["Note"]
    admin = await orm.connect(db.url, registry=registry, default=False, max_connections=1)
    await admin.drop_tables()
    await admin.create_tables()
    await admin.execute(RLS)
    await Note.objects.using(admin).insert_many([{"tenant": "a", "body": "1"}, {"tenant": "b", "body": "2"}])
    app = await orm.connect("postgres://orm_s6_app:app@" + db.url.split("@", 1)[1], registry=registry, default=False, max_connections=1)
    try:
        assert await Note.objects.using(app).count() == 0  # no tenant: the policy hides every row
        with app.tenant("a"):
            assert [n.body for n in await Note.objects.using(app)] == ["1"]
            async with app.transaction():
                assert await Note.objects.using(app).count() == 1
                await Note.objects.using(app).insert(tenant="a", body="3")
            with pytest.raises(orm.DatabaseError, match="row-level security"):
                await Note.objects.using(app).insert(tenant="b", body="x")
            with app.tenant(7):
                assert await Note.objects.using(app).count() == 0
            async with app.primary.transaction():
                assert await Note.objects.using(app).count() == 2
        assert await Note.objects.using(admin).count() == 3
        # SET LOCAL ends with its transaction: the pooled connection keeps no tenant.
        assert await Note.objects.using(app).count() == 0
    finally:
        await app.close()
        await admin.drop_tables()
        await admin.close()
    sqlite = await sqlite_db()
    try:
        with pytest.raises(orm.QueryError, match="row-level security"):
            with sqlite.tenant("a"):
                pass
    finally:
        await sqlite.close()


QUERY_DEFAULTS = "query-defaults" in json.loads(_native.native_artifact()).get("capabilities", [])


def scoped_schema(dialect):
    def field(name, kind="int", **flags):
        return {"name": name, "column": name, "type": kind, **flags}

    return {
        "dialect": dialect,
        "models": [
            {"name": "Shop", "table": "s6_shops", "fields": [
                field("id", primary_key=True, auto_increment=True), field("name", "string"),
            ], "relations": [{"name": "orders", "kind": "many", "target": "Order", "from": "id", "to": "shop_id"}]},
            {"name": "Order", "table": "s6_orders", "fields": [
                field("id", primary_key=True, auto_increment=True), field("shop_id"), field("total"),
            ], "relations": [{"name": "shop", "kind": "one", "target": "Shop", "from": "shop_id", "to": "id",
                              "foreign_key": True, "on_delete": "cascade"}]},
        ],
        "behavior": {"schema_contract": 1, "query_defaults": [{"model": "Order", "filter": {
            "t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "shop_id"}, "r": {"t": "scope", "name": "shop"}}}]},
    }


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a query-defaults artifact")
@pytest.mark.parametrize("dialect", ["postgres", "sqlite"])
async def test_scope_values_in_default_filters_are_closed_by_default(db, dialect):
    registry = orm.Registry()
    m = orm.define(scoped_schema(dialect), registry=registry)
    Shop, Order = m["Shop"], m["Order"]
    url = "sqlite://:memory:" if dialect == "sqlite" else db.url
    sdb = await orm.connect(url, registry=registry, default=False, max_connections=2)
    await sdb.drop_tables()
    await sdb.create_tables()
    try:
        s1, s2 = await Shop.objects.using(sdb).insert_many([{"name": "one"}, {"name": "two"}])
        orders = Order.objects.using(sdb)
        o1, o2, o3 = await orders.insert_many(
            [{"shop_id": s1.id, "total": 10}, {"shop_id": s1.id, "total": 20}, {"shop_id": s2.id, "total": 30}])
        for read in (lambda: orders.all(), lambda: orders.count(), lambda: orders.filter(Order.total > 0).exists(),
                     lambda: Shop.objects.using(sdb).filter(Shop.orders.total > 25).count()):
            with pytest.raises(orm.QueryError, match=r"default filter of Order reads scope\.shop"):
                await read()
        assert await orders.without_defaults().count() == 3
        with orm.scope(shop=s1.id):
            assert sorted(o.total for o in await orders.all()) == [10, 20]
            assert await orders.count() == 2
            assert [o.total for o in await orders.filter(Order.total > 15).prepare()()] == [20]
            # A relation hop applies the scoped filter too.
            assert await Shop.objects.using(sdb).filter(Shop.orders.total > 25).count() == 0
            shop = await Shop.objects.using(sdb).prefetch_related(Shop.orders).get(Shop.id == s1.id)
            assert len(await shop.orders) == 2
            with orm.scope(shop=s2.id):
                assert [o.total for o in await orders.all()] == [30]
            # Every path that plans a statement reads the scope.
            assert "s6_orders" in orders.sql()
            assert "s6_orders" in orders.filter(Order.total > orm.param("t")).prepare().sql(t=1)
            assert "s6_orders" in orders.select(Order.total).sql()
            assert prepare_update(orders.filter(Order.id == o1.id), {"total": 11}).unique
            with orm.debug.n_plus_one():
                assert (await orders.filter(Order.id == orm.param("i")).prepare().get(i=o1.id)).total == 10
            assert await orders.update(total=0) == 2
            assert await orders.update_many([{"id": o3.id, "total": 99}]) == 0
            assert await orders.filter(Order.id == o3.id).delete() == 0
        assert sorted(o.total for o in await orders.without_defaults()) == [0, 0, 30]
    finally:
        await sdb.drop_tables()
        await sdb.close()
