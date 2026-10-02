"""End-to-end tests against Postgres (python -> PyO3 -> SeaORM -> Postgres)."""

import asyncio
from datetime import datetime, timedelta, timezone

import pytest
from blog.models import Comment, Post, User

import orm

NOW = datetime.now(timezone.utc)
YESTERDAY = NOW - timedelta(days=1)
LAST_WEEK = NOW - timedelta(days=7)


async def seed():
    alice = await User.objects.create(email="alice@example.com", name="Alice")
    bob = await User.objects.create(email="bob@example.com", name="Bob")
    carol = await User.objects.create(email="carol@example.com", name="Carol")
    a1, a2, b1 = await Post.objects.bulk_create(
        [
            Post(author=alice, title="old draft", body="...", created_at=LAST_WEEK, views=5),
            Post(author=alice, title="new post", body="...", published=True, views=50),
            Post(author=bob, title="bob's old", body="...", created_at=LAST_WEEK, published=True, views=100),
        ]
    )
    await Comment.objects.bulk_create(
        [
            Comment(post=a2, author=bob, body="nice, 100% agree"),
            Comment(post=a2, author=None, body="anonymous"),
            Comment(post=b1, author=alice, body="hello"),
        ]
    )
    return alice, bob, carol, (a1, a2, b1)


def names(users):
    return sorted(u.name for u in users)


async def test_create_fills_server_defaults(clean):
    u = await User.objects.create(email="x@example.com", name="X")
    assert isinstance(u.id, int)
    assert u.created_at.tzinfo is not None
    p = Post(author=u, title="t", body="b")
    assert p.views == 0 and p.published is False  # Python-side literal defaults
    await p.save()
    assert p.id is not None and p.author_id == u.id


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


async def test_related_set_queries_and_create(clean):
    alice, *_ = await seed()
    with pytest.raises(orm.NotLoaded):
        alice.posts.cached  # noqa: B018
    assert len(await alice.posts) == 2
    assert await alice.posts.filter(Post.published).count() == 1
    p = await alice.posts.create(title="via relation", body="b")
    assert p.author_id == alice.id
    assert await alice.posts.count() == 3


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

    a1.title = "renamed"
    await a1.save()
    await a1.refresh()
    assert a1.title == "renamed" and a1.views == 6

    # Only assigned fields are written, so the concurrent views update above survives.
    b1.author_id = alice.id
    await b1.save()
    assert {p.title for p in await alice.posts} == {"renamed", "new post", "bob's old"}

    unsaved = User(email="later@example.com", name="Later")
    a2.author = unsaved
    await unsaved.save()
    await a2.save()  # picks up the key assigned after the relation was set
    assert (await Post.objects.select_related(Post.author).get(Post.id == a2.id)).author.name == "Later"

    # ON DELETE SET NULL keeps Bob's comment, anonymised.
    await bob.delete()
    assert await Comment.objects.filter(Comment.author_id == None).count() == 2  # noqa: E711
    # ON DELETE CASCADE removes Alice's posts and their comments.
    await alice.delete()
    assert [p.title for p in await Post.objects] == ["new post"]
    assert await Comment.objects.count() == 2
    assert await Post.objects.filter(Post.views > 10).delete() == 1


async def test_integrity_error(clean):
    await User.objects.create(email="dup@example.com", name="A")
    with pytest.raises(orm.IntegrityError):
        await User.objects.create(email="dup@example.com", name="B")


async def test_missing_required_field(clean):
    with pytest.raises(ValueError, match="User.name is required"):
        await User(email="x@example.com").save()


async def test_transactions(clean):
    db = orm.get_database()
    async with db.transaction():
        await User.objects.create(email="in-tx@example.com", name="T")
    assert await User.objects.count() == 1

    with pytest.raises(RuntimeError):
        async with db.transaction():
            await User.objects.create(email="rolled-back@example.com", name="R")
            assert await User.objects.count() == 2  # visible inside the transaction
            raise RuntimeError
    assert await User.objects.count() == 1

    async with db.transaction():
        await User.objects.create(email="outer@example.com", name="O")
        with pytest.raises(orm.IntegrityError):
            async with db.transaction():  # savepoint
                await User.objects.create(email="outer@example.com", name="dup")
        await User.objects.create(email="after@example.com", name="A")
    assert await User.objects.count() == 3


async def test_concurrent_queries(clean):
    await seed()
    counts = await asyncio.gather(*(User.objects.filter(User.posts.views > i).count() for i in range(20)))
    assert counts[0] == 2 and counts[-1] == 2
