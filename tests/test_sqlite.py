"""SQLite through the same Python API and native engine as PostgreSQL."""
import asyncio
import gc
from datetime import datetime, timezone
from pathlib import Path

import pytest
import orm

SOURCE = Path(__file__).resolve().parents[1].joinpath("examples/sqlite/schema.prisma").read_text()


def models(source=SOURCE):
    registry = orm.Registry()
    return registry, orm.loads(source, registry=registry)


@pytest.fixture
async def sqlite():
    registry, classes = models()
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield db, classes, registry
    await db.drop_tables()
    await db.close()


async def test_crud_relations_defaults_and_upserts(sqlite):
    db, m, registry = sqlite
    Author, Book = m["Author"], m["Book"]
    a = await Author.objects.using(db).insert(email="a@example.com", name="Alice")
    assert a.active and a.created_at.tzinfo == timezone.utc
    b = await Author.objects.using(db).insert(email="b@example.com", name="Bob")
    books = await Book.objects.using(db).insert_many([
        {"author_id": a.id, "title": "one"},
        {"author_id": a.id, "title": "two", "pages": 7, "metadata": {"tags": [1, True]}},
        {"author_id": b.id, "title": "three", "status": m["Status"].published},
    ]).returning()
    assert [x.pages for x in books] == [0, 7, 0]
    assert books[0].status is m["Status"].draft
    assert books[1].metadata == {"tags": [1, True]}
    loaded = await Book.objects.using(db).select_related(Book.author).order_by(Book.id)
    assert [x.author.name for x in loaded] == ["Alice", "Alice", "Bob"]
    loaded = await Author.objects.using(db).prefetch_related(Author.books).order_by(Author.id)
    assert [len(x.books.cached) for x in loaded] == [2, 1]
    assert await Author.objects.using(db).filter(Author.books.pages > 0).count() == 1
    assert await Book.objects.using(db).exists()
    assert await Author.objects.using(db).filter(Author.name.icontains("ali")).count() == 1
    same = await Author.objects.using(db).insert(email=a.email, name="Updated").on_conflict(Author.email, update=True).returning()
    assert same.id == a.id and same.name == "Updated"
    rows = await Book.objects.using(db).filter(Book.pages > 0).update(pages=Book.pages + 1).returning()
    assert rows[0].pages == 8
    with pytest.raises(orm.IntegrityError) as e:
        await Book.objects.using(db).insert(author_id=999, title="orphan")
    assert (e.value.sqlstate, e.value.constraint) == ("23503", None)
    with pytest.raises(orm.IntegrityError) as e:
        await Author.objects.using(db).insert(email=a.email, name="dup")
    assert e.value.sqlstate == "23505"
    with pytest.raises(orm.IntegrityError):
        await Book.objects.using(db).insert(author_id=a.id, title="bad", status="invalid")
    assert await Book.objects.using(db).filter(Book.id == books[0].id).delete() == 1
    assert registry.ir()["dialect"] == "sqlite"


async def test_array_agg_is_postgres_only(sqlite):
    db, m, _ = sqlite
    Author = m["Author"]
    with pytest.raises(orm.QueryError, match="sqlite does not support array_agg"):
        await Author.objects.using(db).select(orm.func.array_agg(Author.name)).scalar()
    # Relation aggregates keep the scalar subqueries: no LATERAL join.
    sql = Author.objects.using(db).select(Author.id, orm.func.count(Author.books), orm.func.max(Author.books.pages)).sql()
    assert "LATERAL" not in sql


async def test_bulk_writes(sqlite):
    db, m, _ = sqlite
    Author = m["Author"]
    with pytest.raises(orm.QueryError, match="needs Postgres"):
        await Author.objects.using(db).insert_many([{"email": "a@x.io", "name": "A"}], copy=True)
    # 32 766 parameters at most: 20 000 rows of two fields go to two statements.
    rows = [{"email": f"u{i}@x.io", "name": f"U{i}"} for i in range(20_000)]
    assert len(await Author.objects.using(db).insert_many(rows).returning()) == 20_000
    first, created = await Author.objects.using(db).get_or_insert(email="u0@x.io", defaults={"name": "x"})
    assert not created and first.name == "U0"


