"""End-to-end tests against Postgres (python -> PyO3 -> planner -> tokio-postgres -> Postgres)."""

import asyncio
import contextvars
import warnings
from datetime import datetime, timedelta, timezone

import pytest
from blog.models import Comment, Post, User

import orm
from conftest import DATABASE_URL

NOW = datetime.now(timezone.utc)
YESTERDAY = NOW - timedelta(days=1)
LAST_WEEK = NOW - timedelta(days=7)


async def seed():
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    carol = await User.objects.insert(email="carol@example.com", name="Carol")
    a1, a2, b1 = await Post.objects.insert_many(
        [
            {"author": alice, "title": "old draft", "body": "...", "created_at": LAST_WEEK, "views": 5},
            {"author": alice, "title": "new post", "body": "...", "published": True, "views": 50},
            {"author": bob, "title": "bob's old", "body": "...", "created_at": LAST_WEEK, "published": True, "views": 100},
        ]
    )
    await Comment.objects.insert_many(
        [
            {"post": a2, "author": bob, "body": "nice, 100% agree"},
            {"post": a2, "author": None, "body": "anonymous"},
            {"post": b1, "author": alice, "body": "hello"},
        ]
    )
    return alice, bob, carol, (a1, a2, b1)


def names(users):
    return sorted(u.name for u in users)


async def test_insert_returns_row_with_server_defaults(clean):
    u = await User.objects.insert(email="x@example.com", name="X")
    assert isinstance(u.id, int)
    assert u.created_at.tzinfo is not None
    p = await Post.objects.insert(author=u, title="t", body="b")
    assert p.author_id == u.id
    assert p.views == 0 and p.published is False  # DDL defaults, read back via RETURNING


async def test_insert_many_mixed_columns(clean):
    u = await User.objects.insert(email="x@example.com", name="X")
    posts = await Post.objects.insert_many(
        [
            {"author_id": u.id, "title": "a", "body": "b"},
            {"author_id": u.id, "title": "c", "body": "d", "views": 7, "created_at": LAST_WEEK},
        ]
    )
    assert [(p.title, p.views) for p in posts] == [("a", 0), ("c", 7)]
    assert posts[1].created_at == LAST_WEEK and posts[0].created_at > LAST_WEEK
    assert await Post.objects.insert_many([]) == []


async def test_insert_validation(clean):
    with pytest.raises(ValueError, match="User.name is required"):
        User.objects.insert(email="x@example.com")
    with pytest.raises(TypeError, match="no field 'nope'"):
        User.objects.insert(email="x@example.com", name="X", nope=1)
    with pytest.raises(TypeError, match="plain values"):
        Post.objects.insert(author_id=1, title=Post.body, body="b")


async def test_upsert(clean):
    u = await User.objects.insert(email="a@example.com", name="A")
    same = await User.objects.insert(email="a@example.com", name="A2").on_conflict(User.email).do_update()
    assert (same.id, same.name, same.created_at) == (u.id, "A2", u.created_at)
    skipped = await User.objects.insert(email="a@example.com", name="A3").on_conflict(User.email).do_nothing()
    assert skipped is None
    rows = await User.objects.insert_many(
        [{"email": "a@example.com", "name": "A4"}, {"email": "b@example.com", "name": "B"}]
    ).on_conflict(User.email).do_nothing()
    assert [r.email for r in rows] == ["b@example.com"]
    rows = await User.objects.insert_many(
        [{"email": "a@example.com", "name": "A5"}, {"email": "b@example.com", "name": "B2"}]
    ).on_conflict(User.email).do_update(User.name)
    assert [r.name for r in rows] == ["A5", "B2"]
    assert await User.objects.count() == 2


async def test_unawaited_insert_warns(clean):
    with pytest.warns(RuntimeWarning, match="never awaited"):
        User.objects.insert(email="x@example.com", name="X")
        import gc

        gc.collect()
    assert await User.objects.count() == 0


async def test_instances_are_read_only(clean):
    u = await User.objects.insert(email="x@example.com", name="X")
    with pytest.raises(AttributeError, match=r"await obj.update\(name=...\)"):
        u.name = "Y"
    with pytest.raises(AttributeError, match="read-only"):
        u.posts = []
    with pytest.raises(TypeError, match="User.objects.insert"):
        User(email="x", name="y")


async def test_filter_across_to_many_autojoins(clean):
    await seed()
    # The motivating example: users with a post created before yesterday.
    users = await User.objects.filter(User.posts.created_at < YESTERDAY)
    assert names(users) == ["Alice", "Bob"]  # no duplicates, Carol has no posts


