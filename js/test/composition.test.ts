/** Model composition through the public API; needs an artifact with orm-model-composition selected. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, IntegrityError, loads, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const enabled = (JSON.parse(native().nativeArtifact()).capabilities as string[]).includes("model-composition");
const source = `
model Person {
 id Int @id @default(autoincrement())
 name String @check("name <> 'BADROOT'")
 @@map("composition06_node_people")
}
model Employee {
 salary Int @check("salary > 0")
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_node_employees")
}
model Manager {
 level Int @check("level > 0")
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("composition06_node_managers")
}
model Customer {
 points Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "customer")
 @@map("composition06_node_customers")
}
`;
type Row = Record<string, unknown>;
type Objects = {
  insert(values: Row): Promise<Row>;
  attach(parentId: unknown, values: Row): Promise<Row>;
  count(): Promise<number>;
  get(...filters: unknown[]): Promise<Row>;
  filter(...filters: unknown[]): Objects;
  selectRelated(...paths: unknown[]): Objects;
  update(values: Row): Promise<number>;
  delete(): Promise<number>;
};

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];
  test(`composed create, attach, update and delete (${provider})`, { skip: !enabled || !url }, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${source}`, { registry });
    const [Person, Employee, Manager, Customer] = ["Person", "Employee", "Manager", "Customer"].map((name) => models[name]!);
    const field = (model: unknown, name: string) => (model as Record<string, { eq(value: unknown): unknown }>)[name]!;
    const db = await connect(url!, { registry, default: false });
    const objects = (model: unknown) => (model as { objects: { using(db: unknown): Objects } }).objects.using(db);
    const counts = (...list: unknown[]) => Promise.all(list.map((model) => objects(model).count()));
    await db.dropTables();
    await db.createTables();
    try {
      const alice = await objects(Manager).insert({ name: "Alice", salary: 20, level: 1 });
      assert.deepEqual([alice["name"], alice["salary"], alice["level"]], ["Alice", 20, 1]);
      assert.deepEqual(await counts(Person, Employee, Manager), [1, 1, 1]);

      // One person is an Employee and a Customer at the same time.
      const customer = await objects(Customer).attach(alice["id"], { points: 5 });
      assert.deepEqual([customer["id"], customer["name"], customer["points"]], [alice["id"], "Alice", 5]);
      await assert.rejects(objects(Customer).attach(alice["id"], { points: 6 }), IntegrityError);
      await assert.rejects(objects(Customer).attach((alice["id"] as number) + 100, { points: 6 }), IntegrityError);
      assert.equal(await objects(Customer).count(), 1);

      // A failed leaf or root insert rolls back the whole chain.
      await assert.rejects(objects(Manager).insert({ name: "Bob", salary: 10, level: 0 }), IntegrityError);
      await assert.rejects(objects(Manager).insert({ name: "BADROOT", salary: 10, level: 1 }), IntegrityError);
      assert.equal(await objects(Person).count(), 1);

      // An inherited-field filter selects the composed rows to update.
      assert.equal(await objects(Manager).filter(field(Manager, "salary").eq(20)).update({ level: 4 }), 1);
      assert.equal(await objects(Manager).filter(field(Manager, "salary").eq(99)).update({ level: 5 }), 0);
      assert.equal((await objects(Manager).get(field(Manager, "id").eq(alice["id"])))["level"], 4);
      assert.equal((await objects(Employee).filter(field(Employee, "name").eq("Alice")).get())["salary"], 20);

      const person = await objects(Person).selectRelated(field(Person, "employee"), field(Person, "customer")).get();
      assert.deepEqual([(person["employee"] as Row)["salary"], (person["customer"] as Row)["points"]], [20, 5]);

      // Deleting a child keeps its parent and the sibling child.
      await objects(Manager).filter(field(Manager, "id").eq(alice["id"])).delete();
      assert.deepEqual(await counts(Person, Employee, Manager, Customer), [1, 1, 0, 1]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
