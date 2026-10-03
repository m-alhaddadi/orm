"""End-to-end tests: in_bulk, exists() / scalar subqueries with outer(), window functions,
CTEs, nested and filtered prefetches, instances built in Rust."""

from datetime import datetime, timedelta, timezone

import pytest
from blog.models import Comment, Post, User

import orm
from conftest import DATABASE_URL
from orm import Prefetch, QueryError, exists, func, outer, window

NOW = datetime.now(timezone.utc)


async def seed():
    alice, bob, carol = await User.objects.insert_many(
        [
            {"email": "alice@example.com", "name": "Alice"},
            {"email": "bob@example.com", "name": "Bob"},
            {"email": "carol@example.com", "name": "Carol"},
        ]
    )
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 5, "created_at": NOW - timedelta(days=3)},
            {"author": alice, "title": "a2", "body": "", "views": 50, "created_at": NOW - timedelta(days=2)},
            {"author": alice, "title": "a3", "body": "", "views": 20, "created_at": NOW - timedelta(days=1)},
            {"author": bob, "title": "b1", "body": "", "views": 100, "created_at": NOW - timedelta(days=5)},
        ]
    )
    a1, a2, a3, b1 = posts
    await Comment.objects.insert_many(
        [
            {"post": a2, "author": bob, "body": "c1"},
            {"post": a2, "author": None, "body": "c2"},
            {"post": b1, "author": alice, "body": "c3"},
        ]
    )
    return (alice, bob, carol), posts


# -- in_bulk ---------------------------------------------------------------------------------


async def test_in_bulk(clean):
    (alice, bob, carol), _ = await seed()
    got = await User.objects.in_bulk([alice.id, carol.id, 999, alice.id])
    assert got == {alice.id: alice, carol.id: carol}
    assert got[alice.id].name == "Alice"
    assert await User.objects.in_bulk([]) == {}
    by_email = await User.objects.in_bulk(["bob@example.com"], field=User.email)
    assert by_email == {"bob@example.com": bob}
    assert set(await User.objects.filter(User.name != "Bob").in_bulk()) == {alice.id, carol.id}
    with pytest.raises(ValueError, match="unique"):
        await User.objects.in_bulk(["Bob"], field=User.name)
    with pytest.raises(QueryError, match="sliced"):
        await User.objects.all()[:1].in_bulk([1])


async def test_in_bulk_chunks(clean, monkeypatch):
    (alice, bob, carol), _ = await seed()
    monkeypatch.setattr(orm.query, "IN_BULK_CHUNK", 2)
    assert set(await User.objects.in_bulk([alice.id, bob.id, carol.id])) == {alice.id, bob.id, carol.id}


# -- exists() and scalar subqueries ------------------------------------------------------


async def test_exists_expression(clean):
    (alice, bob, carol), _ = await seed()
    popular = exists(Post.objects.filter(Post.author_id == outer(User.id), Post.views >= 50))
    assert {u.name for u in await User.objects.filter(popular)} == {"Alice", "Bob"}
    assert {u.name for u in await User.objects.filter(~popular)} == {"Carol"}
    # In select(): a boolean per row.
    rows = await User.objects.select(User.name, popular.label("popular")).order_by(User.name)
    assert [tuple(r) for r in rows] == [("Alice", True), ("Bob", True), ("Carol", False)]
    # Uncorrelated.
    assert len(await User.objects.filter(exists(Post.objects.filter(Post.views > 1000)))) == 0
    # A select() with group_by / having.
    many = exists(
        Post.objects.filter(Post.author_id == outer(User.id)).select(Post.author_id).group_by(Post.author_id).having(func.count() > 2)
    )
    assert [u.name for u in await User.objects.filter(many)] == ["Alice"]


async def test_scalar_subquery(clean):
    (alice, bob, carol), _ = await seed()
    latest = (
        Post.objects.filter(Post.author_id == outer(User.id))
        .order_by(Post.created_at.desc())
        .select(Post.title)[:1]
        .as_scalar()
    )
    rows = await User.objects.select(User, latest.label("latest")).order_by(User.id)
    assert [(u.name, t) for u, t in rows] == [("Alice", "a3"), ("Bob", "b1"), ("Carol", None)]
    assert rows[0].latest == "a3" and rows[0].user == alice
    # In filters and ordering.
    best = Post.objects.filter(Post.author_id == outer(User.id)).select(func.max(Post.views)).as_scalar()
    assert [u.name for u in await User.objects.filter(best > 60)] == ["Bob"]
    assert [u.name for u in await User.objects.filter(best.is_not_null()).order_by(best.desc())] == ["Bob", "Alice"]
    # Correlated to the same model: posts above their author's average.
    avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
    assert sorted(p.title for p in await Post.objects.filter(Post.views > avg)) == ["a2"]


