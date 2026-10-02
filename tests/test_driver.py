"""The native Postgres driver: URLs and TLS modes, transaction safety."""

import pytest
from conftest import DATABASE_URL

import orm

BASE = DATABASE_URL.split("?", 1)[0]


@pytest.fixture
async def own():
    """A connection of its own, without creating the blog schema."""
    try:
        db = await orm.connect(DATABASE_URL, max_connections=2, default=False, registry=orm.Registry())
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    await db.execute("DROP TABLE IF EXISTS driver_probe; CREATE TABLE driver_probe (id int)")
    yield db
    await db.execute("DROP TABLE driver_probe")
    await db.close()


async def probe_count(own_db) -> int:
    return int((await own_db._fetch_text("SELECT count(*)::text FROM driver_probe"))[0][0])


async def test_unknown_url_scheme():
    with pytest.raises(orm.DatabaseError, match="unsupported database URL scheme"):
        await orm.connect("mysql://root@localhost/x", default=False)


async def test_unknown_sslmode(own):
    with pytest.raises(orm.DatabaseError, match="unknown sslmode"):
        await orm.connect(f"{BASE}?sslmode=sometimes", default=False)


async def test_bad_password_fails_at_connect(own):
    url = BASE.replace("postgres:postgres@", "postgres:wrong@", 1)
    if url == BASE:
        pytest.skip("test URL has no postgres:postgres credentials")
    with pytest.raises(orm.DatabaseError):
        await orm.connect(url, default=False)


@pytest.mark.parametrize("mode", ["disable", "prefer", "require"])
async def test_sslmodes(own, mode):
    rows = await own._fetch_text("SHOW ssl")
    if mode == "require" and rows[0][0] != "on":
        pytest.skip("server has no TLS")
    other = await orm.connect(f"{BASE}?sslmode={mode}", default=False)
    try:
        used = await other._fetch_text("SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        assert used[0][0] == ("false" if mode == "disable" else "true")
    finally:
        await other.close()


async def test_verify_full_rejects_untrusted_certificate(own):
    rows = await own._fetch_text("SHOW ssl")
    if rows[0][0] != "on":
        pytest.skip("server has no TLS")
    # The local test server's certificate is self-signed, not from a public CA.
    with pytest.raises(orm.DatabaseError):
        await orm.connect(f"{BASE}?sslmode=verify-full", default=False)


async def test_abandoned_transaction_is_rolled_back(own):
    engine = own._engine
    tx = await engine.begin()
    await engine.execute("INSERT INTO driver_probe VALUES (1)", tx)
    del tx  # neither committed nor rolled back: its connection must not be reused as is
    import gc

    gc.collect()
    # Every pooled connection (the abandoned one was replaced) sees no row.
    assert [await probe_count(own) for _ in range(6)] == [0] * 6


async def test_finished_transaction_rejects_queries(own):
    engine = own._engine
    tx = await engine.begin()
    await tx.commit()
    with pytest.raises(orm.DatabaseError, match="already committed"):
        await engine.execute("SELECT 1", tx)
    with pytest.raises(orm.DatabaseError, match="already committed"):
        await tx.rollback()
