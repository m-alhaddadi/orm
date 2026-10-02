# Python API (Phase 1 prototype)

The query style is SQLAlchemy's: typed expressions over model attributes. The model
layer is Django's: `Model.objects` managers, `select_related` / `prefetch_related`,
`DoesNotExist` per model. Everything is async. Queries compile to the ORM IR in Python
and cross into Rust once per operation. Rust plans the SQL with sea-query and runs it on
SeaORM's connection pool.

```python
yesterday = datetime.now(timezone.utc) - timedelta(days=1)
users = await User.objects.filter(User.posts.created_at < yesterday)
```

`User.posts.created_at` autocompletes and type-checks as `ColumnRef[datetime]`. Through
the `.pyi` stub, `User.posts` is the path class of `Post`. The filter joins
automatically: each relation hop becomes a correlated `EXISTS`.

## Layout

| Path | What |
|---|---|
| `python/orm/` | Python package: `expr.py` (expressions → IR), `fields.py` (descriptors), `model.py`, `query.py` (QuerySet), `db.py` (connections, transactions) |
| `native/` | Rust crate `orm._native` (PyO3): `ir.rs` (schema + query IR), `plan.rs` (IR → SQL), `ddl.rs`, `convert.rs`, `lib.rs` |
| `examples/blog/` | `schema.orm` plus `models.py` / `models.pyi` written by hand in the shape codegen will emit; `demo.py` |
| `tests/` | SQL shape tests (no DB), Postgres end-to-end tests, mypy + pyright stub checks |

```bash
uv venv .venv && . .venv/bin/activate
uv pip install maturin pytest pytest-asyncio mypy pyright
maturin develop            # --release for benchmarks
python -m pytest           # needs Postgres; ORM_TEST_DATABASE_URL overrides the default URL
python examples/blog/demo.py
```

## Models: the generated module

For each schema model, the generator will emit two files:

* **`models.py` (runtime).** Declarative field and relation descriptors, e.g.
  `email = f.String(254, unique=True)`, `posts = f.HasMany("Post", via="author_id")`.
  This is what the IR is built from.
* **`models.pyi` (types).** Per model:
  * the model class (`id: f.BigInt[int]`, `posts: f.HasMany[Post, _PostPath]`, a typed
    `update()`);
  * a **path class** `_PostPath` listing the model's columns as `ColumnRef[T]` and its
    relations as other path classes. This is what makes `User.posts.<TAB>` complete;
  * `PostInsert` / `PostUpdate` **TypedDicts**: the row shapes `insert` and `update`
    accept (required vs optional fields come from the schema);
  * a **query set class** `PostQuerySet` with `insert()`, `insert_many()` and
    `update()` typed by those TypedDicts, since `objects`, `filter()` and the other
    builders return `Self`.

The descriptors behave differently on the class and on an instance:

| | on the class | on an instance |
|---|---|---|
| column `User.email` | `ColumnRef[str]`, for queries | `str`, read-only (plain `__dict__` read, no descriptor call) |
| to-one `Post.author` | `_UserPath` | `User`, if loaded, else `NotLoaded` |
| to-many `User.posts` | `_PostPath` | `RelatedSet[Post]`: a query set over the user's posts |

## Queries

Builders return a new immutable `QuerySet`. Awaiting it runs the query:

```python
await User.objects.filter(cond, cond2)        # list[User]; conditions AND-ed
User.objects.exclude(cond)                    # NOT (...)
User.objects.order_by(User.name, User.id.desc())
User.objects.all()[10:20]                     # LIMIT 10 OFFSET 10
async for u in User.objects.filter(...): ...

await qs.first() / qs.last()                  # by ordering, else pk; None if empty
await qs.get(cond)                            # User.DoesNotExist / MultipleObjectsReturned
await qs.count() / qs.exists()
qs.sql()                                      # SQL with values inlined, for debugging
```

Expressions: `== != < <= > >=` (with `== None` meaning `IS NULL`), `.in_()`,
`.not_in()`, `.is_null()`, `.between()`, `.like()` / `.ilike()` / `.contains()` /
`.icontains()` / `.startswith()` / `.endswith()` (only on string columns; wildcards in
the argument are escaped), `+ - * /`. Combine with `& | ~` or `and_() / or_() / not_()`.
A boolean column is a condition by itself: `filter(Post.published)`. Using Python's
`and` / `or` / `not` on expressions raises `TypeError` instead of silently doing the
wrong thing.

### Relation filters (Django semantics, no duplicate rows)