async def test_scalar_subquery_in_update(clean):
    (alice, bob, carol), _ = await seed()
    n_comments = Comment.objects.filter(Comment.post_id == outer(Post.id)).select(func.count()).as_scalar()
    await Post.objects.update(views=n_comments)
    views = {p.title: p.views for p in await Post.objects.all()}
    assert views == {"a1": 0, "a2": 2, "a3": 0, "b1": 1}


async def test_outer_errors(clean):
    with pytest.raises(ValueError, match=r"use outer\(User.id\)"):
        User.objects.filter(exists(Post.objects.filter(Post.author_id == User.id))).sql()
    with pytest.raises(ValueError, match="not a column of an enclosing query"):
        User.objects.filter(User.id == outer(User.id)).sql()
    with pytest.raises(TypeError, match="relation path"):
        outer(User.posts.views)
    with pytest.raises(QueryError, match="one column"):
        Post.objects.select(Post.id, Post.title).as_scalar()


# -- window functions ------------------------------------------------------------------------


async def test_window_functions(clean):
    await seed()
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    rows = await Post.objects.select(Post.title, rank.label("rank")).order_by(Post.title)
    assert [tuple(r) for r in rows] == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]

    running = func.sum(Post.views).over(partition_by=Post.author_id, order_by=Post.created_at, rows=(None, 0))
    rows = await Post.objects.select(Post.title, running.label("total")).order_by(Post.created_at)
    assert [tuple(r) for r in rows] == [("b1", 100), ("a1", 5), ("a2", 55), ("a3", 75)]

    prev = func.lag(Post.views, 1, -1).over(partition_by=Post.author_id, order_by=Post.created_at)
    nxt = func.lead(Post.title).over(order_by=Post.created_at)
    rows = await Post.objects.filter(Post.author.name == "Alice").select(Post.title, prev.label("prev"), nxt.label("next")).order_by(Post.created_at)
    assert [tuple(r) for r in rows] == [("a1", -1, "a2"), ("a2", 5, "a3"), ("a3", 50, None)]

    rows = await Post.objects.select(
        Post.title,
        func.rank().over(order_by=Post.author_id).label("rank"),
        func.dense_rank().over(order_by=Post.author_id).label("dense"),
        func.ntile(2).over(order_by=Post.id).label("half"),
        func.count().over().label("n"),
        func.first_value(Post.title).over(partition_by=Post.author_id, order_by=Post.views).label("least"),
        func.percent_rank().over(order_by=Post.views).label("pct"),
    ).order_by(Post.id)
    assert [(r.rank, r.dense, r.half, r.n, r.least) for r in rows] == [
        (1, 1, 1, 4, "a1"), (1, 1, 1, 4, "a1"), (1, 1, 2, 4, "a1"), (4, 2, 2, 4, "b1")
    ]
    assert rows[3].pct == 1.0

    # Ordering by a window function; a frame by range.
    titles = await Post.objects.order_by(func.row_number().over(order_by=Post.views.desc())).select(Post.title).scalars()
    assert titles == ["b1", "a2", "a3", "a1"]
    rows = await Post.objects.select(Post.title, func.sum(Post.views).over(order_by=Post.views, range=(-20, 0)).label("s")).order_by(Post.views)
    assert [r.s for r in rows] == [5, 25, 50, 100]


async def test_window_function_errors(clean):
    rank = func.row_number().over(order_by=Post.views)
    with pytest.raises(QueryError, match="window functions can only be used in select"):
        await Post.objects.filter(rank <= 3)
    with pytest.raises(QueryError, match="window function: add .over"):
        await Post.objects.select(func.row_number())
    with pytest.raises(QueryError, match="window functions can only be used in select"):
        await Post.objects.update(views=rank)
    with pytest.raises(ValueError, match="rows or range"):
        func.count().over(rows=(None, 0), range=(None, 0))
    async with (await _db()).transaction():
        with pytest.raises(QueryError, match="window functions"):
            await Post.objects.lock().order_by(rank)


