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
| `engine/` | Rust crate `orm-engine`, shared by both bindings: `plan.rs` (IR → sea-query statements), `db/` (drivers: `mod.rs` traits, `postgres.rs`), `exec.rs` (running plans, prefetch, `update_many`), `migrate.rs` (applying migrations), `params.rs` (the values a binding passes in) |
| `cli/` | Rust crate `orm-cli`: the `orm` command line, run as the `orm` binary, `python -m orm` and `npx orm` |
| `bindings/python/` | Rust crate `orm._native` (PyO3) on top of `orm-engine`: `build.rs` (rows → instances and `Row`s), `convert.rs` (Python ↔ values), `lib.rs` |
| `bindings/node/`, `js/` | The TypeScript package on the same engine: [`typescript-api.md`](typescript-api.md) |
| `examples/blog/` | `schema.prisma`, the `models.py` / `models.pyi` generated from it, its migrations, `demo.py` |
| `tests/` | SQL shape tests (no DB), Postgres end-to-end tests, mypy + pyright stub checks |

Python 3.11 or newer (`select()` rows are typed with `TypeVarTuple`).

```bash
uv venv .venv && . .venv/bin/activate
uv pip install maturin pytest pytest-asyncio mypy pyright
uv pip install -e .        # the thin orm package
(cd packaging/python/tooling && maturin develop)   # native tooling profile; --release for benchmarks
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
| one-to-one `User.profile` | `_ProfilePath` | `Profile` or `None`, if loaded, else `NotLoaded` |
| many-to-many `Post.tags` | `_TagPath` | `ManyRelatedSet[Tag]`: the post's tags, with `add()` / `remove()` |

## Custom query-set methods

Named filters (Django's custom managers) go in a subclass of the model's query set, in your own module:

```python
# blog/queries.py
from typing import Self
from blog.models import Post, PostQuerySet

class PostQueries(PostQuerySet):
    def published(self) -> Self:
        return self.filter(Post.published)

    def popular(self, views: int = 100) -> Self:
        return self.filter(Post.views >= views)
```

Generate the models with the class, as `module:Class`:

```bash
python -m orm generate --query-set Post=blog.queries:PostQueries
```

or in `pyproject.toml`:

```toml
[tool.orm.query_sets]
Post = "blog.queries:PostQueries"
```

Then `Post.objects` is a `PostQueries`, and the methods chain with every builder method in both orders:
`await Post.objects.published().filter(Post.author_id == 1).popular()`.
`models.pyi` types `Post.objects` as the class, so mypy and pyright check the calls.

* `models.py` calls `orm.use_query_set(Post, "blog.queries:PostQueries")`.
  The module is imported on the first use of `Post.objects`, so it can import `blog.models` without an import cycle.
  `use_query_set(Model, cls)` also takes the class itself, for models from `orm.load()`.
* Relation sets have the methods too: `await user.posts.published()`, `post.tags.<method>()`.
  The stub does not type them on a relation set yet; `Post.objects.published().filter(Post.author_id == user.id)` is typed.
* `Prefetch(User.posts, Post.objects.published())` uses them for related rows.
* The class must subclass `QuerySet` and must not declare `__slots__` (it mixes with the relation-set classes); `use_query_set` raises `TypeError` otherwise.
  Keep state in the query, not in attributes: builder methods copy the instance `__dict__`, but nothing else.

## Queries

Builders return a new immutable `QuerySet`. Awaiting it runs the query. Awaiting the
same query set again gives the same rows without querying again (a new list each time;
concurrent awaits share one run): the result cache, as in Django. Builders and `.all()`
return new query sets, so `await qs.all()` re-reads; `await User.objects` always
queries, since `User.objects` lives as long as the class. `select()` caches the same way.

```python
await User.objects.filter(cond, cond2)        # list[User]; conditions AND-ed
User.objects.exclude(cond)                    # NOT (...)
User.objects.order_by(User.name, User.id.desc())
User.objects.order_by(-User.created_at, User.id)   # -column is column.desc()
User.objects.all()[10:20]                     # LIMIT 10 OFFSET 10
async for u in User.objects.filter(...): ...

await qs.first() / qs.last()                  # by ordering, else pk; None if empty
await qs.get(cond)                            # User.DoesNotExist / MultipleObjectsReturned
await qs.count() / qs.exists()
await qs.in_bulk([1, 2, 3])                   # {1: <User 1>, 3: <User 3>}: by pk, missing ids left out
await qs.in_bulk(emails, field=User.email)    # by a unique field; no ids: every row
qs.sql()                                      # SQL with values inlined, for debugging
```

`.asc(nulls="first")` and `.desc(nulls="last")` place NULLs; without `nulls`, the database decides.
`-column` is a descending `Ordering`, for `order_by()` and a `Prefetch` query set.
It is never SQL negation: write `0 - Post.views` for that.
`-` works on a column only, and an ordering in `filter()` or `select()` is a `TypeError`.

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

**Instances you already have.** `await orm.prefetch(instances, *paths)` loads relations onto them, as `prefetch_related` does for the rows of a query (Django's `prefetch_related_objects`):

```python
bundle = await Bundle.objects.get(Bundle.id == 1)
await orm.prefetch([bundle], Bundle.items.product, Prefetch(Bundle.versions, Version.objects.order_by(-Version.id)[:1], to_attr="latest"))
```

It takes the same paths and `Prefetch` objects. Only the prefetch queries run: the keys come from the instances, so their own rows are not read again.
The instances are of one model, and the queries run on the database each came from (`using=db` names another).

### One-to-one and many-to-many

```python
# One-to-one (`profile Profile?` on User, Profile.user_id unique)
us = await User.objects.select_related(User.profile)        # LEFT JOIN; user.profile is a Profile or None
us = await User.objects.prefetch_related(User.profile)      # +1 query; profile.user is set back too
await User.objects.filter(User.profile.role == Role.admin)  # EXISTS, like any relation

