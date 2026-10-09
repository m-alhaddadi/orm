"""Prepared queries (``qs.prepare()`` with ``orm.param``) and LIMIT / OFFSET as IR
parameters."""

import json

import pytest
from blog.models import Comment, Post, Profile, User

import orm
from orm import QueryError, param


async def seed():
    alice, bob = await User.objects.insert_many(
        [{"email": "alice@example.com", "name": "Alice"}, {"email": "bob@example.com", "name": "Bob"}]
    )
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "a1", "body": "", "views": 5},
            {"author": alice, "title": "a_2%", "body": "", "views": 50},
            {"author": alice, "title": "a3", "body": "", "views": 20},
            {"author": bob, "title": "b1", "body": "", "views": 100},
        ]
    )
    return (alice, bob), posts


# -- LIMIT / OFFSET as parameters ---------------------------------------------------------


def _ir(qs):
    params = []
    return json.dumps(qs._select_ir("select", params)), params


def test_limit_offset_are_parameters():
    a, pa = _ir(Post.objects.order_by(Post.id)[10:20])
    b, pb = _ir(Post.objects.order_by(Post.id)[30:35])
    assert a == b  # one IR document (and one SQL text) for every page
    assert pa == [10, 10] and pb == [5, 30]


def test_limit_offset_sql():
    assert Post.objects.all()[5:15].sql().endswith("LIMIT 10 OFFSET 5")


# -- prepared queries -------------------------------------------------------------------------


def test_prepared_sql_matches_the_plain_query():
    plain = Post.objects.filter(Post.author_id == 3, Post.title.contains("x_%")).order_by(Post.id)[5:15]
    prepared = (
        Post.objects.filter(Post.author_id == param("a"), Post.title.contains(param("t")))
        .order_by(Post.id)
        .limit(param("n"))
        .offset(param("o"))
        .prepare()
    )
    assert prepared.params == {"a", "t", "n", "o"}
    assert prepared.sql(a=3, t="x_%", n=10, o=5) == plain.sql()


async def test_prepared_reads(clean):
    (alice, bob), posts = await seed()
    by_author = Post.objects.filter(Post.author_id == param("author")).order_by(Post.id).prepare()
    assert [p.title for p in await by_author(author=alice.id)] == ["a1", "a_2%", "a3"]
    assert [p.title for p in await by_author(author=bob.id)] == ["b1"]
    assert await by_author.count(author=alice.id) == 3
    assert await by_author.exists(author=bob.id)
    assert not await by_author.exists(author=-1)
    assert (await by_author.first(author=alice.id)).title == "a1"
    assert await by_author.first(author=-1) is None

    by_id = Post.objects.load(Post.author).filter(Post.id == param("id")).prepare()
    p = await by_id.get(id=posts[3].id)
    assert p.title == "b1" and p.author.name == "Bob"
    with pytest.raises(Post.DoesNotExist):
        await by_id.get(id=-1)
    with pytest.raises(Post.MultipleObjectsReturned):
        await by_author.get(author=alice.id)


async def test_prepared_limit_offset_and_expressions(clean):
    (alice, _), _ = await seed()
    page = (
        Post.objects.filter(Post.views + param("bump") > param("min"))
        .order_by(Post.id)
        .limit(param("n"))
        .offset(param("skip"))
        .prepare()
    )
    assert [p.title for p in await page(bump=0, min=0, n=2, skip=0)] == ["a1", "a_2%"]
    assert [p.title for p in await page(bump=0, min=0, n=2, skip=2)] == ["a3", "b1"]
    assert [p.title for p in await page(bump=10, min=55, n=10, skip=0)] == ["a_2%", "b1"]
    assert await page.count(bump=0, min=0, n=3, skip=2) == 2
    # the slots of the prepared query are bound separately from its first() / count()
    assert (await page.first(bump=0, min=10, n=5, skip=1)).title == "a3"
    with pytest.raises(QueryError, match="non-negative integer"):
        await page(bump=0, min=0, n=-1, skip=0)


async def test_prepared_like_patterns_are_escaped_per_call(clean):
    await seed()
    q = Post.objects.filter(Post.title.contains(param("t"))).order_by(Post.id).prepare()
    assert [p.title for p in await q(t="_2%")] == ["a_2%"]
    assert [p.title for p in await q(t="1")] == ["a1", "b1"]
    starts = Post.objects.filter(Post.title.startswith(param("t"))).prepare()
    assert await starts.count(t="a") == 3
    assert await starts.count(t="a_") == 1


async def test_prepared_has_and_prefetch(clean):
    (alice, bob), _ = await seed()
    await Profile.objects.insert_many(
        [{"user": alice, "links": ["x", "y"]}, {"user": bob, "links": ["y"]}]
    )
    with_link = Profile.objects.filter(Profile.links.has(param("link"))).prepare()
    assert await with_link.count(link="x") == 1
    assert await with_link.count(link="y") == 2

    top = (
        User.objects.order_by(User.id)
        .load(
            User.posts.objects.filter(Post.views >= param("min")).order_by(Post.views.desc()).limit(param("k"))
        )
        .prepare()
    )
    users = await top(min=10, k=1)
    assert [[p.title for p in u.posts.cached] for u in users] == [["a_2%"], ["b1"]]
    users = await top(min=0, k=2)
    assert [[p.title for p in u.posts.cached] for u in users] == [["a_2%", "a3"], ["b1"]]


async def test_prepared_runs_in_the_current_transaction(clean):
    await seed()
    q = Post.objects.filter(Post.views > param("v")).lock().prepare()
    with pytest.raises(orm.TransactionRequired):
        await q(v=0)
    async with orm.get_database().transaction():
        assert len(await q(v=10)) == 3


def test_prepared_value_errors():
    q = Comment.objects.filter(Comment.author_id == param("a")).prepare()
    with pytest.raises(TypeError, match="missing values for a"):
        q.sql()
    with pytest.raises(TypeError, match="no param b"):
        q.sql(a=1, b=2)
    with pytest.raises(ValueError, match="can't be None"):
        q.sql(a=None)


def test_param_misuse():
    with pytest.raises(QueryError, match="prepare"):
        Post.objects.filter(Post.id == param("id")).sql()
    with pytest.raises(TypeError, match="in_"):
        Post.id.in_(param("ids"))
    with pytest.raises(QueryError, match="sliced"):
        Post.objects.limit(param("n"))[:5]
    with pytest.raises(ValueError, match="identifier"):
        param("not a name")