async def test_named_windows(clean):
    await seed()
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    q = Post.objects.select(
        Post.title,
        func.sum(Post.views).over(w).label("total"),
        func.avg(Post.views).over(w).label("avg"),
        func.row_number().over(w).label("n"),
    )
    assert q.sql().count("WINDOW") == 1 and 'OVER w1' in q.sql()
    rows = sorted(await q)
    assert [(r.title, r.total, r.n) for r in rows] == [("a1", 5, 1), ("a2", 55, 2), ("a3", 75, 3), ("b1", 100, 1)]
    assert rows[1].avg == 27.5
    # Extending a window with a frame: OVER (w1 ROWS ...).
    last2 = func.sum(Post.views).over(w, rows=(-1, 0))
    sql = Post.objects.select(Post.title, last2.label("s")).sql()
    assert "OVER (w1 ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)" in sql
    assert sorted(tuple(r) for r in await Post.objects.select(Post.title, last2.label("s"))) == [
        ("a1", 5), ("a2", 55), ("a3", 70), ("b1", 100)
    ]
    # ... and a window without order_by with one.
    part = window(partition_by=Post.author_id)
    rows = await Post.objects.select(Post.title, func.rank().over(part, order_by=Post.views.desc()).label("r"))
    assert sorted(tuple(r) for r in rows) == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]
    # The same window in a subquery is declared there, not in the outer query.
    ranked = Post.objects.select(Post, func.row_number().over(window(Post.author_id, Post.views.desc())).label("rank")).cte("ranked")
    assert [p.title for p in await Post.objects.from_(ranked).filter(ranked.c.rank == 1)] in (["a2", "b1"], ["b1", "a2"])


async def test_named_window_limits(clean):
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    with pytest.raises(ValueError, match="partition_by"):
        func.sum(Post.views).over(w, partition_by=Post.id)
    with pytest.raises(ValueError, match="without its own order_by"):
        func.sum(Post.views).over(w, order_by=Post.id)
    with pytest.raises(ValueError, match="without its own frame"):
        func.sum(Post.views).over(window(rows=(None, 0)), rows=(None, 0))
    # Limits of the current SQL builder: one WINDOW per query, none with ORDER BY / LIMIT.
    other = window(order_by=Post.id)
    with pytest.raises(QueryError, match="only one named window"):
        await Post.objects.select(func.sum(Post.views).over(w), func.sum(Post.views).over(other))
    with pytest.raises(QueryError, match="order_by"):
        await Post.objects.select(func.sum(Post.views).over(w)).order_by(Post.id)
    with pytest.raises(QueryError, match="slicing"):
        await Post.objects.select(func.sum(Post.views).over(w))[:3]


async def test_join_cte(clean):
    (alice, bob, carol), _ = await seed()
    totals = (
        Post.objects.select(Post.author_id, func.sum(Post.views).label("views"), func.count().label("n"))
        .group_by(Post.author_id)
        .cte("totals")
    )
    rows = await User.objects.join(totals, totals.c.author_id == User.id).select(User, totals.c.views).order_by(totals.c.views.desc())
    assert [(u.name, v) for u, v in rows] == [("Bob", 100), ("Alice", 75)]
    rows = await User.objects.join(totals, totals.c.author_id == User.id, outer=True).select(User.name, totals.c.n).order_by(User.id)
    assert [tuple(r) for r in rows] == [("Alice", 3), ("Bob", 1), ("Carol", None)]
    # Filters, instances, count, prefetch.
    busy = User.objects.join(totals, totals.c.author_id == User.id).filter(totals.c.n > 1)
    assert [u.name for u in await busy] == ["Alice"] and await busy.count() == 1
    assert [len(u.posts.cached) for u in await busy.prefetch_related(User.posts)] == [3]
    # Two CTEs joined.
    commenters = Comment.objects.select(Comment.author_id, func.count().label("c")).group_by(Comment.author_id).cte("commenters")
    rows = await (
        User.objects.join(totals, totals.c.author_id == User.id)
        .join(commenters, commenters.c.author_id == User.id)
        .select(User.name, totals.c.views, commenters.c.c)
        .order_by(User.name)
    )
    assert [tuple(r) for r in rows] == [("Alice", 75, 1), ("Bob", 100, 1)]
    with pytest.raises(QueryError, match="can't run on a query set with from_"):
        User.objects.join(totals, totals.c.author_id == User.id).update(name="x")
    with pytest.raises(ValueError, match="already read"):
        User.objects.join(totals, totals.c.author_id == User.id).join(totals, totals.c.n > 0)


