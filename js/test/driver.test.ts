/** The native Postgres driver: URLs and TLS modes, transaction safety. */

import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { DatabaseError, Registry, connect, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const BASE = DATABASE_URL.split("?", 1)[0]!;
let own: Database;

before(async () => {
  // A connection of its own, without the blog schema.
  own = await connect(DATABASE_URL, { maxConnections: 2, default: false, registry: new Registry() });
  await own.execute("DROP TABLE IF EXISTS driver_probe; CREATE TABLE driver_probe (id int)");
});

after(async () => {
  await own.execute("DROP TABLE driver_probe");
  await own.close();
});

async function probeCount(): Promise<number> {
  return Number((await own.fetchText("SELECT count(*)::text FROM driver_probe"))[0]![0]);
}

async function serverHasTls(): Promise<boolean> {
  return (await own.fetchText("SHOW ssl"))[0]![0] === "on";
}

test("an unknown URL scheme", async () => {
  await assert.rejects(connect("mysql://root@localhost/x", { default: false }), (e: unknown) => e instanceof DatabaseError && /unsupported database URL scheme/.test(e.message));
});

test("an unknown sslmode", async () => {
  await assert.rejects(connect(`${BASE}?sslmode=sometimes`, { default: false }), /unknown sslmode/);
});

test("a bad password fails at connect", async (t) => {
  const url = BASE.replace("postgres:postgres@", "postgres:wrong@");
  if (url === BASE) {
    t.skip("the test URL has no postgres:postgres credentials");
    return;
  }
  await assert.rejects(connect(url, { default: false }), DatabaseError);
});

for (const mode of ["disable", "prefer", "require"]) {
  test(`sslmode=${mode}`, async (t) => {
    if (mode === "require" && !(await serverHasTls())) {
      t.skip("the server has no TLS");
      return;
    }
    const other = await connect(`${BASE}?sslmode=${mode}`, { default: false, registry: new Registry() });
    try {
      const used = await other.fetchText("SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()");
      const tls = await serverHasTls();
      assert.equal(used[0]![0], mode === "disable" || !tls ? "false" : "true");
    } finally {
      await other.close();
    }
  });
}

test("verify-full rejects an untrusted certificate", async (t) => {
  if (!(await serverHasTls())) {
    t.skip("the server has no TLS");
    return;
  }
  // The local test server's certificate is self-signed, not from a public CA.
  await assert.rejects(connect(`${BASE}?sslmode=verify-full`, { default: false }), DatabaseError);
});

test("an abandoned transaction is rolled back", async (t) => {
  const g = globalThis as { gc?: () => void; Bun?: { gc(sync: boolean): void } };
  const gc = g.gc ?? (g.Bun ? () => g.Bun!.gc(true) : undefined);
  if (!gc) {
    t.skip("needs node --expose-gc");
    return;
  }
  await (async () => {
    const tx = await own.engine.begin(null);
    await own.engine.execute("INSERT INTO driver_probe VALUES (1)", tx);
    // neither committed nor rolled back: its connection must not be reused as is
  })();
  for (let i = 0; i < 5; i++) {
    gc();
    await new Promise((r) => setTimeout(r, 20));
  }
  // Every pooled connection (the abandoned one was replaced) sees no row.
  const counts = [];
  for (let i = 0; i < 6; i++) {
    counts.push(await probeCount());
  }
  assert.deepEqual(counts, [0, 0, 0, 0, 0, 0]);
});

test("a finished transaction rejects queries", async () => {
  const { wait } = await import("../src/native.js");
  const tx = await own.engine.begin(null);
  await tx.commit();
  await assert.rejects(wait(() => own.engine.execute("SELECT 1", tx)), (e: unknown) => e instanceof DatabaseError && /already committed/.test(e.message));
  await assert.rejects(wait(() => tx.rollback()), /already committed/);
});