# Many-to-many (`tags Tag[] @relation(through: PostTag)`)
await post.tags                                     # list[Tag]
await post.tags.add(news, rust)                     # inserts PostTag rows (existing links are kept)
await post.tags.add(news, through_defaults={"position": 1})  # other fields of the new PostTag rows
await post.tags.remove(news)                        # deletes them; returns how many
await post.tags.set([news, py])                     # exactly these
await post.tags.clear()
tag = await post.tags.insert(name="go")             # insert a Tag and link it, in one transaction
await Post.objects.filter(Post.tags.name == "rust") # one EXISTS over tags JOIN post_tags
await Post.objects.select(Post.title, func.count(Post.tags))
await Post.objects.prefetch_related(Post.tags)      # +1 query: tags JOIN post_tags WHERE post_id IN (...)
await Tag.objects.prefetch_related(Tag.posts.author)
```

* `user.profile` raises `NotLoaded` until `select_related` / `prefetch_related` loads
  it, like a to-one relation; it is never a query in disguise.
* A many-to-many hop is planned as one hop through the join table, so Django's
  semantics hold: conditions in one `filter()` call must hold for the same tag, separate
  calls are independent, `exclude()` means "no such tag". Aggregates
  (`func.count(Post.tags)`) are correlated subqueries over the two tables.
* `post.tags` is a `ManyRelatedSet`: a query set over the post's tags (filter, order,
  count, ...) that reads prefetched rows when unchanged, like `user.posts`. `add()`,
  `remove()`, `set()` take instances or keys. Changing the links drops the prefetched
  rows. `add()` and `set()` take `through_defaults={...}`: values of the join model's
  other fields in the new join rows. Existing links keep their values, and the link's
  key fields can't be set this way. The join model stays an ordinary model for
  anything else (bulk inserts: `await PostTag.objects.insert_many(...)`).
* Prefetching selects the join row's key next to each tag, so a tag linked to two posts
  comes back once per post. `Prefetch(Post.tags, Tag.objects...[:3])` slices per post.

### Decimal, enum and array columns

```python
p = await Profile.objects.insert(user=alice, balance=Decimal("10.25"), role=Role.admin,
                                 links=["https://a.example"])
p.balance                                           # Decimal('10.25'), never a float
await p.update(balance=Profile.balance + Decimal("0.10"))
await Profile.objects.select(func.sum(Profile.balance)).scalar()   # Decimal

p.role is Role.admin                                # members of the generated enum
await Profile.objects.filter(Profile.role.in_([Role.admin, "editor"]))   # values work too