async def test_recursive_cte_with_join(clean):
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(5)])
    first = users[0].id
    walk = User.objects.filter(User.id == first).select(User.id, User.name, func.abs(User.id - User.id).label("depth")).cte(
        "walk",
        recursive=lambda w: User.objects.join(w, User.id == w.c.id + 1).filter(w.c.depth < 2).select(User.id, User.name, w.c.depth + 1),
    )
    assert "JOIN \"walk\" ON" in walk.select(walk.c.name).sql()
    rows = await walk.select(walk.c.name, walk.c.depth).order_by(walk.c.depth)
    assert [tuple(r) for r in rows] == [("u0", 0), ("u1", 1), ("u2", 2)]


async def _db():
    return orm.get_database()


# -- CTEs --------------------------------------------------------------------------------------


async def test_cte_from_filters_window(clean):
    await seed()
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
    top = await Post.objects.from_(ranked).filter(ranked.c.rank <= 2).order_by(Post.title)
    assert [p.title for p in top] == ["a2", "a3", "b1"]
    assert isinstance(top[0], Post) and top[0].author_id is not None
    # Relation filters, select_related and prefetch still work on rows read from a CTE.
    top = await Post.objects.from_(ranked).filter(ranked.c.rank == 1, Post.author.name == "Alice").select_related(Post.author)
    assert [(p.title, p.author.name) for p in top] == [("a2", "Alice")]
    top = await Post.objects.from_(ranked).filter(ranked.c.rank == 1).prefetch_related(Post.comments).order_by(Post.title)
    assert [len(p.comments.cached) for p in top] == [2, 1]
    assert await Post.objects.from_(ranked).filter(ranked.c.rank == 1).count() == 2
    assert await Post.objects.from_(ranked).filter(ranked.c.rank == 9).exists() is False
    rows = await Post.objects.from_(ranked).select(Post.title, ranked.c.rank).order_by(Post.title)
    assert [tuple(r) for r in rows] == [("a1", 3), ("a2", 1), ("a3", 2), ("b1", 1)]


async def test_cte_select_and_subqueries(clean):
    (alice, bob, carol), _ = await seed()
    totals = (
        Post.objects.select(Post.author_id, func.sum(Post.views).label("views"), func.count().label("n"))
        .group_by(Post.author_id)
        .cte("totals")
    )
    rows = await totals.select(totals.c.author_id, totals.c.views).filter(totals.c.n > 1)
    assert [tuple(r) for r in rows] == [(alice.id, 75)]
    assert [tuple(r) for r in await totals.select(func.max(totals.c.views).label("best"))] == [(100,)]
    # A CTE read from a subquery: declared on the statement, once.
    heavy = User.objects.filter(User.id.in_(totals.select(totals.c.author_id).filter(totals.c.views > 80)))
    assert [u.name for u in await heavy] == ["Bob"]
    assert heavy.sql().count("WITH") == 1
    big = exists(totals.select(totals.c.n).filter(totals.c.author_id == outer(User.id), totals.c.views > 50))
    assert [u.name for u in await User.objects.filter(big).order_by(User.id)] == ["Alice", "Bob"]
    # ... in writes too.
    n = await User.objects.filter(User.id.in_(totals.select(totals.c.author_id))).update(name="writer")
    assert n == 2
    assert await User.objects.filter(User.name == "writer").count() == 2
    # CTEs reading CTEs, materialized.
    top = totals.select(totals.c.author_id).filter(totals.c.views > 80).cte("top", materialized=True)
    assert "MATERIALIZED" in User.objects.filter(User.id.in_(top.select(top.c.author_id))).sql()
    assert [u.id for u in await User.objects.filter(User.id.in_(top.select(top.c.author_id)))] == [bob.id]
    assert await Post.objects.filter(Post.author_id.in_(totals.select(totals.c.author_id))).delete() == 4


async def test_recursive_cte(clean):
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(5)])
    first = users[0].id
    chain = User.objects.filter(User.id == first).cte(
        "chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1, User.id < first + 3)
    )
    assert [u.name for u in await User.objects.from_(chain).order_by(User.id)] == ["u0", "u1", "u2"]
    # A select() CTE with a depth column.
    walk = User.objects.filter(User.id == first).select(User.id, func.abs(User.id - User.id).label("depth")).cte(
        "walk",
        recursive=lambda w: User.objects.filter(User.id == w.c.id + 1).select(User.id, w.c.depth + 1),
    )
    assert await walk.select(walk.c.depth).order_by(walk.c.depth.desc()).limit(1).scalars() == [4]


