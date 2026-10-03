"""Live cross-language lock contention, including release on rollback."""
import asyncio
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'examples'))
from blog.models import User
import orm

async def main():
    db = await orm.connect(os.environ['ORM_TEST_DATABASE_URL'], max_connections=1)
    names = ['', 'import', 'ورود 🔒', 'x'*128, 'x'*129, 'x'*4096]
    async def probe(expected):
        process = await asyncio.create_subprocess_exec(
            'node', str(Path(__file__).with_name('probe.mjs')), json.dumps(names), json.dumps(expected),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        out, err = await process.communicate()
        assert process.returncode == 0, (out, err)
    class Rollback(Exception):
        pass
    try:
        async with db.transaction():
            for name in names:
                assert await db.lock(name)
            await probe(False)
            raise Rollback
    except Rollback:
        pass
    await probe(True)
    await db.close()
    print('Live Python/TypeScript contention and rollback release passed')

asyncio.run(main())