await Profile.objects.filter(Profile.links.has("https://a.example"))    # links @> ARRAY[...]
await Profile.objects.filter(Profile.links.has_any(urls))                # links && ...
Profile.links.has_all(urls) / Profile.links.contained_by(urls)          # @> / <@
await Profile.objects.select(func.cardinality(Profile.links))
await Profile.objects.select(Profile.links[1])                          # links[1]: the first element
await Profile.objects.select(func.unnest(Profile.links))                # one row per element
```

* Decimal parameters accept `Decimal`, `int`, `float` (through its `str`) and decimal
  strings; `NaN` and infinities are refused. Values outside the column's precision are
  the database's error.
* Enum classes are `StrEnum` (native and text storage) or `IntEnum` (int storage), so
  members compare equal to their stored values. A value the enum doesn't know (added in
  the database by hand) reads back as the plain value.
* Arrays are Python lists both ways (tuples are accepted); elements may be `None`.
* `Profile.links[i]` is SQL's element access: the index is 1-based, as in the SQL
  text, and an index out of range gives `None`. `func.unnest(...)` returns one row per
  element, so it is valid only as a `select()` column. SQLite has no array columns.

### JSON columns

```python
await Doc.objects.filter(Doc.meta["author"]["name"] == "Ann")       # meta -> 'author' -> 'name' = '"Ann"'
await Doc.objects.filter(Doc.meta["n"] > 3)                         # jsonb ordering: numbers compare as numbers
await Doc.objects.filter(Doc.meta["tags"][0].as_text() == "x")      # ->> : text, no JSON quotes
await Doc.objects.filter(Doc.meta["name"].as_text().icontains("an"))
await Doc.objects.filter(Doc.meta.json_contains({"kind": "post"}))  # meta @> '{"kind": "post"}'
Doc.meta.json_contained_by(value) / Doc.meta.has_key("tags")        # <@ / ?
await Doc.objects.filter(...).update(meta=Doc.meta.json_merge({"seen": True}))   # meta || '{...}'
```

* A path step is a string key or a 0-based integer index. The value is `jsonb`, so a
  comparison binds the other side as JSON: `== "x"` is the JSON string `"x"`, `== 5` the
  number. `as_text()` ends a path and reads the value as text (`->>`).
* On an array column, `col[1]` stays SQL's 1-based element access; a string key on a
  column that is not `Json` is a `TypeError`.
* `json_contains`, `json_contained_by` and `has_key` work on the column and on a path,
  and use a GIN index on the column (`@@index([meta], type: Gin)`).
* `json_merge(value)` is `||`: objects merge one level deep (the keys of `value` win);
  arrays join.
* PostgreSQL only: on SQLite these are a `QueryError`.

### Full-text search

```python
vector = func.to_tsvector("english", Post.body)
await Post.objects.filter(vector.matches("running dogs"))        # @@ plainto_tsquery('english', ...)
query = func.websearch_to_tsquery("english", '"quick fox" -lazy')
await Post.objects.filter(vector.matches(query)).order_by(func.ts_rank(vector, query).desc())
func.to_tsquery("english", "cat & !dog") / func.plainto_tsquery("english", text)
```

* The first argument `"english"` names the text search configuration; without it the
  server's `default_text_search_config` applies. It is written into the SQL as
  `'english'::regconfig` (it must be a name), so the expression matches an expression
  index in every plan, also a prepared statement's generic plan.
* `vector.matches("text")` with a plain string uses `plainto_tsquery` with the vector's
  configuration.
* Index the same expression with a GIN index in the schema:
  `@@index([sql("to_tsvector('english', body)")], type: Gin)`.
* `ts_rank` is a `float`. A tsvector or tsquery itself can't be a `select()` column.
* PostgreSQL only: on SQLite these are a `QueryError`.

### Partial rows, OR of query sets, column paths

* `only(...)` gives partial instances (see [`selection-and-defaults.md`](selection-and-defaults.md)).
  A column through to-one relations trims the joined instance: `Comment.objects.only(Comment.body, Comment.post.title)` loads `comment.post` with `select_related`, with only `title` public.
  Without a column of the model itself (`only(Comment.post.title)`), the model's instances keep only their primary key and relation keys, hidden, as Django's `only("post__title")`.
  A to-many path raises `TypeError`.
* `qs1 | qs2` is one query set with the filter `(filters of qs1) OR (filters of qs2)`; order, loading and `using` come from `qs1`.
  `qs2` must set nothing but filters, neither may be sliced, and each side has at most one `filter()`/`exclude()` call, else `QueryError`:
  conditions of one call through a to-many relation must hold for one related row, so two calls can't be joined into one.
* `Prefetch(User.posts, Post.objects.order_by(-Post.views), to_attr="best", one=True)` stores the first related row, or `None`, in `user.best` instead of a list.
  It needs `to_attr` and a to-many relation, and takes no slice (it is a limit of one per parent).
* `orm.column(Bundle, "items.product.title")` is the column a dotted path names, the same as `Bundle.items.product.title`.
  Each name before the last is a relation, the last a field; an unknown name raises `LookupError`.
  It is for adapters that map request names to columns (search and ordering filters). It is a function, not `Model.column`, so it can't collide with a field named `column`.

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

### Cursor pagination

```python
page = await Post.objects.filter(Post.published).order_by(-Post.created_at).paginate(first=20)
page.items, page.has_next, page.next_cursor
page = await qs.paginate(first=20, after=page.next_cursor)       # the next page
page = await qs.paginate(last=20, before=page.previous_cursor)   # the previous page
```

`paginate()` reads one page by keyset, like `batches()`, so rows added between pages do not repeat others.
A deep page stays fast when an index matches the order and its first column is NOT NULL: the page then starts with a plain bound on that column.
With a nullable first column there is no such bound, and Postgres reads the index from its start to the cursor.
The order is `order_by()`, else the schema default order, else the primary key.
The primary key is added as the last order column when no column of the order is unique.
`select_related`, `prefetch_related`, `only()` and query defaults apply to each page.

* `next_cursor` is the cursor of the last item, and `previous_cursor` that of the first; both are `None` on an empty page.
* `has_next` / `has_previous` come from one extra row in the direction of reading. In the other direction, they are `True` when the call gave a cursor.
* Order columns are columns of the model itself: an expression or a related column is a `QueryError`. JSON, array and enum columns are a `QueryError` too.
* A nullable order column needs `nulls=`: `Post.rank.desc(nulls="last")`.
* A cursor is opaque base64 of the order values and a fingerprint of the order. A cursor from another order or model is a `QueryError`.
* A cursor is not signed. A client can change it to start at any position of the same order, so do not use it for access control.
* A malformed cursor, or one with a value its column can't hold, is `QueryError("invalid cursor")`; a cursor that is not a string is a `TypeError`.
* A cursor holds positions, not filters: it stays valid after a filter change and starts at the same position.
* There is no total count; call `count()` for it.

### Prepared queries

A query the app runs over and over with different values (a lookup per request) can be
built once and then only bound per call. `orm.param("name")` marks a value to supply
on each call:

```python
# module level: the query set, its IR and the IR's JSON are built once
post_by_id = Post.objects.select_related(Post.author).filter(Post.id == orm.param("id")).prepare()
latest = (Post.objects.filter(Post.author_id == orm.param("user"))
          .order_by(Post.created_at.desc())
          .limit(orm.param("n")).offset(orm.param("skip")).prepare())