async def test_same_row_vs_independent_filters(clean):
    await seed()
    # One filter() call: the same post must be old AND unpublished.
    same = await User.objects.filter(User.posts.created_at < YESTERDAY, User.posts.published == False)  # noqa: E712
    assert names(same) == ["Alice"]
    # Two calls: an old post, and an unpublished post (possibly different ones).
    indep = await User.objects.filter(User.posts.created_at < YESTERDAY).filter(User.posts.views > 40)
    assert names(indep) == ["Alice", "Bob"]
    both_same = await User.objects.filter(User.posts.created_at < YESTERDAY, User.posts.views > 40)
    assert names(both_same) == ["Bob"]


async def test_exclude_and_negation(clean):
    await seed()
    # Users without any unpublished post (Carol has no posts at all).
    assert names(await User.objects.exclude(User.posts.published == False)) == ["Bob", "Carol"]  # noqa: E712
    assert names(await User.objects.filter(~(User.email == "bob@example.com"))) == ["Alice", "Carol"]


async def test_or_with_local_column(clean):
    await seed()
    users = await User.objects.filter((User.name == "Carol") | (User.posts.views >= 100))
    assert names(users) == ["Bob", "Carol"]


async def test_nested_and_reverse_paths(clean):
    await seed()
    # Users whose posts got a comment containing a literal '%'.
    assert names(await User.objects.filter(User.posts.comments.body.contains("100%"))) == ["Alice"]
    # Posts commented on by Bob, via comment -> author.
    posts = await Post.objects.filter(Post.comments.author.name == "Bob")
    assert [p.title for p in posts] == ["new post"]
    # To-one hop in a filter.
    assert len(await Post.objects.filter(Post.author.email.endswith("@example.com"))) == 3


async def test_select_related(clean):
    await seed()
    comments = await Comment.objects.select_related(Comment.post.author, Comment.author).order_by(Comment.id)
    first, anon, _ = comments
    assert first.post.title == "new post"
    assert first.post.author.name == "Alice"
    assert first.author.name == "Bob"
    assert anon.author is None  # nullable FK, LEFT JOIN with no match


async def test_unloaded_to_one_raises(clean):
    await seed()
    post = await Post.objects.first()
    with pytest.raises(orm.NotLoaded, match="select_related"):
        post.author  # noqa: B018
    c = await Comment.objects.filter(Comment.author_id == None).get()  # noqa: E711
    assert c.author is None  # null FK needs no loading


async def test_prefetch_related(clean):
    await seed()
    users = await User.objects.prefetch_related(User.posts).order_by(User.name)
    assert [len(u.posts.cached) for u in users] == [2, 1, 0]
    alice = users[0]
    assert [p.title for p in await alice.posts] == ["old draft", "new post"]
    assert alice.posts.cached[0].author is alice  # reverse relation filled in


async def test_related_set_queries_and_insert(clean):
    alice, *_ = await seed()
    with pytest.raises(orm.NotLoaded):
        alice.posts.cached  # noqa: B018
    assert len(await alice.posts) == 2
    assert await alice.posts.filter(Post.published).count() == 1
    p = await alice.posts.insert(title="via relation", body="b")
    assert p.author_id == alice.id
    more = await alice.posts.insert_many([{"title": "x", "body": "y"}, {"title": "z", "body": "w"}])
    assert {q.author_id for q in more} == {alice.id}
    assert await alice.posts.count() == 5


async def test_terminal_methods(clean):
    await seed()
    assert await User.objects.count() == 3
    assert await User.objects.filter(User.posts.views > 1000).exists() is False
    assert (await User.objects.first()).name == "Alice"
    assert (await User.objects.last()).name == "Carol"
    assert (await User.objects.order_by(User.name.desc()).first()).name == "Carol"
    assert await User.objects.filter(User.name == "Nobody").first() is None
    assert (await User.objects.get(User.email == "bob@example.com")).name == "Bob"
    with pytest.raises(User.DoesNotExist):
        await User.objects.get(User.email == "nobody@example.com")
    with pytest.raises(orm.MultipleObjectsReturned):
        await User.objects.get(User.name.startswith(""))
    assert await User.objects.all()[1:].count() == 2
    assert [u.name async for u in User.objects.order_by(User.id)[1:3]] == ["Bob", "Carol"]