async def test_outer_through_a_relation_path(sqlite):
    db, m, _ = sqlite
    Author, Book = m["Author"], m["Book"]
    alice = await Author.objects.using(db).insert(email="a@example.com", name="Alice")
    bob = await Author.objects.using(db).insert(email="b@example.com", name="Bob")
    await Book.objects.using(db).insert_many([{"author_id": alice.id, "title": "one"}, {"author_id": bob.id, "title": "two"}])
    name = Author.objects.filter(Author.email == orm.outer(Book.author.email)).select(Author.name).as_scalar()
    rows = await Book.objects.using(db).order_by(Book.id).select(Book.title, name.label("name"))
    assert [tuple(r) for r in rows] == [("one", "Alice"), ("two", "Bob")]


async def test_transactions_concurrency_and_abandonment(sqlite):
    db, m, _ = sqlite
    Author = m["Author"]
    async with db.transaction():
        await Author.objects.using(db).insert(email="outer", name="outer")
        with pytest.raises(RuntimeError):
            async with db.transaction():
                await Author.objects.using(db).insert(email="inner", name="inner")
                raise RuntimeError("rollback savepoint")
    assert await Author.objects.using(db).count() == 1
    with pytest.raises(RuntimeError):
        async with db.transaction():
            await Author.objects.using(db).insert(email="rolled", name="rolled")
            raise RuntimeError("rollback")
    tx = await db._engine.begin()
    await db._engine.execute("INSERT INTO author (email, name) VALUES ('abandoned', 'abandoned')", tx)
    del tx
    gc.collect()
    assert await asyncio.wait_for(Author.objects.using(db).count(), 3) == 1
    counts = await asyncio.gather(*(Author.objects.using(db).count() for _ in range(20)))
    assert counts == [1] * 20
    tx = await db._engine.begin()
    await tx.commit()
    with pytest.raises(orm.DatabaseError, match="closed"):
        await db._engine.execute("SELECT 1", tx)


async def test_migrations_rebuild_preserve_rows_and_sequences(tmp_path):
    from orm.migrations import Migrations, Migrator
    reg1, _ = models()
    reg2, _ = models(SOURCE.replace("  title     String", '  name      String @renamed_from("title")').replace("  pages     Int", "  added     Int @default(42)\n  pages     Int").replace("@@index([title]", "@@index([name]"))
    directory = str(tmp_path / "migrations")
    Migrations(directory, reg1).make("initial")
    Migrations(directory, reg2).make("rename")
    db = await orm.connect("sqlite://:memory:", registry=reg1, default=False)
    runner = Migrator(db, Migrations(directory, reg1))
    try:
        await runner.upgrade("1")
        await db.execute("INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author; INSERT INTO author (email, name) VALUES ('kept', 'kept')")
        await db.execute("INSERT INTO book (author_id, title) VALUES (101, 'kept')")
        await runner.upgrade()
        assert (await db._fetch_text("SELECT name, added FROM book"))[0] == ("kept", "42")
        assert (await db._fetch_text("SELECT count(*) FROM author"))[0][0] == "1"
        assert await db._fetch_text("PRAGMA foreign_key_check") == []
        await db.execute("INSERT INTO author (email, name) VALUES ('next', 'next')")
        assert (await db._fetch_text("SELECT max(id) FROM author"))[0][0] == "102"
        await runner.downgrade()
        assert (await db._fetch_text("SELECT title FROM book"))[0][0] == "kept"
        assert all(s.applied for s in (await runner.status())[:1])
    finally:
        await db.close()