async def test_cte_errors(clean):
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    with pytest.raises(TypeError, match="columns of Post"):
        Post.objects.from_(totals)
    with pytest.raises(AttributeError, match="no column 'nope'"):
        totals.c.nope
    with pytest.raises(QueryError, match="only available in queries reading totals"):
        await Post.objects.filter(totals.c.n > 1)
    with pytest.raises(QueryError, match="can't run on a query set with from_"):
        await Post.objects.from_(Post.objects.cte("p")).delete()
    other = Post.objects.select(Post.author_id).cte("totals")
    with pytest.raises(ValueError, match="two different CTEs"):
        User.objects.filter(User.id.in_(totals.select(totals.c.author_id)), User.id.in_(other.select(other.c.author_id))).sql()
    with pytest.raises(ValueError, match="several columns named id"):
        Post.objects.select(Post, Post.id.label("id")).cte("x")


# -- prefetch -----------------------------------------------------------------------------------


async def test_nested_prefetch(clean):
    (alice, bob, carol), _ = await seed()
    users = await User.objects.prefetch_related(User.posts.comments, User.comments).order_by(User.id)
    a, b, c = users
    assert [p.title for p in a.posts.cached] == ["a1", "a2", "a3"]
    assert [[x.body for x in p.comments.cached] for p in a.posts.cached] == [[], ["c1", "c2"], []]
    assert a.posts.cached[1].comments.cached[0].post is a.posts.cached[1]  # back reference
    assert a.posts.cached[0].author is a
    assert [x.body for x in b.comments.cached] == ["c1"] and c.posts.cached == []


async def test_to_one_prefetch(clean):
    await seed()
    comments = await Comment.objects.prefetch_related(Comment.post.author, Comment.author).order_by(Comment.id)
    assert [(x.post.title, x.post.author.name) for x in comments] == [("a2", "Alice"), ("a2", "Alice"), ("b1", "Bob")]
    assert comments[0].post is comments[1].post  # one object per related row
    assert [x.author.name if x.author else None for x in comments] == ["Bob", None, "Alice"]


async def test_filtered_prefetch(clean):
    (alice, bob, carol), _ = await seed()
    users = await User.objects.prefetch_related(
        Prefetch(User.posts, Post.objects.filter(Post.views >= 20).order_by(Post.views.desc()))
    ).order_by(User.id)
    # Like Django: the filtered rows are what user.posts holds now.
    assert [p.title for p in users[0].posts.cached] == ["a2", "a3"]
    assert [p.title for p in await users[0].posts] == ["a2", "a3"]
    assert await users[0].posts.count() == 3  # a new query sees every post
    users = await User.objects.prefetch_related(
        Prefetch(User.posts, Post.objects.filter(Post.views >= 20), to_attr="popular"),
    ).order_by(User.id)
    assert [p.title for p in users[0].popular] == ["a2", "a3"] and users[2].popular == []
    with pytest.raises(orm.NotLoaded):
        users[0].posts.cached


async def test_sliced_prefetch_is_per_parent(clean):
    await seed()
    top = Post.objects.order_by(Post.views.desc()).prefetch_related(Post.comments)[:2]
    users = await User.objects.prefetch_related(Prefetch(User.posts, top, to_attr="top")).order_by(User.id)
    assert [[p.title for p in u.top] for u in users] == [["a2", "a3"], ["b1"], []]
    assert [len(p.comments.cached) for p in users[0].top] == [2, 0]
    second = Post.objects.order_by(Post.created_at, Post.author.name)[1:2]
    users = await User.objects.prefetch_related(Prefetch(User.posts, second)).order_by(User.id)
    assert [[p.title for p in u.posts.cached] for u in users] == [["a2"], [], []]


async def test_prefetch_with_select_related_and_nested_queryset(clean):
    await seed()
    users = await User.objects.prefetch_related(
        Prefetch(User.comments, Comment.objects.select_related(Comment.post.author)),
        Prefetch(User.posts, Post.objects.prefetch_related(Prefetch(Post.comments, Comment.objects.filter(Comment.author_id.is_null())))),
    ).order_by(User.id)
    assert [(x.post.title, x.post.author.name) for x in users[1].comments.cached] == [("a2", "Alice")]
    assert [[x.body for x in p.comments.cached] for p in users[0].posts.cached] == [[], ["c2"], []]


