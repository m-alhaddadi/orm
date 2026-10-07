/** `@@protected_write` and `allowWrites`: an application-level write check. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { allowWrites, connect, loads, Registry, SchemaError, WriteProtected, type Database } from "../src/index.js";
import { native } from "../src/native.js";

const capabilities = JSON.parse(native().nativeArtifact()).capabilities as string[];
const source = `datasource db {
  provider = "sqlite"
}
model Post {
  id    Int       @id
  title String
  tags  Tag[]     @relation(through: PostTag)
  links PostTag[]
  @@protected_write
}
model Tag {
  id    Int       @id
  name  String
  posts Post[]    @relation(through: PostTag)
  links PostTag[]
}
model PostTag {
  id      Int  @id @default(autoincrement())
  post_id Int
  tag_id  Int
  post    Post @relation(fields: [post_id], references: [id])
  tag     Tag  @relation(fields: [tag_id], references: [id])
  @@unique([post_id, tag_id])
  @@protected_write
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
async function blog(schema = source): Promise<{ models: Record<string, Any>; db: Database }> {
  const registry = new Registry();
  const models = loads(schema, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  return { models, db };
}

test("every write method fails outside allowWrites and works inside", async () => {
  const { models, db } = await blog();
  const Post = models["Post"];
  const posts = () => Post.objects.using(db);
  const writes: (() => Promise<unknown>)[] = [
    () => posts().insert({ id: 1, title: "a" }),
    () => posts().insertMany([{ id: 2, title: "b" }, { id: 3, title: "c" }]),
    () => posts().insert({ id: 1, title: "upsert" }, { onConflict: Post.id, doUpdate: true }),
    () => posts().filter(Post.id.eq(1)).update({ title: "x" }),
    () => posts().updateMany([{ id: 2, title: "y" }]),
    () => posts().filter(Post.id.eq(3)).delete(),
  ];
  try {
    for (const write of writes) {
      await assert.rejects(write(), (e: Error) => e instanceof WriteProtected && /Post is write-protected .*allowWrites\(\[Post\]/.test(e.message));
      await allowWrites([Post], write);
    }
    assert.deepEqual((await posts().orderBy(Post.id).all()).map((p: Any) => [p.id, p.title]), [[1, "x"], [2, "y"]]);
    const post = await posts().get(Post.id.eq(1));
    await assert.rejects(post.update({ title: "z" }), WriteProtected);
    await assert.rejects(post.delete(), WriteProtected);
    await allowWrites([Post], async () => { await post.update({ title: "z" }); await post.delete(); });
    assert.equal(await posts().count(), 1);
  } finally { await db.close(); }
});

test("the check is on the model the SQL writes; scopes nest; work started inside gets the scope", async () => {
  const { models, db } = await blog();
  const { Post, Tag, PostTag } = models;
  try {
    const post: Any = await allowWrites([Post], () => Post.objects.using(db).insert({ id: 1, title: "a" }));
    const tag = await Tag.objects.using(db).insert({ id: 1, name: "t" });
    await assert.rejects(allowWrites([Post], () => post.tags.add(tag)), (e: Error) => e instanceof WriteProtected && /PostTag/.test(e.message));
    await allowWrites([PostTag], () => post.tags.add(tag));
    assert.equal(await PostTag.objects.using(db).count(), 1);
    await allowWrites([Post], () => allowWrites([Tag], () => Post.objects.using(db).filter(Post.id.eq(1)).update({ title: "b" })));
    let started!: Promise<unknown>;
    await allowWrites([Post], async () => { started = Post.objects.using(db).insert({ id: 2, title: "b" }); });
    await started;
    assert.throws(() => allowWrites(["Post"] as never, async () => {}), TypeError);
    // Reads are unchanged, and raw SQL still writes.
    assert.equal(await Post.objects.using(db).count(), 2);
    assert.equal(await db.execute("UPDATE post SET title = 'raw'"), 2);
  } finally { await db.close(); }
});

test("@@protected_write creates no DDL and no snapshot change", () => {
  const schema = (text: string) => {
    const registry = new Registry();
    loads(text, { registry });
    const s = registry.native();
    return [s.ddl(), s.snapshot()];
  };
  assert.deepEqual(schema(source), schema(source.replaceAll("@@protected_write", "")));
  assert.throws(() => loads(source.replace("@@protected_write", "@@protected_write(true)"), { registry: new Registry() }),
    (e: Error) => e instanceof SchemaError && /@@protected_write takes no arguments/.test(e.message));
});

test("a proxy writes the table of its protected root", { skip: !capabilities.includes("proxy-models") }, async () => {
  const { models, db } = await blog(source + "model Draft {\n  @@proxy.of(Post)\n}\n");
  const { Post, Draft } = models;
  try {
    await assert.rejects(Draft.objects.using(db).insert({ id: 1, title: "a" }), (e: Error) => e instanceof WriteProtected && /Post/.test(e.message));
    await allowWrites([Post], () => Draft.objects.using(db).insert({ id: 1, title: "a" }));
    await allowWrites([Draft], () => Draft.objects.using(db).filter(Draft.id.eq(1)).delete());
  } finally { await db.close(); }
});

test("a composed write also writes its protected parent", { skip: !capabilities.includes("model-composition") }, async () => {
  const { models, db } = await blog(`datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id @default(autoincrement())
  name String
  @@protected_write
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}
`);
  const { Person, Employee } = models;
  try {
    await assert.rejects(Employee.objects.using(db).insert({ name: "a", salary: 1 }), (e: Error) => e instanceof WriteProtected && /Person/.test(e.message));
    const alice: Any = await allowWrites([Employee, Person], () => Employee.objects.using(db).insert({ name: "a", salary: 1 }));
    await assert.rejects(Employee.objects.using(db).filter(Employee.id.eq(alice.id)).update({ name: "b" }), WriteProtected);
  } finally { await db.close(); }
});