async def test_kept_rename_hints_do_not_redirect_later_rebuilds(tmp_path):
    from orm.migrations import Migrations, Migrator
    renamed = SOURCE.replace("  books      Book[]", '  books      Book[]\n  @@map("writer")\n  @@renamed_from("author")').replace("  title     String", '  name      String @renamed_from("title")').replace("@@index([title]", "@@index([name]")
    regs = [models(source)[0] for source in (SOURCE, renamed, renamed.replace("  pages     Int", "  added     Int @default(42)\n  pages     Int"))]
    directory = str(tmp_path / "migrations")
    for reg, name in zip(regs, ("initial", "rename", "later")):
        Migrations(directory, reg).make(name)
    db = await orm.connect("sqlite://:memory:", registry=regs[0], default=False)
    runner = Migrator(db, Migrations(directory, regs[0]))
    try:
        await runner.upgrade("1")
        await db.execute("INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author; INSERT INTO author (email, name) VALUES ('kept', 'kept')")
        await db.execute("INSERT INTO book (author_id, title) VALUES (101, 'kept')")
        await runner.upgrade("2")
        await runner.upgrade()
        assert await db._fetch_text("SELECT w.email, b.name, b.added FROM writer w JOIN book b ON b.author_id = w.id") == [("kept", "kept", "42")]
        assert await db._fetch_text("PRAGMA foreign_key_check") == []
        await db.execute("INSERT INTO writer (email, name) VALUES ('next', 'next')")
        assert (await db._fetch_text("SELECT max(id) FROM writer"))[0][0] == "102"
        await runner.downgrade()
        assert await db._fetch_text("SELECT w.email, b.name FROM writer w JOIN book b ON b.author_id = w.id") == [("kept", "kept")]
    finally:
        await db.close()