post = await post_by_id.get(id=post_id)         # also .first() .count() .exists()
posts = await latest(user=me.id, n=20, skip=40) # the rows
latest.params                                   # frozenset({'user', 'n', 'skip'})
```

A call copies the stored parameter list, fills in the values and hands the stored IR
JSON to the engine. It skips building the query set, the expression tree, the IR and
its JSON: −22% on `get` by primary key and −13% to −31% on a 50-row read with two filters
(`bench/RESULTS.md`, "Prepared queries"). Rust still plans each call, and Postgres
reuses the statement, which is prepared once per connection.

* A `param()` stands for one value: in comparisons and arithmetic, `like` / `contains`
  / `startswith` / `endswith` (the pattern is escaped when the value is bound),
  `has()`, `limit()` and `offset()`, and in filters of `Prefetch` query sets.
* Values can't be `None`: whether a comparison is `= $1` or `IS NULL` is fixed when the
  query is built, so write `is_null()` into the query instead.
* `in_(param(...))` is rejected, because the number of values is part of the query. A
  query set limited by a `param()` can't be sliced; use `limit()` / `offset()`.
* A `param()` outside a prepared query raises `QueryError`. Calls with a missing or
  unknown name raise `TypeError`.
* Prepared queries read. Inserts and updates already send their rows to Rust without an
  IR, so there is nothing to prepare.

`LIMIT` and `OFFSET` are parameters in the IR for every query, not only prepared ones:
`qs[10:20]` and `qs[30:40]` are the same IR document and the same SQL text.

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
  `lower`, `upper`, `length`, `abs`, `coalesce`, `now`, and for strings `concat`, `trim`,
  `ltrim`, `rtrim`, `replace`, `substr` (1-based start, optional length) and `strpos`
  (1-based, 0 if absent; `INSTR` on SQLite). Over the model's own columns an
  aggregate summarizes the rows (of each `group_by()` group). Over a relation path it is
  computed **per row in a correlated subquery**: `func.count(User.posts)` is `(SELECT
  COUNT(*) FROM posts WHERE posts.author_id = users.id)`. So two aggregates over different
  relations never multiply each other, unlike Django's JOIN-based `annotate(Count(...),
  Count(...))`, and they work in `filter()` too.
* **Filtered aggregates**: every aggregate takes `filter=cond`, which is
  `FILTER (WHERE cond)`: the aggregate reads only the rows where `cond` holds. Several
  conditional counts fit in one grouped query:

  ```python
  await Post.objects.select(
      Post.author_id,
      func.count(filter=Post.published),
      func.sum(Post.views, filter=Post.created_at > last_week),
  ).group_by(Post.author_id)
  await User.objects.select(User, func.count(User.posts, filter=User.posts.published))
  ```

  Over a relation, the filter applies inside the correlated subquery; it must read the
  same relation path as the aggregate. With `.over(...)` the filter comes before `OVER`.
  SQLite supports `FILTER` since 3.30 (the bundled SQLite is newer).
* **String concatenation** has two forms with different `NULL` rules:
  `func.concat(a, " ", b)` is `CONCAT(...)` and reads a `NULL` part as an empty string;
  `a.concat(b)` is `a || b` and is `NULL` when either side is `NULL`.
* Integer `SUM`s come back as `int` (cast to `bigint`), `AVG` as `float` (as a
  `Decimal` over decimal columns).
* Columns through to-one relations are `LEFT JOIN`ed; a to-many column outside an
  aggregate is rejected (it would repeat rows).
* `lock()` works with plain column selects, not with aggregates, window functions,
  `group_by` or `distinct`. `select_related` / `prefetch_related` don't combine with
  `select()`.
* A condition is a boolean column once labelled: `select(User.name, (User.id > 3).label("big"))`.
* **`CASE`** is `func.case((cond, value), ..., default=v)`: the value of the first true
  condition, else `default` (`None` without one). It is a value like any other, in
  `select()`, `filter()`, `order_by()`, `update()` and `do_update()`:

  ```python
  heat = func.case((Post.views >= 50, "hot"), (Post.views >= 20, "warm"), default="cold")
  await Post.objects.filter(heat == "hot")
  # Django's Sum(Case(When(published=True, then=1), default=0)):
  await Post.objects.select(Post.author_id, func.sum(func.case((Post.published, 1), default=0))).group_by(Post.author_id)
  await Post.objects.update(views=func.case((Post.views > 100, 100), default=Post.views))
  ```

  Plain values bind with the type the context expects (the updated field, the other side
  of a comparison), else with the type of the first branch that is not a plain value.
  With only plain values, a float or `Decimal` among integers makes the result a float or
  `Decimal`.

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
* `outer(Post.author.name)` reads a related row of the enclosing query's row through
  to-one relations, in a correlated scalar subquery. A to-many hop has several rows, so
  it is a `QueryError`.
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
posts = await Post.objects.insert_many([          # one statement per batch
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

# Read, or insert when missing: (row, created). The lookup is one unique constraint.
user, created = await User.objects.get_or_insert(email="a@x.io", defaults={"name": "Al"})

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
  required fields raise before any SQL runs. Fields left out get their
  `@client_default`, else the column's database default (`DEFAULT` in the VALUES
  list), and `RETURNING` reads them back.
* `insert_many(rows)` splits the rows so that no statement has more parameters than
  the database accepts (65,535 on Postgres, 32,766 on SQLite).
  `insert_many(rows, batch_size=n)` sets a lower number of rows for each statement.
  All the statements run in one transaction (or in the current one).
* `insert_many(rows, copy=True)` loads the rows with Postgres
  `COPY ... FROM STDIN (FORMAT binary)`, for large imports. `await` gives the row
  count, not instances. It is one statement: a duplicate key stops the whole load and
  no row is written. Client defaults fill values first; a field must be set in every
  row or in none, because COPY has no per-row `DEFAULT`. `on_conflict()`,
  `batch_size`, SQLite, composed models, models with native write behavior and fields
  that write through an SQL template (other than enums) raise.
  `bench/copy_insert.py` compares it with `insert_many(rows)`: 200,000 posts in 1.6 s
  against 3.2 s (measured once on a loaded machine; the batched insert also builds
  the instances).
* `on_conflict(*columns, where=cond)` picks a partial unique index:
  `.on_conflict(Task.shop, Task.task_type, where=Task.deleted_at.is_null())` gives
  `ON CONFLICT (shop, task_type) WHERE deleted_at IS NULL`. Postgres uses the
  condition to find the index, so it must match the index predicate without
  parameters (`is_null()`, a boolean column); a compared value is a parameter and
  Postgres cannot match it.
* `get_or_insert(defaults=..., **lookup)` reads the row that matches `lookup`.
  When there is none, it inserts `lookup` and `defaults` with
  `ON CONFLICT (lookup) DO NOTHING`, and reads again when a concurrent insert wins.
  So concurrent calls give one row, and exactly one call gets `created=True`.
  The lookup fields must be the fields of one unique constraint; Postgres raises
  otherwise. A `None` lookup value raises, because `NULL` never conflicts.
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

The column-role extensions change these writes (see `schema-extensions.md`).
`@timestamps.updated_at` and `@locking.version` fields are set on each update.
On a `@soft_delete.deleted_at` model, `delete()` soft-deletes, and `hard_delete()`, `undelete()`, `all_with_deleted()` and `deleted_only()` exist.
A stale versioned instance write raises `orm.VersionConflict`.

### Transactions

```python
async with db.transaction():          # commit on success, rollback on exception
    async with db.transaction(): ...  # nested: savepoint