async def test_update_and_delete(clean):
    alice, bob, carol, (a1, a2, b1) = await seed()
    n = await Post.objects.filter(Post.author.name == "Alice").update(views=Post.views + 1)
    assert n == 2
    assert sorted(p.views for p in await alice.posts) == [6, 51]

    # Instance update writes only the given fields and refreshes from RETURNING, so
    # a1 (still holding views=5 in memory) picks up the 6 from the bulk update above.
    await a1.update(title="renamed")
    assert (a1.title, a1.views) == ("renamed", 6)
    await a1.update(views=Post.views * 10)
    assert a1.views == 60

    await b1.update(author=alice)
    assert b1.author_id == alice.id
    assert {p.title for p in await alice.posts} == {"renamed", "new post", "bob's old"}

    # ON DELETE SET NULL keeps Bob's comment, anonymised.
    await bob.delete()
    assert await Comment.objects.filter(Comment.author_id == None).count() == 2  # noqa: E711
    with pytest.raises(User.DoesNotExist):
        await bob.update(name="ghost")
    assert await Post.objects.filter(Post.views > 90).delete() == 1  # b1
    # ON DELETE CASCADE removes Alice's remaining posts and their comments.
    await alice.delete()
    assert await Post.objects.count() == 0
    assert await Comment.objects.count() == 0

    await carol.update(name="Caroline")
    await carol.refresh()
    assert carol.name == "Caroline"


async def test_update_and_delete_returning(clean):
    alice, bob, carol, (a1, a2, b1) = await seed()
    posts = await Post.objects.filter(Post.author_id == alice.id).update(views=Post.views + 1).returning()
    assert sorted(p.views for p in posts) == [6, 51]
    assert await Post.objects.filter(Post.views > 1000).update(views=0).returning() == []
    assert await Post.objects.update().returning() == []  # nothing to set: no SQL

    gone = await Post.objects.filter(Post.views > 90).delete().returning()
    assert [p.title for p in gone] == ["bob's old"]
    assert await Post.objects.count() == 2

    # A statement that is never awaited warns, like an un-awaited coroutine.
    with pytest.warns(RuntimeWarning, match="never awaited"):
        Post.objects.update(views=0)
        import gc

        gc.collect()
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        await Post.objects.update(views=0)
    assert {p.views for p in await Post.objects} == {0}


async def test_upsert_with_expressions(clean):
    alice = await User.objects.insert(email="a@x.io", name="Alice")
    p = await Post.objects.insert(author=alice, title="t", body="b", views=3)
    again = {"id": p.id, "author": alice, "title": "t2", "body": "b", "views": 4}
    p2 = await Post.objects.insert(**again).on_conflict(Post.id).do_update(views=Post.views + orm.excluded(Post.views))
    assert (p2.id, p2.views, p2.title) == (p.id, 7, "t")  # only the given assignment ran
    p3 = await Post.objects.insert(**again).on_conflict(Post.id).do_update(Post.title, published=True)
    assert (p3.views, p3.title, p3.published) == (7, "t2", True)
    rows = [{**again, "views": 1}, {"author": alice, "title": "new", "body": "b", "views": 2}]
    out = await Post.objects.insert_many(rows).on_conflict(Post.id).do_update(views=orm.excluded(Post.views) * 100)
    assert sorted(o.views for o in out) == [2, 100]
    with pytest.raises(orm.QueryError):
        await Post.objects.update(views=orm.excluded(Post.views))


async def test_update_many(clean):
    alice, bob, carol, (a1, a2, b1) = await seed()
    rows = [
        {"id": a1.id, "title": "A1", "views": 1},
        {"id": a2.id, "title": "A2", "views": 2},
        {"id": b1.id, "title": "B1", "views": 3},
    ]
    assert await Post.objects.update_many(rows) == 3
    assert {(p.title, p.views) for p in await Post.objects} == {("A1", 1), ("A2", 2), ("B1", 3)}

    # The query set's filters still apply: Bob's post is left alone.
    rows = [{"id": a1.id, "views": 10}, {"id": b1.id, "views": 30}]
    assert await Post.objects.filter(Post.author_id == alice.id).update_many(rows) == 1
    assert await alice.posts.update_many([{"id": a2.id, "views": 20}, {"id": b1.id, "views": 30}]) == 1
    assert sorted(p.views for p in await Post.objects) == [3, 10, 20]

    # Relations, NULLs, RETURNING, unknown ids.
    out = await Comment.objects.update_many(
        [{"id": c.id, "author": None} for c in await Comment.objects]
    ).returning()
    assert len(out) == 3 and all(c.author_id is None for c in out)
    moved = await Post.objects.update_many([{"id": b1.id, "author": alice}, {"id": 999, "author": alice}]).returning()
    assert [(p.id, p.author_id) for p in moved] == [(b1.id, alice.id)]
    assert await Post.objects.update_many([]) == 0


async def test_update_many_batches_are_one_transaction(clean):
    alice, bob, carol, (a1, a2, b1) = await seed()
    rows = [{"id": a1.id, "views": 7}, {"id": a2.id, "views": 8}, {"id": b1.id, "views": -1}]
    with pytest.raises(orm.IntegrityError):  # views >= 0 fails in the third batch
        await Post.objects.update_many(rows, batch_size=1)
    assert sorted(p.views for p in await Post.objects) == [5, 50, 100]  # nothing kept
    assert len(await Post.objects.update_many(rows[:2], batch_size=1).returning()) == 2


