"""Explicit reference loaders run on the instance context and cache per owner."""
import asyncio
import os

import pytest
import orm
from orm.query import QuerySet

SOURCE = '''
model Owner {
  id Int @id
  name String
  required Required[]
  optional Optional[]
  detail Detail?
  role Role?
  @@map("ref04_owners")
}
model Required {
  id Int @id
  owner_id Int
  owner Owner @relation(fields: [owner_id], references: [id])
  @@map("ref04_required")
}
model Optional {
  id Int @id
  owner_id Int?
  owner Owner? @relation(fields: [owner_id], references: [id])
  @@map("ref04_optional")
}
model Detail {
  id Int @id
  owner_id Int @unique
  owner Owner @relation(fields: [owner_id], references: [id])
  @@map("ref04_details")
}
model Role {
  id Int @id
  person Owner @relation(fields: [id], references: [id])
  @@map("ref04_roles")
}
'''


@pytest.fixture(params=["sqlite", "postgres"])
async def references(request):
    dialect = request.param
    registry = orm.Registry()
    models = orm.loads(f'datasource db {{\n provider = "{dialect}"\n}}\n' + SOURCE, registry=registry)
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_REFERENCE_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models
    finally:
        await db.drop_tables()
        await db.close()


async def seed(db, m):
    await m["Owner"].objects.using(db).insert(id=1, name="one")
    await m["Owner"].objects.using(db).insert(id=2, name="two")
    return await m["Required"].objects.using(db).insert(id=1, owner_id=1)


async def test_identity_reload_fk_invalidation_and_counts(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    calls = 0
    original = QuerySet.first

    async def first(qs):
        nonlocal calls
        calls += 1
        return await original(qs)

    monkeypatch.setattr(QuerySet, "first", first)
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    results = await asyncio.gather(*(row.load_owner() for _ in range(12)))
    assert calls == 1
    assert all(value is results[0] for value in results)
    assert row.owner is results[0]
    assert await row.load_owner() is row.owner
    assert calls == 1
    await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="changed")
    fresh = await row.load_owner(reload=True)
    assert fresh is row.owner and fresh is not results[0] and fresh.name == "changed"
    assert calls == 2
    await row.update(owner_id=2)
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await row.load_owner()).id == 2
    assert calls == 3
    await m["Required"].objects.using(db).update(owner_id=1)
    await row.refresh()
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await row.load_owner()).id == 1
    eager = await m["Required"].objects.using(db).select_related(m["Required"].owner).get()
    assert await eager.load_owner() is eager.owner
    assert calls == 4


async def test_cached_absence_refresh_reverse_and_null(references, monkeypatch):
    db, m = references
    await seed(db, m)
    owner = await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).get()
    calls = 0
    original = QuerySet.first

    async def first(qs):
        nonlocal calls
        calls += 1
        return await original(qs)

    monkeypatch.setattr(QuerySet, "first", first)
    assert await owner.load_detail() is None
    assert owner.detail is None
    assert await owner.load_detail() is None and calls == 1
    await m["Detail"].objects.using(db).insert(id=1, owner_id=1)
    assert await owner.load_detail() is None
    await owner.refresh()
    with pytest.raises(orm.NotLoaded):
        _ = owner.detail
    assert (await owner.load_detail()).id == 1 and calls == 2
    nullable = await m["Optional"].objects.using(db).insert(id=1, owner_id=None)
    assert await nullable.load_owner() is None and calls == 2
    assert await nullable.load_owner(reload=True) is None and calls == 2


async def test_required_integrity_optional_missing_and_retry(references):
    db, m = references
    row = await seed(db, m)
    nullable = await m["Optional"].objects.using(db).insert(id=1, owner_id=1)
    # Emulate physically corrupt legacy data without changing server constraints.
    row.__dict__["owner_id"] = 999
    nullable.__dict__["owner_id"] = 999
    with pytest.raises(orm.IntegrityError, match="required target"):
        await row.load_owner()
    assert not row.__dict__["_reference_pending"]
    assert await nullable.load_owner() is None
    assert nullable.owner is None
    assert await nullable.load_owner() is None
    row.__dict__["owner_id"] = 1
    assert (await row.load_owner()).id == 1


