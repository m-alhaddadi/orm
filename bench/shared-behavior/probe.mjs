import assert from 'node:assert/strict';
import { connect } from '../../js/dist/src/index.js';
import '../../js/dist/test/blog/models.js';
const db = await connect(process.env.ORM_TEST_DATABASE_URL, {maxConnections:1});
const expected = JSON.parse(process.argv[3]);
await db.transaction(async () => {
  for (const name of JSON.parse(process.argv[2])) {
    assert.equal(await db.lock(name, {nowait:true}), expected);
  }
});
await db.close();
