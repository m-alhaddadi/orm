# Python API (Phase 1 prototype)

The query style is SQLAlchemy's: typed expressions over model attributes. The model
layer is Django's: `Model.objects` managers, `select_related` / `prefetch_related`,
`DoesNotExist` per model. Everything is async. Queries compile to the ORM IR in Python
and cross into Rust once per operation. Rust plans the SQL with sea-query and runs it
with its own Postgres driver (tokio-postgres; see [Drivers and dialects](#drivers-and-dialects)).

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
| `python/orm/` | Python package: `expr.py` (expressions, `func`, `outer` / `exists`, windows → IR), `fields.py` (descriptors), `model.py`, `query.py` (QuerySet, `Prefetch`), `select.py` (`select()`, `Row`), `cte.py` (CTEs), `write.py` (insert / update / delete statements), `db.py` (connections, transactions), `schema.py` / `migrations.py` / `ext/` (schema objects, migrations, extensions: [`schema.md`](schema.md)) |
| `core/` | Rust crate `orm-core`, no binding code: schema language, IR (`ir.rs`), extensions, migrations, code generation, the `orm` CLI (see [`schema.md`](schema.md)) |
| `native/` | Rust crate `orm._native` (PyO3) on top of `orm-core`: `plan.rs` (IR → sea-query statements), `db/` (drivers: `mod.rs` traits, `postgres.rs`), `build.rs` (rows → instances and `Row`s), `convert.rs` (Python ↔ values), `lib.rs` |
| `examples/blog/` | `schema.prisma`, the `models.py` / `models.pyi` generated from it, its migrations, `demo.py` |
| `tests/` | SQL shape tests (no DB), Postgres end-to-end tests, mypy + pyright stub checks |

Python 3.11 or newer (`select()` rows are typed with `TypeVarTuple`).

```bash
uv venv .venv && . .venv/bin/activate
uv pip install maturin pytest pytest-asyncio mypy pyright
maturin develop            # --release for benchmarks
python -m pytest           # needs Postgres; ORM_TEST_DATABASE_URL overrides the default URL
python examples/blog/demo.py
```

## Models: the generated module

`python -m orm generate` turns `schema.prisma` into two files (see [`schema.md`](schema.md)):

* **`models.py` (runtime).** The compiled schema IR plus `orm.define()`, which builds
  the model classes (field and relation descriptors) from it. `orm.load("schema.prisma")`
  does the same at runtime without a generated file.
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
await qs.in_bulk([1, 2, 3])                   # {1: <User 1>, 3: <User 3>}: by pk, missing ids left out
await qs.in_bulk(emails, field=User.email)    # by a unique field; no ids: every row
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
post.author                         # NotLoaded unless select_related or prefetched

await User.objects.prefetch_related(User.posts.comments)   # +2 queries: posts, then their comments
await Comment.objects.prefetch_related(Comment.post)       # to-one too: comment.post
await User.objects.prefetch_related(
    Prefetch(User.posts, Post.objects.filter(Post.published).order_by(Post.views.desc())[:3]),
    Prefetch(User.comments, Comment.objects.select_related(Comment.post), to_attr="recent"),
)
```

The main query and its prefetch queries run in the same Rust call, one query per
relation level (keys deduplicated, `NULL` keys skipped). Each key is a bound parameter,
so more keys than a statement takes (65 535 on Postgres, the dialect's `max_params`)
split into several queries; the split is between parents, so ordering and slices per
parent are unaffected. (Django doesn't split: on Postgres it inlines the values into the
SQL text client-side, so the limit doesn't apply to it.)

* A path loads every relation along it: `User.posts.comments` fills `user.posts` and
  each post's `comments`. Paths with a common prefix share its query.
* `Prefetch(path, queryset, to_attr=None)` gives the related rows their own query set:
  filters, ordering, `select_related` and further `prefetch_related` (nested under it).
  As in Django, the rows fill the relation itself, so `user.posts.cached` and `await
  user.posts` then give the filtered rows; `user.posts.filter(...)` and other new
  queries still see every post. With `to_attr` they go to a plain list attribute
  (`user.recent`) and the relation stays unloaded. Two different query sets for the same
  attribute raise `ValueError`.
* A **slice applies per parent**: `[:3]` above is each user's three most viewed posts,
  planned as `ROW_NUMBER() OVER (PARTITION BY author_id ORDER BY views DESC, id)` in a
  subquery and `WHERE _rn <= 3` around it (Django 4.2 does the same).

### Big tables: batches

```python
async for post in Post.objects.filter(Post.published).iterate(batch_size=1000):
    ...
async for batch in Post.objects.batches(500):      # list[Post] per batch
    await search_index.add(batch)
```

Each batch is `WHERE <filters> AND id > <last id> ORDER BY id LIMIT n` (keyset paging, no
`OFFSET`), so memory stays flat and every batch is an index range scan. `select_related`,
`prefetch_related` and `lock()` apply per batch; `order_by` and slicing are rejected.

## Columns and aggregates: `select()`

`select()` is the one way to read anything other than whole instances. It replaces
Django's `values()`, `values_list()`, `aggregate()` and `annotate()`:

```python
rows = await (
    Post.objects.filter(Post.published)
    .select(Post.author_id, func.count().label("posts"), func.sum(Post.views).label("views"))
    .group_by(Post.author_id)
    .having(func.count() > 2)
    .order_by(func.count().desc())
)
rows[0].posts; rows[0][1]; author_id, posts, views = rows[0]    # Row: a tuple with names

await Post.objects.select(func.max(Post.views)).scalar()         # int | None
await Post.objects.filter(...).select(Post.id).scalars()          # list[int]
await qs.select(...).first() / .one()                             # one Row
await Post.objects.select(Post.title, Post.author.name.label("author"))   # to-one: LEFT JOIN
await Post.objects.select(Post.author_id).distinct()
await Post.objects.select(Post.title).distinct(Post.author_id).order_by(Post.author_id, Post.views.desc())

# The model plus computed values (Django's annotate):
for user, n_posts, views in await User.objects.select(User, func.count(User.posts), func.sum(User.posts.views)):
    ...
# Aggregates over relations also filter:
await User.objects.filter(func.count(User.posts) > 2)
# Subqueries:
await Post.objects.filter(Post.author_id.in_(User.objects.filter(...).select(User.id)))
```

| Django | here |
|---|---|
| `values("id", "title")` / `values_list(...)` | `select(Post.id, Post.title)` |
| `values_list("id", flat=True)` | `select(Post.id).scalars()` |
| `aggregate(total=Sum("views"))` | `select(func.sum(Post.views)).scalar()` |
| `values("author").annotate(n=Count("id"))` | `select(Post.author_id, func.count()).group_by(Post.author_id)` |
| `annotate(n=Count("posts"))` → `user.n` | `select(User, func.count(User.posts))` → `(user, n)` rows |

* **Rows** are `Row`s: tuple subclasses (index, unpack, compare with tuples) whose items
  also have names: the field name for columns, `.label()` for expressions, the function
  name for functions (`count`, `sum`), the lower-case model name for a model. Two columns
  with the same name must be labelled. Statically a row is `Row[int, int, int | None]`
  (from overloads of `select()`), so unpacking and indexing are typed; names are `Any`.
  Selecting fewer columns is about twice as fast as building instances (1000 rows: 1.2 ms
  vs 2.3 ms here).
* **Aggregates** are `func.count` / `sum` / `avg` / `min` / `max`; scalar functions are
  `lower`, `upper`, `length`, `abs`, `coalesce`, `now`. Over the model's own columns an
  aggregate summarizes the rows (of each `group_by()` group). Over a relation path it is
  computed **per row in a correlated subquery**: `func.count(User.posts)` is `(SELECT
  COUNT(*) FROM posts WHERE posts.author_id = users.id)`. So two aggregates over different
  relations never multiply each other, unlike Django's JOIN-based `annotate(Count(...),
  Count(...))`, and they work in `filter()` too.
* Integer `SUM`s come back as `int` (cast to `bigint`), `AVG` as `float`.
* Columns through to-one relations are `LEFT JOIN`ed; a to-many column outside an
  aggregate is rejected (it would repeat rows).
* `lock()` works with plain column selects, not with aggregates, window functions,
  `group_by` or `distinct`. `select_related` / `prefetch_related` don't combine with
  `select()`.
* A condition is a boolean column once labelled: `select(User.name, (User.id > 3).label("big"))`.

## Subqueries: `exists()`, scalar values, `outer()`

Relation paths cover subqueries along declared relations. For anything else a query
set or `select()` goes inside another query, and `outer(col)` (Django's `OuterRef`)
names a column of the enclosing query:

```python
from orm import exists, outer

# EXISTS as a condition (negate with ~), or a boolean column
await User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id), Post.views > 100)))
await User.objects.filter(~exists(Comment.objects.filter(Comment.author_id == outer(User.id))))

