"""Decimal, enum and array columns, one-to-one and many-to-many relations, against
Postgres (blog example: Profile, Tag, PostTag)."""

from decimal import Decimal

import pytest
from blog.models import Post, PostTag, Priority, Profile, Role, Tag, User

import orm
from orm import func


async def users():
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    return alice, bob


# -- decimals -----------------------------------------------------------------------------


async def test_decimal_values_are_exact(clean):
    alice, bob = await users()
    p = await Profile.objects.insert(user=alice, balance=Decimal("10.25"))
    assert p.balance == Decimal("10.25") and isinstance(p.balance, Decimal)
    q = await Profile.objects.insert(user=bob)
    assert q.balance == Decimal("0.00")  # numeric(12, 2) keeps its scale
    # ints, decimal strings and floats are accepted
    await q.update(balance="0.10")
    assert q.balance == Decimal("0.10")
    await q.update(balance=Profile.balance + Decimal("0.20"))
    assert q.balance == Decimal("0.30")  # 0.1 + 0.2, exactly
    await q.update(balance=3)
    assert await Profile.objects.filter(Profile.balance > Decimal("5")).count() == 1
    assert await Profile.objects.filter(Profile.balance.in_([3, Decimal("10.25")])).count() == 2
    total = await Profile.objects.select(func.sum(Profile.balance)).scalar()
    assert total == Decimal("13.25")
    avg = await Profile.objects.select(func.avg(Profile.balance)).scalar()
    assert isinstance(avg, Decimal) and avg == Decimal("6.625")
    with pytest.raises(TypeError, match="finite decimal"):
        await q.update(balance="lots")
    # values numeric(12, 2) can't hold are the database's error
    with pytest.raises(orm.DatabaseError):
        await q.update(balance=Decimal("1e20"))


async def test_decimal_wire_format_round_trips(clean):
    # SUM over no rows is NULL, so COALESCE gives the parameter back: any precision
    for text in ["0", "-1", "12345678901234567890.123456789", "0.000001", "-0.5", "100000000", "1E+3"]:
        back = await Profile.objects.select(func.coalesce(func.sum(Profile.balance), Decimal(text))).scalar()
        assert back == Decimal(text)
    alice, _ = await users()
    await Profile.objects.insert(user=alice)
    for text in ["1234567890.12", "-0.01", "0.50", "-99999999.99"]:
        p = await Profile.objects.update(balance=Decimal(text)).returning()
        assert p[0].balance == Decimal(text) and str(p[0].balance) == text


# -- enums --------------------------------------------------------------------------------


async def test_native_enum(clean):
    alice, bob = await users()
    a = await Profile.objects.insert(user=alice)
    assert a.role is Role.member  # the default, read back as the member
    b = await Profile.objects.insert(user=bob, role="admin")  # stored values are accepted
    assert b.role is Role.admin
    assert [p.user_id for p in await Profile.objects.filter(Profile.role == Role.admin)] == [bob.id]
    assert await Profile.objects.filter(Profile.role.in_([Role.member, Role.editor])).count() == 1
    await a.update(role=Role.editor)
    assert a.role is Role.editor
    rows = await Profile.objects.select(Profile.role).order_by(Profile.role)
    assert [r.role for r in rows] == [Role.editor, Role.admin]  # the enum's order
    with pytest.raises(orm.DatabaseError, match="invalid input value for enum"):
        await a.update(role="owner")


async def test_int_enum(clean):
    t = await Tag.objects.insert(name="news")
    assert t.priority is Priority.normal
    hot = await Tag.objects.insert(name="hot", priority=Priority.high)
    assert hot.priority == 3 and hot.priority is Priority.high
    assert [x.name for x in await Tag.objects.filter(Tag.priority > Priority.normal)] == ["hot"]
    assert await Tag.objects.select(func.max(Tag.priority)).scalar() is Priority.high
    with pytest.raises(orm.IntegrityError):
        await Tag.objects.insert(name="bad", priority=7)


# -- arrays -------------------------------------------------------------------------------


async def test_array_columns(clean):
    alice, bob = await users()
    a = await Profile.objects.insert(user=alice, links=["https://a.example", "https://b.example"])
    b = await Profile.objects.insert(user=bob)
    assert a.links == ["https://a.example", "https://b.example"] and b.links == []
    assert await Profile.objects.filter(Profile.links.has("https://a.example")).count() == 1
    assert await Profile.objects.filter(Profile.links.has_all(["https://a.example", "https://x"])).count() == 0
    assert await Profile.objects.filter(Profile.links.has_any(["https://x", "https://b.example"])).count() == 1
    assert await Profile.objects.filter(Profile.links.contained_by(["https://x"])).count() == 1  # the empty one
    assert await Profile.objects.filter(Profile.links == []).count() == 1
    await b.update(links=["one", None])
    assert b.links == ["one", None]
    n = await Profile.objects.select(func.cardinality(Profile.links)).order_by(Profile.id).scalars()
    assert n == [2, 2]
    with pytest.raises(TypeError, match="expected a list"):
        await b.update(links="one")
    rows = await Profile.objects.select(Profile.links[1].label("first"), Profile.links[3].label("third")).order_by(Profile.id)
    assert [tuple(r) for r in rows] == [("https://a.example", None), ("one", None)]
    assert await Profile.objects.filter(Profile.links[2] == "https://b.example").count() == 1
    assert sorted(await Profile.objects.select(func.unnest(Profile.links)).scalars(), key=repr) == ["https://a.example", "https://b.example", "one", None]