```

The current transaction lives in a `ContextVar`, so queries inside the block use it
without passing it around. Tasks started inside the block inherit it.

```python
async with db.transaction():
    order = await Order.objects.insert(...)
    await db.on_commit(lambda: send_receipt.delay(order.id))   # after COMMIT only
```

`await db.on_commit(fn)` calls `fn()` after the outermost transaction on `db` commits, and awaits the result when it is awaitable.
A rollback drops the callback. A rolled-back savepoint drops only the callbacks registered inside it.
Outside a transaction, `fn()` runs at once.
Callbacks run in registration order, outside the transaction.
An error in a callback goes to the caller of `transaction()`, and the later callbacks do not run; the transaction is already committed.

### Database errors

An `orm.DatabaseError` (and its subclasses `IntegrityError`, `LockNotAvailable`) has three attributes:

* `sqlstate`: the five-character SQLSTATE, for example `"23505"` (unique), `"23503"` (foreign key), `"23502"` (not null), `"23514"` (check).
  SQLite constraint failures get the same Postgres codes; other SQLite errors give `None`.
* `constraint`: the name of the violated constraint (Postgres only), for example `"users_email_key"`.
* `detail`: the database's DETAIL line (Postgres only), for example `"Key (email)=(a@example.com) already exists."`.

Each is `None` when the database did not give it.

```python
try:
    await User.objects.insert(email=email, name=name)
except orm.IntegrityError as e:
    if e.constraint != "users_email_key":
        raise
    ...  # the email is taken
```

The ORM does not retry a transaction.
A transaction that fails with a serialization failure (`40001`) or a deadlock (`40P01`) can run again:

```python
async def transfer(db, a, b, amount, attempts=3):
    for attempt in range(attempts):
        try:
            async with db.transaction():
                ...  # the whole transaction, reads included
            return
        except orm.DatabaseError as e:
            if e.sqlstate not in ("40001", "40P01") or attempt == attempts - 1:
                raise
            await asyncio.sleep(0.05 * 2**attempt)
```

Retry the whole transaction, never one statement in it: Postgres aborts the transaction on these errors.
`40001` occurs only at the `REPEATABLE READ` and `SERIALIZABLE` isolation levels; the default `READ COMMITTED` gives deadlocks only.

### Protected writes

`@@protected_write` is an application-level check in the ORM. It does not protect the database.
Raw SQL (`db.execute`), migrations, other ORM processes without this schema, and other database clients can still write.
For database-level protection, use `@@trigger` or database grants.

```python
with orm.allow_writes(Post):               # a sync `with`: it does no I/O
    await post.update(title="new")