async def test_transaction_and_separate_database_context(references):
    db, m = references
    await seed(db, m)
    row = await m["Required"].objects.using(db).get()
    with pytest.raises(RuntimeError, match="rollback"):
        async with db.transaction():
            await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="uncommitted")
            assert (await row.load_owner()).name == "uncommitted"
            raise RuntimeError("rollback")
    assert (await row.load_owner(reload=True)).name == "one"


async def test_cancelled_waiter_failure_and_inflight_refresh(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    gate = asyncio.Event()
    started = asyncio.Event()
    original = QuerySet.first
    calls = 0

    async def slow(qs):
        nonlocal calls
        calls += 1
        value = await original(qs)
        started.set()
        await gate.wait()
        return value

    monkeypatch.setattr(QuerySet, "first", slow)
    one = asyncio.create_task(row.load_owner())
    await started.wait()
    two = asyncio.create_task(row.load_owner())
    await asyncio.sleep(0)
    one.cancel()
    with pytest.raises(asyncio.CancelledError):
        await one
    await row.update(owner_id=2)
    gate.set()
    assert (await two).id == 1
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await row.load_owner()).id == 2
    assert calls == 2

    async def fail(qs):
        raise RuntimeError("temporary failure")

    monkeypatch.setattr(QuerySet, "first", fail)
    with pytest.raises(RuntimeError, match="temporary"):
        await row.load_owner(reload=True)
    monkeypatch.setattr(QuerySet, "first", original)
    assert (await row.load_owner(reload=True)).id == 2


def test_collision_rejected_atomically():
    registry = orm.Registry()
    before = registry.ir()
    with pytest.raises(TypeError, match="collides"):
        orm.loads(SOURCE.replace("owner_id Int\n", "owner_id Int\n  load_owner String\n", 1), registry=registry)
    assert registry.ir() == before


async def test_hidden_dependency_keys_and_missing_key_fail_before_io(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    row.__dict__["_orm_internal"] = {"owner_id": row.__dict__.pop("owner_id")}
    owner = await row.load_owner()
    assert row.owner is owner
    owner.__dict__["_orm_internal"] = {"id": owner.__dict__.pop("id")}
    assert await row.load_owner() is owner
    assert row.owner is owner
    owner.__dict__["_orm_internal"].clear()
    assert await row.load_owner() is owner
    row.__dict__["_orm_internal"].clear()
    async def forbidden(qs):
        raise AssertionError("must fail before I/O")
    monkeypatch.setattr(QuerySet, "first", forbidden)
    with pytest.raises(orm.NotLoaded, match="owner_id"):
        await row.load_owner(reload=True)


async def test_update_with_an_unloaded_reference_key(references):
    db, m = references
    row = await seed(db, m)
    row.__dict__.pop("owner_id")
    await row.update(owner_id=2)
    assert (await row.load_owner()).id == 2


async def test_different_transaction_contexts_do_not_coalesce(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    gate, started = asyncio.Event(), asyncio.Event()
    original = QuerySet.first
    calls = 0
    async def delayed(qs):
        nonlocal calls
        calls += 1
        value = await original(qs)
        if calls == 1:
            started.set()
            await gate.wait()
        return value
    monkeypatch.setattr(QuerySet, "first", delayed)
    outside = asyncio.create_task(row.load_owner())
    await started.wait()
    try:
        async with db.transaction():
            await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="inside")
            inside = await row.load_owner(reload=True)
            assert inside.name == "inside" and calls == 2
            gate.set()
            assert (await outside).name == "one"
            assert row.owner is inside
    finally:
        gate.set()
        await outside


async def test_named_shared_primary_key_references(references):
    db, m = references
    await seed(db, m)
    role = await m["Role"].objects.using(db).insert(id=1)
    person = await role.load_person()
    assert person.pk == role.pk and role.person is person
    reverse = await person.load_role()
    assert reverse.pk == person.pk and person.role is reverse
    assert await reverse.load_person() is reverse.person