# A one-column, at-most-one-row query as a value: (SELECT ...)
latest = (Post.objects.filter(Post.author_id == outer(User.id))
          .order_by(Post.created_at.desc()).select(Post.title)[:1].as_scalar())
await User.objects.select(User, latest.label("latest"))              # Row[User, str]
avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
await Post.objects.filter(Post.views > avg)                           # above their author's average
await Post.objects.update(views=Comment.objects.filter(Comment.post_id == outer(Post.id)).select(func.count()).as_scalar())
```

* `outer(Model.col)` refers to the nearest enclosing query over `Model` (a subquery
  of a subquery can reach two levels up). Using `User.id` directly inside a `Post`
  subquery raises a `ValueError` that suggests `outer(User.id)`.
* A scalar subquery returning more than one row is a database error, as in SQL:
  slice it (`[:1]`) or aggregate. No row gives `NULL` (`None`).
* `in_()` subqueries take `outer()` too. A subquery over the same table as its outer
  query gets an alias (`FROM posts AS s1`), so both stay addressable.
* `qs.exists()` (awaited, a `bool`) stays the way to ask about one query set.

## Window functions

```python
rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
await Post.objects.select(Post.title, rank.label("rank"))
await Post.objects.select(Post.title, func.sum(Post.views).over(order_by=Post.created_at, rows=(None, 0)))  # running total
await Post.objects.select(Post.title, func.lag(Post.views, 1, 0).over(order_by=Post.created_at))
```

* `.over(partition_by=..., order_by=..., rows=(start, end) | range=(start, end))` on any
  `func` call. Frame bounds: `None` unbounded, `0` the current row, `-n` n preceding,
  `n` n following (SQLAlchemy's convention).
* **Shared windows**: `w = window(partition_by=..., order_by=..., rows=...)` defines a
  window once; `.over(w)` uses it, and the query gets `WINDOW w1 AS (...)` with
  `OVER w1` at each use. `.over(w, order_by=...)` / `.over(w, rows=...)` extend it
  (`OVER (w1 ROWS ...)`) when it has no ordering / frame of its own; partitioning comes
  from the window only (Postgres' rules). Each query, subquery or CTE declares the
  windows it uses.

  ```python
  w = window(partition_by=Post.author_id, order_by=Post.created_at)
  await Post.objects.select(Post.title, func.sum(Post.views).over(w), func.avg(Post.views).over(w),
                            func.sum(Post.views).over(w, rows=(-1, 0)))
  ```

  For now the planner takes **one shared window per query, in a query without
  `order_by()`, slicing or `lock()`**, and raises `QueryError` otherwise: sea-query
  (still on its master) keeps a single `WINDOW` per statement and writes it after `ORDER
  BY` / `LIMIT`, where Postgres rejects it. Inline `.over(partition_by=...)` has no such
  limit. The Python API already takes any number of windows, so the limit goes away with
  the SQL builder fix, without API changes.
* Window-only functions: `row_number`, `rank`, `dense_rank`, `percent_rank`,
  `cume_dist`, `ntile(n)`, `lag` / `lead(expr, offset=1, default=None)`,
  `first_value`, `last_value`, `nth_value(expr, n)`. Without `.over()` they raise
  `QueryError`. The aggregates (`count`, `sum`, ...) work over windows too.
* Window functions go in `select()` and `order_by()`. In `filter()`, `having()`,
  `group_by()` or an update they raise `QueryError`, since SQL evaluates them after
  `WHERE`. To filter on one, compute it in a CTE and filter that (below).

## CTEs: `WITH`

`qs.cte(name)` and `qs.select(...).cte(name)` name a query; whatever reads it declares
it, so it ends up once in the statement's `WITH` clause (subqueries included):

```python
ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
await Post.objects.from_(ranked).filter(ranked.c.rank <= 3)          # top 3 per author, as Post instances