async def test_fallback_sql_runs_on_postgres(clean):
    """The SQL used for databases without these features, run on Postgres."""
    alice, bob, carol, (a1, a2, b1) = await seed()
    db = await orm.connect(
        DATABASE_URL, default=False, max_connections=2, _disable=("update_from_values", "ilike", "returning")
    )
    try:
        rows = [{"id": a1.id, "title": "x", "views": 1}, {"id": b1.id, "title": "y", "views": 2}]
        assert await Post.objects.using(db).update_many(rows) == 2
        assert {(p.title, p.views) for p in await Post.objects.filter(Post.id.in_([a1.id, b1.id]))} == {
            ("x", 1),
            ("y", 2),
        }
        assert names(await User.objects.using(db).filter(User.name.icontains("ALI"))) == ["Alice"]
        with pytest.raises(orm.QueryError, match="does not support update"):
            await Post.objects.using(db).update(views=0).returning()
    finally:
        await db.close()


async def _in_new_tx(fn):
    """Run ``fn`` in a separate transaction (a task outside the caller's one)."""

    async def run():
        async with orm.get_database().transaction():
            return await fn()

    return await asyncio.create_task(run(), context=contextvars.Context())


async def test_row_locks(clean):
    alice, bob, carol, (a1, a2, b1) = await seed()
    db = orm.get_database()
    with pytest.raises(orm.TransactionRequired):
        await User.objects.lock().get(User.id == alice.id)
    with pytest.raises(orm.QueryError):
        await User.objects.lock().count()

    async with db.transaction():
        locked = await User.objects.lock().get(User.id == alice.id)
        assert locked == alice
        with pytest.raises(orm.LockNotAvailable):
            await _in_new_tx(lambda: User.objects.lock(nowait=True).get(User.id == alice.id))
        with pytest.raises(orm.LockNotAvailable):  # exclusive blocks shared too
            await _in_new_tx(lambda: User.objects.lock(False, nowait=True).get(User.id == alice.id))
        others = await _in_new_tx(lambda: User.objects.order_by(User.id).lock(skip_locked=True))
        assert names(others) == ["Bob", "Carol"]
        # Plain reads aren't blocked.
        assert (await _in_new_tx(lambda: User.objects.get(User.id == alice.id))).name == "Alice"

    async with db.transaction():
        await Post.objects.select_related(Post.author).lock(exclusive=False).filter(Post.id == a1.id)
        # Shared locks coexist; the joined author row isn't locked at all.
        assert len(await _in_new_tx(lambda: Post.objects.lock(False, nowait=True).filter(Post.id == a1.id))) == 1
        await _in_new_tx(lambda: User.objects.lock(nowait=True).get(User.id == alice.id))
        with pytest.raises(orm.LockNotAvailable):
            await _in_new_tx(lambda: Post.objects.lock(nowait=True).filter(Post.id == a1.id))


async def test_advisory_locks(clean):
    db = orm.get_database()
    with pytest.raises(orm.TransactionRequired):
        await db.lock("import")
    async with db.transaction():
        assert await db.lock("import") is True
        assert await _in_new_tx(lambda: db.lock("import", nowait=True)) is False
        assert await _in_new_tx(lambda: db.lock("other", nowait=True)) is True
        assert await _in_new_tx(lambda: db.lock(42, exclusive=False, nowait=True)) is True
    assert await _in_new_tx(lambda: db.lock("import", nowait=True)) is True  # released at commit


async def test_integrity_error(clean):
    await User.objects.insert(email="dup@example.com", name="A")
    with pytest.raises(orm.IntegrityError):
        await User.objects.insert(email="dup@example.com", name="B")


async def test_transactions(clean):
    db = orm.get_database()
    async with db.transaction():
        await User.objects.insert(email="in-tx@example.com", name="T")
    assert await User.objects.count() == 1

    with pytest.raises(RuntimeError):
        async with db.transaction():
            await User.objects.insert(email="rolled-back@example.com", name="R")
            assert await User.objects.count() == 2  # visible inside the transaction
            raise RuntimeError
    assert await User.objects.count() == 1

    async with db.transaction():
        await User.objects.insert(email="outer@example.com", name="O")
        with pytest.raises(orm.IntegrityError):
            async with db.transaction():  # savepoint
                await User.objects.insert(email="outer@example.com", name="dup")
        await User.objects.insert(email="after@example.com", name="A")
    assert await User.objects.count() == 3


async def test_concurrent_queries(clean):
    await seed()
    counts = await asyncio.gather(*(User.objects.filter(User.posts.views > i).count() for i in range(20)))
    assert counts[0] == 2 and counts[-1] == 2