| Query | Meaning | SQL |
|---|---|---|
| `filter(User.posts.views > 10, User.posts.published)` | some **single** post is both | one `EXISTS (… views > 10 AND published)` |
| `filter(User.posts.views > 10).filter(User.posts.published)` | some post has views > 10 and some (maybe other) post is published | two `EXISTS` |
| `exclude(User.posts.published == False)` | no unpublished post (users without posts included) | `NOT EXISTS` |
| `filter((User.email == x) \| (User.posts.views > 10))` | users without posts still match on email | `email = x OR EXISTS (…)` |
| `filter(User.posts.comments.body.contains("hi"))` | nested hops | nested `EXISTS` |
| `filter(Post.author.email == x)` | to-one hops | `EXISTS` on the PK (planned as a semi-join) |

This matches what Django returns for multi-valued relations: one `filter()` call means
the same related row, separate calls are independent, and negation means "none". It
differs in one way: Django uses a `JOIN` and returns a parent once **per matching
child** unless you add `.distinct()`. `EXISTS` returns each parent once and needs no
`DISTINCT`. The same filter code also works in `UPDATE` and `DELETE`, where Postgres
doesn't allow `JOIN`.

`NOT` doesn't group into a subquery: `filter(User.posts.a & ~User.posts.b)` means "a
post with a, and no post with b", as in Django.

### Loading related objects

Async code can't lazy-load on attribute access, so access is explicit (like SQLAlchemy's
`lazy="raise"`):

```python
cs = await Comment.objects.select_related(Comment.post.author, Comment.author)  # LEFT JOINs
cs[0].post.author.name

us = await User.objects.prefetch_related(User.posts)   # +1 query: WHERE author_id IN (...)
us[0].posts.cached                  # list[Post], no await; post.author is set back to the user
await us[0].posts                   # uses the prefetched rows if present, else queries
await us[0].posts.filter(Post.published).count()
await us[0].posts.insert(title=..., body=...)   # author_id filled in
post.author                         # NotLoaded unless select_related
```

The main query and its prefetch queries run in the same Rust call.

## Writes

There is no `save()`, and no hidden unit of work. Instances are **read-only snapshots**
of rows: `user.name = "x"` raises (and is a type error), and `User(...)` can't be
constructed. Every write is a statement you await, named after its SQL:

```python
# INSERT ... RETURNING: the instance comes back with id, defaults, timestamps
alice = await User.objects.insert(email="a@x.io", name="Alice")
posts = await Post.objects.insert_many([          # one statement for all rows
    {"author": alice, "title": "Hi", "body": "..."},
    {"author_id": alice.id, "title": "Yo", "body": "...", "views": 3},
])
await alice.posts.insert(title="...", body="...")   # FK filled in

# Upsert: INSERT ... ON CONFLICT (email) DO UPDATE / DO NOTHING
alice = await User.objects.insert(email="a@x.io", name="Al").on_conflict(User.email).do_update()
maybe = await User.objects.insert(email="a@x.io", name="Al").on_conflict(User.email).do_nothing()  # None if it existed
await User.objects.insert_many(rows).on_conflict(User.email).do_update(User.name)

# UPDATE / DELETE over a query: set-based, returns the row count
await Post.objects.filter(Post.author.name == "Alice").update(views=Post.views + 1)
await Post.objects.filter(Post.views < 10).delete()

# One row, by primary key
await post.update(title="New", views=Post.views + 1)   # UPDATE ... RETURNING; refreshes `post`
await post.delete()
await post.refresh()
```

* `insert` validates eagerly. Unknown fields, expressions as values and missing
  required fields raise before any SQL runs. Fields left out get the column's database
  default (`DEFAULT` in the VALUES list), and `RETURNING` reads them back.
* `do_update()` with no columns overwrites the fields you passed except the conflict
  columns, so `created_at` isn't reset to `now()`. Pass columns to choose them.
* An insert statement that is never awaited emits a `RuntimeWarning`, the same safety
  net an un-awaited coroutine has.
* `instance.update()` changes exactly the fields named, and the instance then shows
  what the database stored, including expression results and concurrent changes to
  other columns. Instances read through `.using(db)` write back to `db`.

Where the ideas come from:

| | SQLAlchemy 2.0 | Rails (ActiveRecord) | here |
|---|---|---|---|
| insert one | `session.add(User(...)); await session.commit()` | `User.create(email: ...)` | `await User.objects.insert(email=...)` |
| insert many | `await s.execute(insert(User).returning(User), rows)` | `User.insert_all(rows)` | `await User.objects.insert_many(rows)` |
| upsert | `pg_insert(User).on_conflict_do_update(...)` | `User.upsert_all(rows, unique_by: :email)` | `.insert_many(rows).on_conflict(User.email).do_update()` |
| update a set | `update(User).where(...).values(...)` | `User.where(...).update_all(...)` | `await User.objects.filter(...).update(...)` |
| update a row | mutate + flush (unit of work) | `user.update(name: "B")` | `await user.update(name="B")` |
| delete | `delete(User).where(...)` / `session.delete(u)` | `delete_all` / `user.destroy` | `await qs.delete()` / `await user.delete()` |

Statement names and `RETURNING` follow SQLAlchemy Core. Row-level `update(**values)`,
`insert_all` / `upsert_all` with `unique_by`, and choosing the update columns follow
Rails. What's left out on purpose: SQLAlchemy's session and dirty tracking, Rails'
`save` / `attr=` + `save`, and callbacks. A write happens only where the code says
`await ...insert/update/delete`.

### Transactions

```python
async with db.transaction():          # commit on success, rollback on exception
    async with db.transaction(): ...  # nested: savepoint
```

The current transaction lives in a `ContextVar`, so queries inside the block use it
without passing it around. Tasks started inside the block inherit it.

## The FFI boundary

```
QuerySet ──(IR json + params list)──▶ Engine.run ──▶ planner (sea-query) ──▶ SeaORM ──▶ Postgres
         ◀──(list[tuple] + prefetched rows, one pass)──────────────────────────────────┘
```

* The schema IR is sent once, at `connect()`. Query IR is JSON with literals in a
  separate positional `params` list, converted to SQL values by the column type they're
  compared with (`native/src/convert.rs`).
* The IR talks about models, fields and relation paths only. Joins, `EXISTS`, aliases
  and SeaORM types stay in `plan.rs`, so the engine can be swapped later as the plan
  intends.
* Rows come back as tuples in schema field order. Python builds the instances
  (`_from_row`: `__new__` + `__dict__.update`).

Rough numbers (release build, localhost TCP, 1000-row reads): building a query set and its
IR costs ~15 µs of Python. A 1-row read is ~0.1 ms over a bare `SELECT 1` on the same
connection. Reading 1000 posts takes 3.3 ms (Django async was 10.4 ms in Phase 0), and
1000 posts + author via `select_related` take 7.0 ms (Django 18.1 ms). Most of the gap
to the Phase 0 prototype (2.5 / 4.7 ms) is Python-side instance construction, ~0.6 µs
per row, which can move into Rust later.

## Decisions taken in this round (open to change)

1. **Expressions only** for filters, no `field__lookup=` strings (your call). `update()`
   and `insert()` take keyword arguments, typed per model in the stub.
2. **Awaitable query sets** (your call): `await qs`, `await qs.first()`, `async for`.
3. **`EXISTS` for every relation hop in filters**, with Django's grouping rules (see
   above). JOINs only for `select_related` and `order_by`, and only along to-one
   relations. Ordering by a to-many column is rejected.
4. **Explicit both sides of a relation** in the generated code (`BelongsTo` +
   `HasMany`) rather than Django's implicit `related_name`. The schema names both.
5. **Explicit writes, read-only instances** (your call: no `save()`). Defaults live in
   the database (DDL `DEFAULT`) and come back via `RETURNING`. Callable defaults
   declared in Python are evaluated at insert time.
6. **Instance equality** is by model class and primary key (Django).
7. **One default database** set by `connect()`, `.using(db)` to override, like Django's
   `using()`.
8. `db.create_tables()` / `drop_tables()` exist as a development helper until
   migrations do.

## Not done yet

* Schema DSL parser and code generator (`models.py` / `.pyi` are hand-written).
* `values()` / `values_list()`, aggregates beyond `count()`, `annotate`, `distinct`,
  `in_bulk`, `select_for_update`, `update().returning()` for query sets, upsert with
  expression updates (`views = views + EXCLUDED.views`).
* Nested `prefetch_related` paths and `Prefetch(queryset=...)`.
* Building instances in Rust (the remaining per-row cost), caching of compiled plans,
  chunking very large `IN (...)` prefetches.
* `TCP_NODELAY` for sqlx (the Phase 0 finding) is still open.
* `has_one`, many-to-many, composite keys, UUID / JSON / decimal column types.
* A sync API.