await Post.objects.filter(...).update(title="x")   # raises orm.WriteProtected
```

Every ORM write to a `@@protected_write` model fails outside `orm.allow_writes(...)` with `orm.WriteProtected`:
`insert`, `insert_many`, upserts, `update`, `update_many`, `delete`, instance writes and composed writes.
The check is on the table that the SQL writes, so `post.tags.add()` needs `allow_writes(PostTag)` when `PostTag` is protected.
The scope is a `ContextVar`, like the transaction: tasks started inside it get it.
A nested scope adds its models to the outer ones. `allow_writes` starts no transaction.
A query set is lazy, so the scope applies where the write is awaited, not where it is built:
`u = qs.update(...)` inside the scope and `await u` outside it raises `WriteProtected`.
Do not `yield` inside `allow_writes` in an async generator: when the consumer stops early, the scope stays open in the consumer task.
`orm.hooks.prepare_insert` and `prepare_update` also check protection, so a file field uploads nothing for a rejected write.
See `docs/schema.md`, "Protected writes".

### Locks

```python
async with db.transaction():
    post = await Post.objects.lock().get(Post.id == 1)                      # FOR UPDATE
    await Post.objects.filter(...).lock(exclusive=False)                     # FOR SHARE
    jobs = await Job.objects.filter(Job.state == "ready").lock(skip_locked=True).limit(10)
    await Post.objects.lock(nowait=True).get(...)       # raises orm.LockNotAvailable if locked
    await db.lock("import:42")                          # advisory lock on a name, not a row
    got = await db.lock(42, exclusive=False, nowait=True)   # False instead of waiting

async with db.lock("shop:7:sync", session=True, timeout=5):  # no transaction needed
    await call_shopify(...)
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
* `async with db.lock(key, session=True, timeout=5):` is a session advisory lock: it
  holds the lock for the block, with no transaction, so the block can make slow calls
  (HTTP) without an open transaction. The lock pins one pool connection; queries in the
  block use other connections. It waits at most `timeout` seconds (forever when `None`,
  not at all with `nowait=True`) and raises `orm.LockNotAvailable` when another session
  still holds the lock. The lock is released when the block ends, also on an error;
  when the unlock fails or the task is cancelled, the connection is closed, so the
  server releases the lock.
* Optimistic locking (version columns) is not in the core.
  Select the `orm-locking` extension and mark the field `@locking.version`; see `schema-extensions.md`.

### Read replicas

```python
db = await orm.connect(primary_url, replicas=[replica1_url, replica2_url])
users = await User.objects.filter(...)                  # a replica, in turn
fresh = await User.objects.using("primary").get(...)    # the primary
```

* Reads (`select`, `count`, `exists`, prepared queries) outside a transaction go to the next replica, in turn.
* Writes, raw `db.execute`, migrations, and every statement inside `db.transaction()` go to the primary.
* `db.primary` is a view of the database without its replicas; it shares the transactions of `db`.
  `.using("primary")` is `.using(<the query set's database>.primary)`, resolved when it is called.
* A replica can lag behind the primary. To read your own write, read in the same transaction or use `.using("primary")`.
* No health checks or failover: an error on a replica goes to the caller.
* `max_connections` applies to each pool. `db.close()` closes all of them.

### Tenants and row-level security

```python
with db.tenant(shop.id):                 # a sync `with`: it does no I/O
    orders = await Order.objects.all()    # RLS policies see current_setting('app.tenant')
```

* Every transaction on `db` in the block first runs `SELECT set_config('app.tenant', '<id>', true)`, the same as `SET LOCAL app.tenant = ...`, with the value bound as a parameter.
* A statement outside a transaction runs in a transaction of its own: `BEGIN`, `set_config`, the statement, `COMMIT`. That is four round trips instead of one (estimated); put many statements in one `db.transaction()`.
* A transaction that is already open keeps its setting. Savepoints use the setting of their transaction.
* The setting ends with each transaction, so pooled connections keep no tenant.
* Replicas get the same setting. Session locks (`session=True`) do not.
* The policy must read the setting, for example `USING (shop_id::text = current_setting('app.tenant', true))`. The ORM does not create policies. A superuser and the table owner bypass RLS unless the table has `FORCE ROW LEVEL SECURITY`.
* SQLite raises `QueryError`.
* `orm.scope(shop=...)` is the application-side filter (see `docs/selection-and-defaults.md`, "Scope values").

### Finding N+1 queries: `orm.debug`

The ORM never loads a relation by itself, so an N+1 comes from explicit code: a `load_x()` call or a query in a loop.
`orm.debug.n_plus_one` finds it:

```python
with orm.debug.n_plus_one(threshold=5, fail=True):
    for c in customers:
        await c.load_person()
# orm.debug.NPlusOne: 20 queries with one shape `SELECT ... FROM "person" WHERE "person"."id" = $1 ...`
#   at app/views.py:42; use select_related(Customer.person)
```