async def test_prefetch_errors(clean):
    with pytest.raises(ValueError, match="different query sets"):
        User.objects.prefetch_related(Prefetch(User.posts, Post.objects.all()), Prefetch(User.posts, Post.objects.all()))
    with pytest.raises(TypeError, match="query set of Post"):
        Prefetch(User.posts, Comment.objects.all())  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="does not start at User"):
        User.objects.prefetch_related(Post.comments)


# -- instances built in Rust -----------------------------------------------------------------------


async def test_instances_get_their_database(clean):
    await seed()
    db = orm.get_database()
    users = await User.objects.using(db).prefetch_related(User.posts).select_related()
    assert all(u.__dict__["_db"] is db for u in users)
    assert all(p.__dict__["_db"] is db for u in users for p in u.posts.cached)
    rows = await User.objects.using(db).select(User, User.name)
    assert rows[0][0].__dict__["_db"] is db
    new = await User.objects.using(db).insert(email="z@x.io", name="Z")
    assert new.__dict__["_db"] is db
    assert "_db" not in (await User.objects.first()).__dict__


async def test_prefetch_query_reading_a_cte(clean):
    await seed()
    best = Post.objects.select(Post.id).filter(Post.views >= 50).cte("best")
    users = await User.objects.prefetch_related(
        Prefetch(User.posts, Post.objects.filter(Post.id.in_(best.select(best.c.id))), to_attr="best")
    ).order_by(User.id)
    assert [[p.title for p in u.best] for u in users] == [["a2"], ["b1"], []]


async def test_correlated_in_subquery(clean):
    await seed()
    commented = Comment.objects.filter(Comment.author_id == outer(User.id)).select(Comment.post_id)
    rows = await User.objects.filter(exists(Post.objects.filter(Post.id.in_(commented)))).order_by(User.id)
    assert [u.name for u in rows] == ["Alice", "Bob"]


# -- prefetch over many parents: keys split across queries ----------------------------------------


@pytest.fixture
async def small_params(clean):
    """A second connection whose statements take at most 5 parameters, so prefetches
    split their keys after a few parents."""
    db = await orm.connect(DATABASE_URL, max_connections=2, default=False, _disable=("max_params=5",))
    yield db
    await db.close()


async def test_max_params_option_is_checked(clean):
    with pytest.raises(orm.QueryError, match="invalid"):
        await orm.connect(DATABASE_URL, default=False, _disable=("max_params=abc",))


async def test_prefetch_splits_keys(small_params):
    db = small_params
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"u{i}"} for i in range(12)])
    posts = await Post.objects.insert_many(
        [{"author": u, "title": f"{u.name}-{j}", "body": "", "views": j} for u in users for j in range(3)]
    )
    await Comment.objects.insert_many([{"post": p, "body": f"on {p.title}"} for p in posts[::2]])

    def shape(us):
        return [[(p.title, [c.body for c in p.comments.cached]) for p in u.posts.cached] for u in us]

    qs = User.objects.prefetch_related(User.posts.comments).order_by(User.id)
    assert shape(await qs.using(db)) == shape(await qs)
    assert sum(len(u.posts.cached) for u in await qs.using(db)) == 36

    # A slice per parent still holds: each parent's rows stay in one query.
    top = Prefetch(User.posts, Post.objects.filter(Post.views >= 0).order_by(Post.views.desc())[:2], to_attr="top")
    got = await User.objects.prefetch_related(top).order_by(User.id).using(db)
    assert [[p.views for p in u.top] for u in got] == [[2, 1]] * 12

    # To-one, with repeated keys.
    cs = await Comment.objects.prefetch_related(Comment.post.author).order_by(Comment.id).using(db)
    names = {u.id: u.name for u in users}
    assert [c.post.author.name for c in cs] == [names[p.author_id] for p in posts[::2]]


async def test_prefetch_beyond_the_parameter_limit(clean):
    # 70 000 parents: more keys than Postgres takes parameters in one statement.
    db = orm.get_database()
    await db.execute("INSERT INTO users (email, name) SELECT 'u' || g || '@x.io', 'u' FROM generate_series(1, 70000) g")
    await db.execute("INSERT INTO posts (author_id, title, body) SELECT id, 't', '' FROM users WHERE id % 10000 = 0")
    users = await User.objects.prefetch_related(User.posts)
    assert len(users) == 70000
    assert sum(len(u.posts.cached) for u in users) == 7
