import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, IntegrityError, NotLoaded, type Database } from "../src/index.js";
import { INTERNAL } from "../src/model.js";
import { QuerySet } from "../src/query.js";

const source = `
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
}`;
// Runtime schemas intentionally have no static model types. Generated APIs are checked in Rust/type fixtures.
type Dynamic = Record<string, any>;
for (const dialect of ["sqlite", "postgres"]) {
  test(`${dialect}: explicit loads, identity, reload, absence, integrity, invalidation, context and races`, async () => {
    const registry = new Registry();
    const m = loads(`datasource db {\n provider = "${dialect}"\n}\n${source}`, { registry }) as Dynamic;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_REFERENCE_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    const original = QuerySet.prototype.first;
    let calls = 0;
    await db.dropTables();
    await db.createTables();
    try {
      const Owner = m["Owner"], Required = m["Required"], Optional = m["Optional"], Detail = m["Detail"];
      await Owner.objects.using(db).insert({ id: 1, name: "one" });
      await Owner.objects.using(db).insert({ id: 2, name: "two" });
      const row = await Required.objects.using(db).insert({ id: 1, ownerId: 1 });
      QuerySet.prototype.first = async function (...args) { calls++; return original.apply(this, args); };
      assert.throws(() => row.owner, NotLoaded);
      const values = await Promise.all(Array.from({ length: 12 }, () => row.loadOwner()));
      assert.equal(calls, 1);
      assert.ok(values.every((value) => value === values[0]));
      assert.equal(row.owner, values[0]);
      assert.equal(await row.loadOwner(), row.owner);
      await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "changed" });
      assert.notEqual(await row.loadOwner({ reload: true }), values[0]);
      assert.equal(row.owner.name, "changed");
      await row.update({ ownerId: 2 });
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await row.loadOwner()).id, 2);
      await Required.objects.using(db).update({ ownerId: 1 });
      await row.refresh();
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await row.loadOwner()).id, 1);
      const eager = await Required.objects.using(db).selectRelated(Required.owner).get();
      assert.equal(await eager.loadOwner(), eager.owner);
      assert.equal(calls, 4);
      const owner = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
      assert.equal(await owner.loadDetail(), null);
      assert.equal(await owner.loadDetail(), null);
      assert.equal(calls, 5);
      await Detail.objects.using(db).insert({ id: 1, ownerId: 1 });
      assert.equal(await owner.loadDetail(), null);
      await owner.refresh();
      assert.throws(() => owner.detail, NotLoaded);
      assert.equal((await owner.loadDetail()).id, 1);
      const optional = await Optional.objects.using(db).insert({ id: 1, ownerId: null });
      assert.equal(await optional.loadOwner(), null);
      assert.equal(await optional.loadOwner({ reload: true }), null);
      assert.equal(calls, 6);
      optional.ownerId = 999;
      assert.equal(await optional.loadOwner(), null);
      assert.equal(optional.owner, null);
      assert.equal(await optional.loadOwner(), null);
      row.ownerId = 999;
      await assert.rejects(row.loadOwner({ reload: true }), IntegrityError);
      row.ownerId = 1;
      assert.equal((await row.loadOwner()).id, 1);
      await assert.rejects(db.transaction(async () => {
        await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "uncommitted" });
        assert.equal((await row.loadOwner({ reload: true })).name, "uncommitted");
        throw new Error("rollback");
      }), /rollback/);
      assert.equal((await row.loadOwner({ reload: true })).name, "changed");

      let releaseOutside!: () => void;
      let outsideStarted!: () => void;
      const outsideGate = new Promise<void>((done) => { releaseOutside = done; });
      const outsideReady = new Promise<void>((done) => { outsideStarted = done; });
      let contextQueries = 0;
      QuerySet.prototype.first = async function (...args) {
        contextQueries++;
        const value = await original.apply(this, args);
        if (contextQueries === 1) { outsideStarted(); await outsideGate; }
        return value;
      };
      const outsideLoad = row.loadOwner({ reload: true });
      await outsideReady;
      await db.transaction(async () => {
        await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "context-specific" });
        const inside = await row.loadOwner({ reload: true });
        assert.equal(inside.name, "context-specific");
        assert.equal(contextQueries, 2);
        releaseOutside();
        assert.equal((await outsideLoad).name, "changed");
        assert.equal(row.owner, inside);
      });

      let release!: () => void;
      let start!: () => void;
      const gate = new Promise<void>((done) => { release = done; });
      const started = new Promise<void>((done) => { start = done; });
      QuerySet.prototype.first = async function (...args) {
        const value = await original.apply(this, args);
        start(); await gate;
        return value;
      };
      const inFlight = row.loadOwner({ reload: true });
      await started;
      await row.update({ ownerId: 2 });
      release();
      assert.equal((await inFlight).id, 1);
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await row.loadOwner()).id, 2);
      QuerySet.prototype.first = async function () { throw new Error("temporary"); };
      await assert.rejects(row.loadOwner({ reload: true }), /temporary/);
      QuerySet.prototype.first = original;
      assert.equal((await row.loadOwner({ reload: true })).id, 2);
      // Helper-only source keys remain sufficient for loading without becoming public.
      const hidden = await Required.objects.using(db).get();
      hidden[INTERNAL] = { ownerId: hidden.ownerId };
      delete hidden.ownerId;
      assert.equal((await hidden.loadOwner()).id, 2);
      assert.equal(hidden.owner.id, 2);
      delete hidden[INTERNAL];
      await hidden.update({ ownerId: 1 });
      assert.equal((await hidden.loadOwner()).id, 1);
      const role = await m["Role"].objects.using(db).insert({ id: 1 });
      const person = await role.loadPerson();
      assert.equal(role.person, person);
      assert.equal(person.pk, role.pk);
      const reverse = await person.loadRole();
      assert.equal(person.role, reverse);
      assert.equal(reverse.pk, person.pk);
    } finally {
      QuerySet.prototype.first = original;
      await db.dropTables();
      await db.close();
    }
  });
}

test("loader camelCase collision fails atomically", () => {
  const registry = new Registry();
  assert.throws(() => loads(source.replace("owner_id Int\n", "owner_id Int\n load_owner String\n"), { registry }), /collides/);
  assert.equal([...registry].length, 0);
});