* The scope counts its statements by shape: the SQL with placeholders, without values.
  It reads the same events as `db.on_query`, so prefetch queries and `update_many` batches count one by one.
  Tasks started in the scope count too, and so do the queries of an inner scope.
  An inner scope cannot raise the threshold of an outer scope; the outer scope also counts the inner queries.
  The pages of one ORM loop (`batches()`, `iterate()`, the chunks of `in_bulk()`) count as one query when they have one shape.
* The call site is the line that awaits the query (`await qs`, `await p.customers.all()`).
  A query that `asyncio.gather()` or `create_task()` runs reports the line that started the event loop.
* When the block ends, a shape that ran more than `threshold` times (default 5) raises `orm.debug.NPlusOne` with `fail=True`, or gives an `orm.debug.NPlusOneWarning`.
  The exception and the `with ... as report` value carry the report: each shape, its SQL, its count, the call site of its first query and the fix.
* The fix is `select_related(...)` for a repeated `load_x()`, and `prefetch_related(...)` for a repeated unchanged to-many or many-to-many query (`await post.comments`, `await post.tags`).
* The call site and the SQL text are captured only inside the scope.
  Outside it, each query pays one `ContextVar` read (about 15 ns, measured).
* In a test suite, add `pytest_plugins = ["orm.testing"]` to `conftest.py`.
  The `n_plus_one` fixture counts the whole test and fails it at teardown; set `n_plus_one.threshold` to change the threshold.

### Raw SQL and query plans

```python
rows = await db.fetch("SELECT id, email FROM users WHERE created_at > $1 AND name = $2", since, "Ann")
# [{"id": 7, "email": "ann@example.com"}]
n = await db.execute("VACUUM ANALYZE posts")        # one or more statements, no parameters; rows affected

print(await Post.objects.filter(Post.author_id == 7).explain())              # EXPLAIN
print(await Post.objects.filter(Post.author_id == 7).explain(analyze=True))  # runs it: real times and rows
```

* `db.fetch(sql, *params)` runs one query and gives a list of dicts by column name (a repeated column name keeps the last value).
  Placeholders are `$1, $2, ...` on Postgres and `?` on SQLite.
* A parameter's type comes from its Python value: `int` is `bigint`, `str` is `text`, `dict` and `list` are JSON, and `datetime`, `date`, `Decimal`, `UUID` and `bool` have their own types.
  Cast in the SQL where a column needs another type: `WHERE id = $1::uuid` for a `str`.
* Cells come back by the column type that the database reports.
  Postgres gives `bigint`, `integer`, `smallint`, `double precision`, `real`, `boolean`, text types and enums, `timestamptz`, `date`, `uuid`, `json`, `jsonb`, `numeric` and arrays of them.
  Any other type (`timestamp`, `interval`, `bytea`, ...) raises `DatabaseError`; cast it in the SQL (`::text`, `::timestamptz`).
  SQLite gives each value by its storage class (`int`, `float`, `str`, `None`); a blob raises.
* `fetch` runs in the current transaction, and query hooks see it.
* `qs.explain(analyze=False)` gives the plan of the query set's SELECT as text, not of its prefetch queries.
  On Postgres, `analyze=True` runs the query with `EXPLAIN (ANALYZE, BUFFERS)`; a query set with `lock()` raises `QueryError` there, because the run would take the row locks.
  On SQLite, it gives `EXPLAIN QUERY PLAN`, each step indented under its parent. SQLite has no `analyze`, so `analyze=True` raises `QueryError`.

### Query hooks and OpenTelemetry

`db.on_query(hook)` calls `hook(event)` after each statement that the database runs, and gives a function that removes the hook:

```python
def log_slow(e: orm.QueryEvent) -> None:
    if e.duration > 0.1:
        logger.warning("%.0f ms, %d rows: %s", e.duration * 1000, e.rows, e.sql)

remove = db.on_query(log_slow)
```

* `orm.QueryEvent` has `sql` (the SQL with placeholders, never the values), `start` (Unix seconds), `duration` (seconds), `rows` and `error`.
  `rows` is the rows returned, or the rows affected by a statement that returns no rows.
  `error` is the database's message when the statement failed; the ORM call still raises.
* Each statement gives one event: a prefetch query, each `update_many` batch, each `db.execute`.
  Reads sent to a replica and the lock and unlock of `db.lock(..., session=True)` give events too.
  `COMMIT`, `ROLLBACK` and the `set_config` statements of `db.tenant()` give no event.
* The hooks belong to the database: `db.primary` shares them.
* The engine times the statement in Rust, from the send to the last row, without the conversion to Python objects.
* The hook runs after the ORM call ends, in the task that made the call.
  So context variables, for example the current span or a request id, are those of the caller.
  An exception in a hook propagates to that caller.
* With no hook and no N+1 scope, a query pays one list check and one `ContextVar` read.
  With a hook, each call creates a trace and each statement records one event.
  A prepared `get` on SQLite took 252 µs with an empty hook and 247 µs without (measured on a loaded machine, so the difference is noise-level).

`orm.otel.instrument(db)` gives each statement a client span, and gives a function that stops the spans.
It needs `opentelemetry-api` (`pip install 'orm[otel]'`); configure the SDK and the exporter as usual.
`instrument(db, tracer=t)` uses the tracer `t` instead of `get_tracer("orm")`.