# -- one-to-one ---------------------------------------------------------------------------


async def test_has_one(clean):
    alice, bob = await users()
    p = await Profile.objects.insert(user=alice, role=Role.admin)
    with pytest.raises(orm.NotLoaded):
        alice.profile
    us = await User.objects.load(User.profile).order_by(User.id)
    assert us[0].profile == p and us[1].profile is None
    us = await User.objects.load(User.profile.objects.as_prefetch()).order_by(User.id)
    assert us[0].profile == p and us[1].profile is None
    assert us[0].profile.user is us[0]  # the back side is filled in too
    assert [u.name for u in await User.objects.filter(User.profile.role == Role.admin)] == ["Alice"]
    assert [u.name for u in await User.objects.exclude(User.profile.role == Role.admin)] == ["Bob"]
    rows = await User.objects.select(User.name, User.profile.role).order_by(User.id)
    assert [tuple(r) for r in rows] == [("Alice", Role.admin), ("Bob", None)]
    with pytest.raises(orm.IntegrityError):
        await Profile.objects.insert(user=alice)  # one profile per user


# -- many-to-many -------------------------------------------------------------------------


async def blog():
    alice, bob = await users()
    p1, p2, p3 = await Post.objects.insert_many(
        [
            {"author": alice, "title": "one", "body": "."},
            {"author": alice, "title": "two", "body": "."},
            {"author": bob, "title": "three", "body": "."},
        ]
    ).returning()
    news, rust, py = await Tag.objects.insert_many([{"name": "news"}, {"name": "rust"}, {"name": "python"}]).returning()
    await p1.tags.add(news, rust)
    await p2.tags.add(rust.id)  # keys work too
    await p2.tags.add(rust)  # existing links are left alone
    return (p1, p2, p3), (news, rust, py)


async def test_many_to_many_links_and_queries(clean):
    (p1, p2, p3), (news, rust, py) = await blog()
    assert await PostTag.objects.count() == 3
    assert [t.name for t in await p1.tags.order_by(Tag.name)] == ["news", "rust"]
    assert [p.title for p in await rust.posts.order_by(Post.id)] == ["one", "two"]
    assert await p3.tags.count() == 0
    # filters follow both hops, one EXISTS per filter() call
    assert [p.title for p in await Post.objects.filter(Post.tags.name == "rust").order_by(Post.id)] == ["one", "two"]
    both = Post.objects.filter(Post.tags.name == "rust").filter(Post.tags.name == "news")
    assert [p.title for p in await both] == ["one"]
    assert await Post.objects.filter(Post.tags.name == "rust", Post.tags.name == "news").count() == 0
    assert [p.title for p in await Post.objects.exclude(Post.tags.name == "rust")] == ["three"]
    assert [u.name for u in await User.objects.filter(User.posts.tags.name == "news")] == ["Alice"]
    # aggregates over the relation
    rows = await Post.objects.select(Post.title, func.count(Post.tags)).order_by(Post.id)
    assert [tuple(r) for r in rows] == [("one", 2), ("two", 1), ("three", 0)]
    assert [t.name for t in await Tag.objects.filter(func.count(Tag.posts) == 0)] == ["python"]

    # unlinking
    assert await p1.tags.remove(news) == 1
    assert await p1.tags.remove(news) == 0
    await p1.tags.set([news, py])
    assert sorted(t.name for t in await p1.tags) == ["news", "python"]
    assert await p1.tags.clear() == 2
    assert await p1.tags.count() == 0
    # insert and link
    t = await p3.tags.insert(name="go")
    assert [x.name for x in await p3.tags] == ["go"] and t.priority is Priority.normal
    with pytest.raises(TypeError, match="links Tag"):
        await p3.tags.add(p1)


async def test_many_to_many_prefetch(clean):
    (p1, p2, p3), (news, rust, py) = await blog()
    posts = await Post.objects.load(Post.tags).order_by(Post.id)
    assert [[t.name for t in p.tags.cached] for p in posts] == [["news", "rust"], ["rust"], []]
    assert [t.name for t in await posts[1].tags] == ["rust"]  # served from the prefetched rows
    # nested, both directions
    tags = await Tag.objects.load(Tag.posts.author).order_by(Tag.id)
    assert [[(p.title, p.author.name) for p in t.posts.cached] for t in tags] == [
        [("one", "Alice")],
        [("one", "Alice"), ("two", "Alice")],
        [],
    ]
    # a slice applies per parent
    top = Post.tags.objects.order_by(Tag.name.desc())[:1].label("first_tag")
    posts = await Post.objects.load(top).order_by(Post.id)
    assert [[t.name for t in p.first_tag] for p in posts] == [["rust"], ["rust"], []]
    # filtered
    only = Post.tags.objects.filter(Tag.name != "rust")
    posts = await Post.objects.load(only).order_by(Post.id)
    assert [[t.name for t in p.tags.cached] for p in posts] == [["news"], [], []]
    # links changed: the prefetched rows are dropped
    await posts[0].tags.add(py)
    with pytest.raises(orm.NotLoaded):
        posts[0].tags.cached
