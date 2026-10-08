"""Commit callbacks, session advisory locks, read replicas and tenants."""
import asyncio
import time

import pytest

import orm
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