* The span starts and ends at the statement's times. Its parent is the current span of the caller.
* The span name is the SQL operation (`SELECT`, `INSERT`, ...).
  The attributes are `db.system.name` (`postgresql` or `sqlite`), `db.operation.name`, `db.query.text` (with placeholders) and `db.response.returned_rows`.
* A failed statement gets the `ERROR` status and the database's message.

### Hooks for packages: `orm.hooks`

A package that changes writes and reads from outside the ORM (for example
`orm-file-storage`) uses these public hooks, not private names:

* `prepare_insert(qs, values)` checks one insert as `insert()` does (required fields,
  conversion, native planning) without SQL or I/O. `await prepared.execute(values)`
  inserts it, by default with the checked values.
* `prepare_update(qs, values)` checks `qs.update(**values)` the same way.
  `prepared.unique` is true when the engine proves that the filters pin one row by a
  non-null primary key or unique field. `await prepared.execute(values, returning=False)`
  runs it.
* `decode_field(Model, name, decode)` reads the loaded value of a field as
  `decode(value)`. The instance keeps the stored value for writes and filters.

## Model metadata and test factories

The ORM has no factory library. It gives the two things a factory library needs:

* `orm.describe(Model)`: plain data about the model.
  `fields` gives per field the name, column, schema type, `python_type` (an enum field gives its enum class), `nullable`, `array` (with `element_type`), `max_length`, `primary_key`, `unique`, `default` (`"database"`, `"client"` or `None`) and `insert` (whether `insert()` takes it).
  `relations` gives the kind (`belongs_to`, `has_one`, `has_many`, `many_to_many`), the target class, the `from`/`to` fields, the `through` model and `nullable`.
  `unique` lists the unique keys, the primary key first.
* The insert path: `await Model.objects.insert(**values)`, which also takes a related instance for a to-one key (`author=user`).

Instances come only from the database, so a factory builds the insert values, not an instance.
With factory_boy, `_build` gives the values and `_create` gives the insert, to await:

```python
class PostFactory(factory.Factory):
    class Meta:
        model = Post

    title = factory.Sequence(lambda n: f"post {n}")
    body = "..."
    author = factory.SubFactory(UserFactory)  # a User instance, awaited by the caller first

    @classmethod
    def _build(cls, model_class, *args, **kwargs):
        return kwargs

    @classmethod
    def _create(cls, model_class, *args, **kwargs):
        return model_class.objects.insert(**kwargs)  # a coroutine: `await PostFactory.create()`
```

factory_boy has no async support, so a `SubFactory` with `create` gives a coroutine; build related rows first, or use a small async factory over `describe()` (see `tests/test_query_api.py`).

## The FFI boundary

```
QuerySet ──(IR json + params list)──▶ Engine.run ──▶ planner (sea-query) ──▶ driver ──▶ Postgres
         ◀──(list[tuple] + prefetched rows, one pass)───────────────────────────────┘
```

* The schema IR is sent once, at `connect()`. Query IR is JSON with literals (and
  `LIMIT` / `OFFSET`) in a separate positional `params` list, converted to SQL values by the column type they're
  compared with (`bindings/python/src/convert.rs`).
* The IR talks about models, fields and relation paths only. Joins, `EXISTS` and
  aliases stay in `plan.rs`; SQL text and driver types stay in `db/`.
* Rust builds the result objects (`bindings/python/src/build.rs`). `Schema` holds each model's
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
   the database (DDL `DEFAULT`) and come back via `RETURNING`. A schema
   `@client_default` fills an omitted insert value natively. Callable defaults
   declared in Python fill an omitted value at the same step, before native
   transforms and validators; an explicit value, also `None`, wins over both.
6. **Instance equality** is by model class and primary key (Django).
7. **One default database** set by `connect()`, `.using(db)` to override, like Django's
   `using()`.
8. `db.create_tables()` / `drop_tables()` stay as a development helper (idempotent
   `IF NOT EXISTS` DDL). Evolving databases use migrations: see [`schema.md`](schema.md).
   An existing database is adopted with `await orm.migrations.pull(db)` (its schema file),
   `Migrator.baseline()` and `Migrator.drift()`
   (see [`schema.md`](schema.md#adopting-a-live-database-pull-baseline-drift)).
   A migration may hold a `data.py` with `async def run(db)`, run by `Migrator.upgrade()`
   and `python -m orm migrate` in the migration's transaction
   (see [`schema.md`](schema.md#data-migrations)).
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
* **Execution is per driver**, behind the traits in `engine/src/db/mod.rs`: `Driver`
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

* A MySQL driver.
* Caching of compiled plans: a decision, not open work. [`performance.md`](performance.md)
  keeps it out until profiling shows a benefit; `prepare()` and the driver's statement
  cache exist.
* `SEARCH` / `CYCLE` clauses for recursive CTEs, filtering on window functions without
  a CTE.
* Several shared windows per query, and shared windows with `ORDER BY` / `LIMIT`: needs
  a fix in sea-query (or our own SELECT writer); see Window functions.
* Composite keys. (One-to-one, many-to-many, decimal, enum, array, UUID and JSON
  columns are done, see [`schema.md`](schema.md).)
* `select_related` through a many-to-many relation (it would repeat rows; use
  `prefetch_related`).
