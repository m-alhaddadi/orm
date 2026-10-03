"""Advisory key compatibility and validation precedence."""
import hashlib

import pytest

import orm


@pytest.mark.parametrize('key', ['', 'import', 'ورود 🔒', 'x' * 128, 'x' * 129, 'x' * 4096])
async def test_string_and_numeric_keys_contend(db, key):
    other = await orm.connect(db.url, default=False, max_connections=1)
    numeric = int.from_bytes(hashlib.blake2b(key.encode(), digest_size=8).digest(), 'big', signed=True)
    try:
        async with db.transaction():
            assert await db.lock(key, exclusive=False)
            async with other.transaction():
                assert await other.lock(numeric, exclusive=False, nowait=True)
                assert not await other.lock(numeric, nowait=True)
        async with other.transaction():
            assert await other.lock(numeric, nowait=True)
    finally:
        await other.close()


async def test_lock_validation(db):
    with pytest.raises(orm.TransactionRequired):
        await db.lock(True)
    async with db.transaction():
        for key in [True, 1.5, None]:
            with pytest.raises(TypeError, match='lock key must be an int or a str'):
                await db.lock(key)
        for key in [-(2**63) - 1, 2**63]:
            with pytest.raises(ValueError, match='lock key must fit in 64 bits'):
                await db.lock(key)
        with pytest.raises(UnicodeEncodeError):
            await db.lock('\ud800')
        for key in [-(2**63), 2**63 - 1]:
            assert await db.lock(key, nowait=True)
