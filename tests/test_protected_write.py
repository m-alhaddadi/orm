"""`@@protected_write` and `orm.allow_writes`: an application-level write check."""
import asyncio
import json

import pytest
import orm
from orm import _native

CAPABILITIES = json.loads(_native.native_artifact())["capabilities"]

SOURCE = '''datasource db {
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
'''


@pytest.fixture
async def blog():
    registry = orm.Registry()
    models = orm.loads(SOURCE, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield models, db
    await db.close()


async def test_every_write_method_fails_outside_the_scope_and_works_inside(blog):
    models, db = blog
    Post = models["Post"]
    posts = Post.objects.using(db)
    writes = [
        lambda: posts.insert(id=1, title="a"),
        lambda: posts.insert_many([{"id": 2, "title": "b"}, {"id": 3, "title": "c"}]),
        lambda: posts.insert(id=1, title="upsert").on_conflict(Post.id).do_update(),
        lambda: posts.filter(Post.id == 1).update(title="x"),
        lambda: posts.update_many([{"id": 2, "title": "y"}]),
        lambda: posts.filter(Post.id == 3).delete(),
    ]
    for write in writes:
        with pytest.raises(orm.WriteProtected, match=r"Post is write-protected .*orm.allow_writes\(Post\)"):
            await write()
        with orm.allow_writes(Post):
            await write()
    assert [(p.id, p.title) for p in await posts.order_by(Post.id)] == [(1, "x"), (2, "y")]
    post = await posts.get(Post.id == 1)
    with pytest.raises(orm.WriteProtected):
        await post.update(title="z")
    with pytest.raises(orm.WriteProtected):
        await post.delete()
    with orm.allow_writes(Post):
        await post.update(title="z")
        await post.delete()
    assert await posts.count() == 1


async def test_the_check_is_on_the_model_the_sql_writes(blog):
    models, db = blog
    Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
    with orm.allow_writes(Post):
        post = await Post.objects.using(db).insert(id=1, title="a")
    tag = await Tag.objects.using(db).insert(id=1, name="t")  # not protected
    # post.tags.add() writes PostTag rows, so the protection of PostTag decides.
    with orm.allow_writes(Post), pytest.raises(orm.WriteProtected, match="PostTag"):
        await post.tags.add(tag)
    with orm.allow_writes(PostTag):
        await post.tags.add(tag)
    assert await PostTag.objects.using(db).count() == 1
    # Scopes nest: an inner scope adds to the models of the outer one.
    with orm.allow_writes(Post), orm.allow_writes(Tag):
        await Post.objects.using(db).filter(Post.id == 1).update(title="b")
    with pytest.raises(TypeError, match="model class"):
        with orm.allow_writes("Post"):  # type: ignore[arg-type]
            pass


async def test_tasks_started_in_the_scope_get_it_and_reads_and_raw_sql_are_unchanged(blog):
    models, db = blog
    Post = models["Post"]

    async def insert(id):
        return await Post.objects.using(db).insert(id=id, title="a")

    with orm.allow_writes(Post):
        task = asyncio.create_task(insert(1))
    await task
    later = asyncio.create_task(insert(2))
    with pytest.raises(orm.WriteProtected):
        await later
    assert await Post.objects.using(db).count() == 1
    assert await db.execute("UPDATE post SET title = 'raw'") == 1
    assert (await Post.objects.using(db).get()).title == "raw"


def test_protected_write_creates_no_ddl_and_no_snapshot_change():
    def ddl(source):
        registry = orm.Registry()
        orm.loads(source, registry=registry)
        schema = registry.prepare()
        return schema.ddl(), schema.snapshot()

    assert ddl(SOURCE) == ddl(SOURCE.replace("@@protected_write", ""))
    with pytest.raises(orm.SchemaError, match="@@protected_write takes no arguments"):
        orm.loads(SOURCE.replace("@@protected_write", "@@protected_write(true)", 1), registry=orm.Registry())


@pytest.mark.skipif("proxy-models" not in CAPABILITIES, reason="requires a proxy-models artifact")
async def test_a_proxy_writes_the_table_of_its_protected_root():
    source = SOURCE + 'model Draft {\n  @@proxy.of(Post)\n}\n'
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Post, Draft = models["Post"], models["Draft"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(orm.WriteProtected, match="Post"):
            await Draft.objects.using(db).insert(id=1, title="a")
        with orm.allow_writes(Post):
            await Draft.objects.using(db).insert(id=1, title="a")
        with orm.allow_writes(Draft):
            await Draft.objects.using(db).filter(Draft.id == 1).delete()
    finally:
        await db.close()


@pytest.mark.skipif("model-composition" not in CAPABILITIES, reason="requires a model-composition artifact")
async def test_a_composed_write_also_writes_its_protected_parent():
    source = '''datasource db {
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
'''
    registry = orm.Registry()
    # The proxy shares the protection of its root, so it cannot declare its own.
    with pytest.raises(orm.SchemaError, match="Draft: a proxy cannot declare @@protected_write; protect the root model Post"):
        orm.loads(SOURCE.replace("  @@protected_write\n", "", 1) + 'model Draft {\n  @@proxy.of(Post)\n  @@protected_write\n}\n', registry=orm.Registry())
    models = orm.loads(source, registry=registry)
    Person, Employee = models["Person"], models["Employee"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(orm.WriteProtected, match="Person"):
            await Employee.objects.using(db).insert(name="a", salary=1)
        with orm.allow_writes(Employee, Person):
            alice = await Employee.objects.using(db).insert(name="a", salary=1)
        with pytest.raises(orm.WriteProtected, match="Person"):
            await Employee.objects.using(db).filter(Employee.id == alice.id).update(name="b")
    finally:
        await db.close()