async def test_string_functions_and_concatenation():
    from orm import func
    registry, m = models('datasource db { provider = "sqlite" }\nmodel Note {\n  id BigInt @id @default(autoincrement())\n  title String\n  tag String?\n}')
    Note = m["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Note.objects.using(db).insert_many([{"title": "  a-b  ", "tag": "x"}, {"title": "c", "tag": None}])
        rows = await Note.objects.using(db).order_by(Note.id).select(
            func.concat(Note.title, Note.tag).label("c"), Note.title.concat(Note.tag).label("p"), func.trim(Note.title).label("t"),
            func.ltrim(Note.title).label("l"), func.rtrim(Note.title).label("r"), func.replace(Note.title, "-", "+").label("x"),
            func.substr(func.trim(Note.title), 2, 1).label("s"), func.strpos(Note.title, "b").label("i"),
        )
        assert [tuple(r) for r in rows] == [
            ("  a-b  x", "  a-b  x", "a-b", "a-b  ", "  a-b", "  a+b  ", "-", 5),
            ("c", None, "c", "c", "c", "c", "", 0),
        ]
        assert await Note.objects.using(db).filter(Note.title.concat("!") == "c!").count() == 1
    finally:
        await db.close()


async def test_case_expression():
    from orm import func
    registry, m = models('datasource db { provider = "sqlite" }\nmodel Note {\n  id BigInt @id @default(autoincrement())\n  n Int\n}')
    Note = m["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Note.objects.using(db).insert_many([{"n": 1}, {"n": 5}, {"n": 10}])
        size = func.case((Note.n >= 10, "big"), (Note.n >= 5, "mid"), default="small")
        assert await Note.objects.using(db).order_by(Note.id).select(size).scalars() == ["small", "mid", "big"]
        assert await Note.objects.using(db).select(func.sum(func.case((Note.n >= 5, 1), default=0))).scalar() == 2
        await Note.objects.using(db).update(n=func.case((Note.n == 1, 100), default=Note.n))
        assert await Note.objects.using(db).order_by(Note.id).select(Note.n).scalars() == [100, 5, 10]
    finally:
        await db.close()


async def test_aggregate_filter():
    from orm import func
    registry, m = models('datasource db { provider = "sqlite" }\nmodel Note {\n  id BigInt @id @default(autoincrement())\n  n Int\n}')
    Note = m["Note"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Note.objects.using(db).insert_many([{"n": 1}, {"n": 5}, {"n": 10}])
        row = await Note.objects.using(db).select(func.count(filter=Note.n >= 5), func.sum(Note.n, filter=Note.n < 10)).one()
        assert tuple(row) == (2, 6)
    finally:
        await db.close()


async def test_file_database_and_target_mismatch(tmp_path):
    reg, _ = models()
    url = f"sqlite://{tmp_path / 'database.db'}"
    db = await orm.connect(url, registry=reg, default=False)
    await db.create_tables()
    await db.execute("INSERT INTO author (email, name) VALUES ('persisted', 'persisted')")
    await db.close()
    db = await orm.connect(url, registry=reg, default=False)
    assert (await db._fetch_text("SELECT name FROM author"))[0][0] == "persisted"
    await db.close()
    with pytest.raises(orm.SchemaError, match="schema targets postgres, connection uses sqlite"):
        await orm.connect("sqlite://:memory:", registry=orm.Registry(), default=False)


async def test_unsupported_queries(sqlite):
    db, m, _ = sqlite
    Author = m["Author"]
    with pytest.raises(orm.QueryError, match="does not support"):
        await Author.objects.using(db).select(Author.name).distinct(Author.name)
    async with db.transaction():
        with pytest.raises(orm.QueryError, match="does not support"):
            await Author.objects.using(db).lock().all()
        with pytest.raises(orm.QueryError, match="advisory locks"):
            await db.lock("example")


async def test_aggregates_windows_ctes_sliced_prefetch_and_bulk_updates(sqlite):
    from orm import func, Prefetch
    db, m, _ = sqlite
    Author, Book = m["Author"], m["Book"]
    authors = await Author.objects.using(db).insert_many([
        {"email": "a", "name": "A"}, {"email": "b", "name": "B"},
    ]).returning()
    books = await Book.objects.using(db).insert_many([
        {"author_id": a.id, "title": f"{a.name}{pages}", "pages": pages}
        for a in authors for pages in [3, 7, 11]
    ]).returning()
    assert await Book.objects.using(db).select(func.sum(Book.pages)).scalar() == 42
    rank = func.row_number().over(partition_by=Book.author_id, order_by=Book.pages.desc())
    rows = await Book.objects.using(db).select(Book.title, rank.label("rank")).order_by(Book.title)
    assert [tuple(r) for r in rows] == [("A11", 1), ("A3", 3), ("A7", 2), ("B11", 1), ("B3", 3), ("B7", 2)]
    totals = Book.objects.using(db).select(Book.author_id, func.sum(Book.pages).label("pages")).group_by(Book.author_id).cte("totals")
    rows = await Author.objects.using(db).join(totals, totals.c.author_id == Author.id).select(Author.name, totals.c.pages).order_by(Author.id)
    assert [tuple(r) for r in rows] == [("A", 21), ("B", 21)]
    second = Book.objects.order_by(Book.pages.desc())[1:2]
    rows = await Author.objects.using(db).prefetch_related(Prefetch(Author.books, second)).order_by(Author.id)
    assert [[b.title for b in a.books.cached] for a in rows] == [["A7"], ["B7"]]
    rows = await Book.objects.using(db).update_many([{"id": b.id, "pages": b.pages + 1} for b in books]).returning()
    assert sorted(b.pages for b in rows) == [4, 4, 8, 8, 12, 12]
    with pytest.raises(orm.IntegrityError):
        await Book.objects.using(db).update_many([{"id": b.id, "pages": -1 if i == 1 else 99} for i, b in enumerate(books)], batch_size=1)
    assert await Book.objects.using(db).select(func.sum(Book.pages)).scalar() == 48


async def test_uuid_date_timestamp_and_inline_trigger():
    from datetime import date, timedelta
    from uuid import UUID
    source = '''
    datasource db { provider = "sqlite" }
    model Event {
      id String @id @db.Uuid
      day DateTime @db.Date
      at DateTime
      name String
      @@trigger(uppercase, after: [insert], body: "UPDATE event SET name = upper(NEW.name) WHERE id = NEW.id")
    }
    '''
    reg, m = models(source)
    db = await orm.connect("sqlite://:memory:", registry=reg, default=False)
    try:
        await db.create_tables()
        ident = UUID("c689c080-a6d0-4d3e-9305-7fe896158ac9")
        at = datetime(2026, 3, 4, 12, 34, 56, 123456, tzinfo=timezone(timedelta(hours=3, minutes=30)))
        event = await m["Event"].objects.using(db).insert(id=ident, day=date(2026, 3, 4), at=at, name="hello")
        loaded = await m["Event"].objects.using(db).get(m["Event"].id == event.id)
        assert loaded.id == ident and loaded.day == date(2026, 3, 4)
        assert loaded.at == at.astimezone(timezone.utc)
        assert loaded.name == "HELLO"
    finally:
        await db.close()


async def test_finished_nested_transaction_and_cancellation_release_connection(sqlite):
    db, m, _ = sqlite
    tx = await db._engine.begin()
    nested = await db._engine.begin(tx)
    await nested.commit()
    await db._engine.execute("INSERT INTO author (email, name) VALUES ('abandoned', 'abandoned')", tx)
    del tx
    gc.collect()
    # Keep the finished child alive: it must no longer retain its parent's guard.
    assert await asyncio.wait_for(m["Author"].objects.using(db).count(), 3) == 0
    started = asyncio.Event()
    async def work():
        async with db.transaction():
            await m["Author"].objects.using(db).insert(email="cancelled", name="cancelled")
            started.set()
            await asyncio.Event().wait()
    task = asyncio.create_task(work())
    await started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert await asyncio.wait_for(m["Author"].objects.using(db).count(), 3) == 0


@pytest.mark.parametrize("object_sql", [
    "CREATE INDEX custom_index ON author(name)",
    "CREATE TRIGGER custom_trigger AFTER INSERT ON author BEGIN SELECT 1; END",
])
async def test_migration_refuses_unmanaged_objects(tmp_path, object_sql):
    from orm.migrations import Migrations, Migrator
    first, _ = models()
    second, _ = models(SOURCE.replace("  name       String", "  name       String\n  extra      Int @default(1)"))
    directory = str(tmp_path / "migrations")
    Migrations(directory, first).make("initial")
    Migrations(directory, second).make("extra")
    db = await orm.connect("sqlite://:memory:", registry=first, default=False)
    runner = Migrator(db, Migrations(directory, first))
    try:
        await runner.upgrade("1")
        await db.execute("INSERT INTO author (email, name) VALUES ('a', 'A')")
        await db.execute(object_sql)
        with pytest.raises(orm.DatabaseError, match="orm_unmanaged_index_or_trigger"):
            await runner.upgrade()
        assert (await db._fetch_text("SELECT name FROM author"))[0][0] == "A"
        assert [s.applied for s in await runner.status()] == [True, False]
        assert await db._fetch_text("SELECT name FROM sqlite_schema WHERE name LIKE 'custom_%'")
        await db.execute("DROP INDEX custom_index" if "INDEX" in object_sql else "DROP TRIGGER custom_trigger")
        await runner.upgrade()
        await runner.downgrade()
    finally:
        await db.close()


async def test_failed_rebuild_rolls_back_schema_rows_and_history(tmp_path):
    from orm.migrations import Migrations, Migrator
    first, _ = models()
    second, _ = models(SOURCE.replace('  pages     Int    @default(0) @check("pages >= 0")', '  pages     Int    @default(0) @check("pages >= 10")'))
    directory = str(tmp_path / "migrations")
    Migrations(directory, first).make("initial")
    Migrations(directory, second).make("strict")
    db = await orm.connect("sqlite://:memory:", registry=first, default=False)
    runner = Migrator(db, Migrations(directory, first))
    try:
        await runner.upgrade("1")
        await db.execute("INSERT INTO author (email, name) VALUES ('a', 'A'); INSERT INTO book (author_id, title) VALUES (1, 'kept')")
        with pytest.raises(orm.IntegrityError):
            await runner.upgrade()
        assert (await db._fetch_text("SELECT title, pages FROM book"))[0] == ("kept", "0")
        assert [s.applied for s in await runner.status()] == [True, False]
        assert await db._fetch_text("SELECT name FROM sqlite_schema WHERE name LIKE '__orm_%'") == []
        assert (await db._fetch_text("PRAGMA foreign_keys"))[0][0] == "1"
        await db.execute("UPDATE book SET pages=10")
        await runner.upgrade()
        await runner.downgrade()
    finally:
        await db.close()


async def test_cyclic_fks_table_rename_and_empty_sequence(tmp_path):
    from orm.migrations import Migrations, Migrator
    source = '''
    datasource db { provider = "sqlite" }
    model A {
      id BigInt @id @default(autoincrement())
      b_id BigInt?
      b B? @relation("AB", fields: [b_id], references: [id])
      bs B[] @relation("BA")
    }
    model B {
      id BigInt @id @default(autoincrement())
      a_id BigInt?
      a A? @relation("BA", fields: [a_id], references: [id])
      as A[] @relation("AB")
    }
    model Empty {
      id BigInt @id @default(autoincrement())
    }
    '''
    first, _ = models(source)
    second, _ = models(source.replace('      bs B[] @relation("BA")', '      bs B[] @relation("BA")\n      @@map("renamed_a")\n      @@renamed_from("a")'))
    directory = str(tmp_path / "migrations")
    Migrations(directory, first).make("initial")
    Migrations(directory, second).make("rename")
    db = await orm.connect("sqlite://:memory:", registry=first, default=False)
    runner = Migrator(db, Migrations(directory, first))
    try:
        await runner.upgrade("1")
        await db.execute("INSERT INTO a DEFAULT VALUES; INSERT INTO b (a_id) VALUES (1); UPDATE a SET b_id=1; INSERT INTO empty (id) VALUES (200); DELETE FROM empty")
        await runner.upgrade()
        assert (await db._fetch_text("SELECT b_id FROM renamed_a"))[0][0] == "1"
        assert await db._fetch_text("PRAGMA foreign_key_check") == []
        await db.execute("INSERT INTO empty DEFAULT VALUES")
        assert (await db._fetch_text("SELECT id FROM empty"))[0][0] == "201"
        await runner.downgrade()
        assert (await db._fetch_text("SELECT b_id FROM a"))[0][0] == "1"
        assert await db._fetch_text("PRAGMA foreign_key_check") == []
        await db.execute("INSERT INTO empty DEFAULT VALUES")
        assert (await db._fetch_text("SELECT max(id) FROM empty"))[0][0] == "202"
    finally:
        await db.close()


async def test_cross_dialect_snapshots_are_rejected(tmp_path):
    from orm.migrations import Migrations, Migrator
    postgres, _ = models(SOURCE.replace('provider = "sqlite"', 'provider = "postgresql"'))
    sqlite, _ = models()
    directory = str(tmp_path / "migrations")
    initial = Migrations(directory, postgres).make("initial")
    assert '"version": 1' in Path(initial.path, "snapshot.json").read_text()
    with pytest.raises(orm.SchemaError, match="snapshot targets postgres"):
        Migrations(directory, sqlite).make("wrong")
    db = await orm.connect("sqlite://:memory:", registry=sqlite, default=False)
    try:
        with pytest.raises(orm.migrations.MigrationError, match="targets postgres"):
            await Migrator(db, Migrations(directory, sqlite)).upgrade()
    finally:
        await db.close()


FOLLOW = """
datasource db {
  provider = "sqlite"
}

model Person {
  id        BigInt   @id @default(autoincrement())
  name      String
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
  followers Person[] @relation(through: Follow, through_fields: [followee, follower])
}

model Follow {
  id          BigInt @id @default(autoincrement())
  follower_id BigInt
  followee_id BigInt
  follower    Person @relation("follower", fields: [follower_id], references: [id])
  followee    Person @relation("followee", fields: [followee_id], references: [id])

  @@unique([follower_id, followee_id])
}
"""


async def test_self_many_to_many_has_both_sides():
    registry, m = models(FOLLOW)
    Person, Follow = m["Person"], m["Follow"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        people = Person.objects.using(db)
        ann, bob, cat = await people.insert_many([{"name": "ann"}, {"name": "bob"}, {"name": "cat"}]).returning()
        await ann.following.using(db).add(bob, cat)
        await bob.following.using(db).add(cat)
        assert await Follow.objects.using(db).count() == 3
        # each side reads the join rows in its own direction
        assert [p.name for p in await ann.following.using(db).order_by(Person.id)] == ["bob", "cat"]
        assert [p.name for p in await cat.followers.using(db).order_by(Person.id)] == ["ann", "bob"]
        assert await ann.followers.using(db).count() == 0
        # a link removed from one side is gone from the other
        await cat.followers.using(db).remove(ann)
        assert [p.name for p in await ann.following.using(db)] == ["bob"]
        assert [p.name for p in await people.filter(Person.followers.name == "bob")] == ["cat"]
        rows = await people.select(Person.name, orm.func.count(Person.followers)).order_by(Person.id)
        assert [tuple(r) for r in rows] == [("ann", 0), ("bob", 1), ("cat", 1)]
        loaded = await people.prefetch_related(Person.following, Person.followers).order_by(Person.id)
        assert [([f.name for f in p.following.cached], [f.name for f in p.followers.cached]) for p in loaded] == [
            (["bob"], []),
            (["cat"], ["ann"]),
            ([], ["bob"]),
        ]
    finally:
        await db.drop_tables()
        await db.close()
