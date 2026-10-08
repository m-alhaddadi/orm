"""Bulk writes: insert_many batches, partial-index upserts, get_or_insert, many-to-many
links with extra fields, COPY."""

import pytest
from blog.models import Tag, User

import orm
from conftest import DATABASE_URL

# -- insert_many batches ----------------------------------------------------------------------


async def test_insert_many_splits_by_the_parameter_limit(clean):
    # One field per row: 70 000 rows are 70 000 parameters, more than Postgres's 65 535.
    tags = await Tag.objects.insert_many([{"name": f"t{i}"} for i in range(70_000)])
    assert len(tags) == 70_000
    assert [t.name for t in tags[:2]] == ["t0", "t1"]
    assert tags[-1].name == "t69999"
    assert await Tag.objects.count() == 70_000


async def test_insert_many_batch_size(clean):
    # One statement can't update a row twice; one row per statement can.
    rows = [{"email": "a@x.io", "name": "A1"}, {"email": "a@x.io", "name": "A2"}]
    with pytest.raises(orm.DatabaseError, match="second time"):
        await User.objects.insert_many(rows).on_conflict(User.email).do_update()
    users = await User.objects.insert_many(rows, batch_size=1).on_conflict(User.email).do_update()
    assert [u.name for u in users] == ["A1", "A2"]
    assert (await User.objects.get(User.email == "a@x.io")).name == "A2"


async def test_insert_many_batches_run_in_one_transaction(clean):
    await Tag.objects.insert(name="taken")
    rows = [{"name": f"n{i}"} for i in range(5)] + [{"name": "taken"}]
    with pytest.raises(orm.IntegrityError):
        await Tag.objects.insert_many(rows, batch_size=2)
    assert await Tag.objects.count() == 1


async def test_insert_many_batches_by_max_params(clean):
    db = await orm.connect(DATABASE_URL, max_connections=2, default=False, _disable=("max_params=5",))
    try:
        rows = [{"email": "dup@x.io", "name": f"U{i}"} for i in range(3)]
        # Two parameters per row: two rows per statement, so the duplicate email
        # appears once in each statement.
        rows = [rows[0], {"email": "b@x.io", "name": "B"}, rows[1], {"email": "c@x.io", "name": "C"}]
        users = await User.objects.using(db).insert_many(rows).on_conflict(User.email).do_update()
        assert [u.name for u in users] == ["U0", "B", "U1", "C"]
    finally:
        await db.close()


async def test_insert_many_batch_size_is_checked(clean):
    with pytest.raises(ValueError, match="batch_size"):
        User.objects.insert_many([], batch_size=0)