totals = (Post.objects.select(Post.author_id, func.sum(Post.views).label("views"))
          .group_by(Post.author_id).cte("totals"))
await totals.select(totals.c.author_id, totals.c.views).filter(totals.c.views > 100)   # rows
await User.objects.filter(User.id.in_(totals.select(totals.c.author_id)))

chain = User.objects.filter(User.id == 1).cte(                          # WITH RECURSIVE
    "chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1))
await User.objects.from_(chain)
```

* **Joins**: `qs.join(cte, on, outer=False)` is `[LEFT] JOIN <cte> ON <on>`, so a CTE's
  columns come along with each row (django-cte's `cte.join(...)`):

  ```python
  await (User.objects.join(totals, totals.c.author_id == User.id, outer=True)
         .select(User, totals.c.views).order_by(totals.c.views.desc()))
  ```

  A row matching several CTE rows comes back once per match, as in SQL. Writes refuse
  a query set with joins.
* **Subqueries in `FROM`**: `Model.objects.from_(cte)` reads the model's rows from a
  CTE with the model's columns (`Post.objects....cte(...)` or a `select(Post, ...)`),
  so filters, relation paths, `select_related`, `prefetch_related`, `count()` and
  `select()` work as usual; its extra columns are `cte.c.<name>`. Writes refuse a
  `from_()` query set.
* `cte.select(...)` queries a CTE with no model: filter, group, order and slice it like
  any `select()`, await it for rows, or use it in `in_()`, `exists()` and
  `as_scalar()`. Columns are named as in `select()` rows (labels, field names).
* `recursive=lambda cte: query` adds the recursive part, `UNION ALL` or
  (`distinct=True`) `UNION`; it must have as many columns as the first part. It reads
  the CTE through `join()` (`User.objects.join(c, User.id == c.c.id + 1)`), or just by
  using `c.c.<col>`, which adds the CTE to `FROM` (`FROM users, chain WHERE ...`).
  `materialized=True / False` adds `[NOT] MATERIALIZED`.
* CTEs work in `update()` / `delete()` filters too. Each prefetch query declares the
  CTEs it reads. Two different CTEs with one name in a statement raise `ValueError`.

Compared with [django-cte](https://github.com/dimagi/django-cte) (4.0), the model is the
same (a CTE wraps a query, `cte.c.x` ↔ `cte.col.x`, `join`, recursion through a
function receiving the CTE, `materialized`), with three differences: a CTE is declared
by whatever reads it instead of `with_cte(cte, select=...)`, so it can't be forgotten
or declared twice; the recursive part is given on its own instead of as `base.union(...)`
inside the function; and `from_(cte)` gives model instances where `cte.queryset()`
gives Django rows typed by the CTE's query.

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
# ... with expressions: the existing row is `Post.<col>`, the proposed one `excluded(Post.<col>)`
await Post.objects.insert_many(rows).on_conflict(Post.slug).do_update(views=Post.views + excluded(Post.views))

# UPDATE / DELETE over a query: set-based, returns the row count
await Post.objects.filter(Post.author.name == "Alice").update(views=Post.views + 1)
await Post.objects.filter(Post.views < 10).delete()
# ... or the affected rows, with RETURNING (only added when you ask for it)
posts = await Post.objects.filter(...).update(views=Post.views + 1).returning()   # list[Post]
gone = await Post.objects.filter(...).delete().returning()

# Each row to its own values, by primary key: one statement per batch
n = await Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])
posts = await Post.objects.update_many(rows, batch_size=1000).returning()

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
* `do_update(*columns, **values)`: `columns` take the proposed values, `values` are
  plain values or expressions. With neither, every field passed to the insert except
  the conflict columns is overwritten.
* `update()` and `delete()` validate when called and return a statement: awaiting it
  gives the row count (no `RETURNING` in the SQL), `.returning()` gives the rows.
* `update_many(rows)` is Django's `bulk_update` without mutated instances: each row is a
  dict with the primary key and the fields to set, the same fields in every row, plain
  values only. Postgres gets one `UPDATE posts SET title = v.column2 FROM (VALUES ...) AS
  v WHERE posts.id = v.column1` per batch; databases without `UPDATE ... FROM` will get
  Django's `SET title = CASE WHEN id = ... THEN ... END WHERE id IN (...)`. Batches hold as
  many rows as fit in 65535 parameters unless `batch_size` says fewer, and run in one
  transaction. The query set's filters still apply (`alice.posts.update_many(rows)`
  leaves other authors' posts alone); ids that match nothing are skipped.
* A write statement that is never awaited emits a `RuntimeWarning`, the same safety
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
| update rows to different values | `session.execute(update(User), rows)` | `User.update(ids, rows)` (one by one) | `await User.objects.update_many(rows)` |
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

### Locks

```python
async with db.transaction():
    post = await Post.objects.lock().get(Post.id == 1)                      # FOR UPDATE
    await Post.objects.filter(...).lock(exclusive=False)                     # FOR SHARE
    jobs = await Job.objects.filter(Job.state == "ready").lock(skip_locked=True).limit(10)
    await Post.objects.lock(nowait=True).get(...)       # raises orm.LockNotAvailable if locked
    await db.lock("import:42")                          # advisory lock on a name, not a row
    got = await db.lock(42, exclusive=False, nowait=True)   # False instead of waiting
```

* `lock(exclusive=True, *, nowait=False, skip_locked=False)`: `exclusive` is
  `FOR UPDATE`, otherwise `FOR SHARE`. Only the model's own rows are locked
  (`FOR ... OF <table>`), never rows joined by `select_related`.
* Locks are held until the transaction ends, so both `lock()` and `db.lock()` raise
  `orm.TransactionRequired` outside `db.transaction()`. `count()` / `exists()` /
  `update()` / `delete()` on a locked query set raise `QueryError` (writes lock the rows
  they change anyway).
* `db.lock(key)` is a transaction-scoped Postgres advisory lock. Postgres keys are
  64-bit integers; a `str` key is hashed to one in Python (first 8 bytes of BLAKE2b,
  signed big-endian).
* No optimistic locking (version columns) on purpose.

## The FFI boundary

```
QuerySet ──(IR json + params list)──▶ Engine.run ──▶ planner (sea-query) ──▶ driver ──▶ Postgres
         ◀──(list[tuple] + prefetched rows, one pass)───────────────────────────────┘
```

* The schema IR is sent once, at `connect()`. Query IR is JSON with literals in a
  separate positional `params` list, converted to SQL values by the column type they're
  compared with (`native/src/convert.rs`).
* The IR talks about models, fields and relation paths only. Joins, `EXISTS` and
  aliases stay in `plan.rs`; SQL text and driver types stay in `db/`.
* Rust builds the result objects (`native/src/build.rs`). `Schema` holds each model's
  class (passed by `Registry.native()`), and an instance is made as `cls.__new__(cls)`
  would make it, its fields written straight into its `__dict__`: no Python code runs per
  row. `select_related` objects, prefetched lists, the reverse to-one (`post.author`)
  and `_db` (for `using()`) are attached in the same pass; `select()` rows are built as
  `Row` objects; `insert` / `.returning()` rows are instances too.

Rough numbers (release build, localhost TCP, 1000-row reads): building a query set and its
IR costs ~15 µs of Python. A 1-row read is ~0.1 ms over a bare `SELECT 1` on the same
connection. Reading 1000 posts takes ~1.9 ms (Django async was 10.4 ms in Phase 0), and
1000 posts + author via `select_related` ~3.1 ms (Django 18.1 ms). Building instances in
Rust took these from ~2.3 / ~4.3 ms, below the Phase 0 prototype's 2.5 / 4.7 ms
(`bench/engine_bench.py`, numbers in
[`bench/RESULTS.md`](../bench/RESULTS.md#instances-built-in-rust)).

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
8. `db.create_tables()` / `drop_tables()` stay as a development helper (idempotent
   `IF NOT EXISTS` DDL). Evolving databases use migrations: see [`schema.md`](schema.md).
9. **Async only**, no sync API (see `PLAN.md`, Decisions).

## Drivers and dialects

```
planner ──sea-query statement──▶ db::build(dialect) ──(SQL, [Value])──▶ Driver ──▶ database
   └── checks orm_core::dialect::Capabilities
```

* **SQL building is per dialect** (sea-query has Postgres, MySQL and SQLite builders).
  `orm_core::dialect` lists what each dialect can do (`RETURNING`, `ON CONFLICT`,
  `ILIKE`, row-lock flavours, `UPDATE ... FROM (VALUES ...)`, savepoints). The planner
  checks it, so a query needing a missing feature raises `QueryError` naming it (or is
  emulated: `icontains` becomes `LOWER(x) LIKE ...` without `ILIKE`) instead of sending
  SQL the database rejects. The same table is meant for build-time schema checks.
  `orm.connect(url, _disable=("ilike", "update_from_values"))` switches capabilities off,
  so the tests run those fallback paths on Postgres.
* **Execution is per driver**, behind the traits in `native/src/db/mod.rs`: `Driver`
  (a pool), `Executor` (run SQL with values, on the pool or in a transaction),
  `Transaction` (commit, rollback, savepoints via `begin()`), `RowSet` (decode rows by
  the schema's column types, straight into Python tuples). `db::connect` picks the
  driver from the URL scheme. Adding a database means a dialect, its capabilities and
  one driver; the planner and everything above stay as they are.
* **Postgres** uses tokio-postgres with a deadpool pool: binary protocol, parameter
  types stated in each `Parse` (taken from the value, which the planner typed by
  column), statements prepared once per connection and cached, `TCP_NODELAY` on.
  `sslmode` works as in libpq: `disable`; `prefer` (default) / `require` encrypt
  without checking the certificate; `verify-ca` / `verify-full` check it against the
  webpki roots (both also check the host name). A transaction that is dropped without
  commit or rollback closes its connection rather than returning it to the pool.

SeaORM was the engine until this point; only its sea-query and pool were in use (its
entities, relations and loaders need Rust types compiled per schema, and its
`find_also_related` / `load_many` are what `select_related` / `prefetch_related` already
do). Replacing it with tokio-postgres made most operations 5–33% faster with TLS off (with
TLS on, the TLS cost hides the difference)
(`bench/engine_bench.py`, numbers in [`bench/RESULTS.md`](../bench/RESULTS.md#engine-seaorm-vs-tokio-postgres)).

## Not done yet

* More SQL functions (one line each in the planner), string concatenation.
* Drivers for MySQL and SQLite; schema checks against a dialect's capabilities.
* Caching of compiled plans.
* `outer()` through relation paths (`outer(Post.author.name)`), `SEARCH` / `CYCLE`
  clauses for recursive CTEs, filtering on window functions without a CTE.
* Several shared windows per query, and shared windows with `ORDER BY` / `LIMIT`: needs
  a fix in sea-query (or our own SELECT writer); see Window functions.
* `has_one`, many-to-many, composite keys, decimal / array column types. (UUID and JSON
  are done, see [`schema.md`](schema.md).)
