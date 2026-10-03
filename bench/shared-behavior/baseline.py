"""Original public lock implementation, retained as the benchmark control."""
import hashlib
from orm.errors import QueryError, TransactionRequired

async def lock(self, key: int | str, exclusive: bool=True, *, nowait: bool=False) -> bool:
    """Take an advisory lock on ``key`` until the transaction ends: a lock on a
        name rather than on rows, e.g. "only one worker imports this file at a time".

        ``exclusive=False`` takes a shared lock (any number of shared holders, but no
        exclusive one). Waits for the lock unless ``nowait``, in which case it returns
        ``False`` instead of waiting. A ``str`` key is hashed to a 64-bit one (the first
        8 bytes of its BLAKE2b digest, signed big-endian). Must run inside
        ``db.transaction()``.
        """
    if self.url.startswith('sqlite://'):
        raise QueryError('sqlite does not support advisory locks')
    if self._tx() is None:
        raise TransactionRequired('db.lock() outside a transaction would release the lock at once; run it inside `async with db.transaction():`')
    if isinstance(key, bool) or not isinstance(key, (int, str)):
        raise TypeError(f'lock key must be an int or a str, got {key!r}')
    if isinstance(key, str):
        key = int.from_bytes(hashlib.blake2b(key.encode(), digest_size=8).digest(), 'big', signed=True)
    if not -2 ** 63 <= key < 2 ** 63:
        raise ValueError('lock key must fit in 64 bits')
    fn = 'pg_' + ('try_' if nowait else '') + 'advisory_xact_lock' + ('' if exclusive else '_shared')
    rows = await self._fetch_text(f'SELECT {fn}({int(key)})::text')
    return not nowait or rows[0][0] == 'true'
