"""Bulk writes: insert_many batches, partial-index upserts, get_or_insert, many-to-many
links with extra fields, COPY."""

import asyncio
from datetime import datetime, timezone
from decimal import Decimal

import pytest
from blog.models import Comment, Post, PostTag, Priority, Profile, Role, Tag, User

import orm
from conftest import DATABASE_URL

# -- insert_many batches ----------------------------------------------------------------------


async def test_insert_many_splits_by_the_parameter_limit(clean):
    # One field per row: 70 000 rows are 70 000 parameters, more than Postgres's 65 535.
    tags = await Tag.objects.insert_many([{"name": f"t{i}"} for i in range(70_000)])
    assert len(tags) == 70_000
    assert [t.name for t in tags[:2]] == ["t0", "t1"]
    assert tags[-1].name == "t69999"
    assert await Tag.objects.count() == 70_000


async def test_insert_many_batch_size(clean):
    # One statement can't update a row twice; one row per statement can.
    rows = [{"email": "a@x.io", "name": "A1"}, {"email": "a@x.io", "name": "A2"}]
    with pytest.raises(orm.DatabaseError, match="second time"):
        await User.objects.insert_many(rows).on_conflict(User.email).do_update()
    users = await User.objects.insert_many(rows, batch_size=1).on_conflict(User.email).do_update()
    assert [u.name for u in users] == ["A1", "A2"]
    assert (await User.objects.get(User.email == "a@x.io")).name == "A2"


async def test_insert_many_batches_run_in_one_transaction(clean):
    await Tag.objects.insert(name="taken")
    rows = [{"name": f"n{i}"} for i in range(5)] + [{"name": "taken"}]
    with pytest.raises(orm.IntegrityError):
        await Tag.objects.insert_many(rows, batch_size=2)
    assert await Tag.objects.count() == 1


async def test_insert_many_batches_by_max_params(clean):
    db = await orm.connect(DATABASE_URL, max_connections=2, default=False, _disable=("max_params=5",))
    try:
        rows = [{"email": "dup@x.io", "name": f"U{i}"} for i in range(3)]
        # Two parameters per row: two rows per statement, so the duplicate email
        # appears once in each statement.
        rows = [rows[0], {"email": "b@x.io", "name": "B"}, rows[1], {"email": "c@x.io", "name": "C"}]
        users = await User.objects.using(db).insert_many(rows).on_conflict(User.email).do_update()
        assert [u.name for u in users] == ["U0", "B", "U1", "C"]
    finally:
        await db.close()


async def test_insert_many_batch_size_is_checked(clean):
    with pytest.raises(ValueError, match="batch_size"):
        User.objects.insert_many([], batch_size=0)


# -- upserts on a partial unique index --------------------------------------------------------


@pytest.fixture
async def anon_index(clean):
    """One anonymous comment per post and body: a partial unique index."""
    await clean.execute("CREATE UNIQUE INDEX comments_anon_body ON comments (post_id, body) WHERE author_id IS NULL")
    yield clean
    await clean.execute("DROP INDEX comments_anon_body")


async def test_on_conflict_where_picks_a_partial_index(anon_index):
    alice = await User.objects.insert(email="a@x.io", name="A")
    post = await Post.objects.insert(author=alice, title="t", body="b")
    first = await Comment.objects.insert(post=post, body="hi")
    with pytest.raises(orm.DatabaseError, match="no unique or exclusion constraint"):
        await Comment.objects.insert(post=post, body="hi").on_conflict(Comment.post_id, Comment.body).do_nothing()
    where = Comment.author_id.is_null()
    assert await Comment.objects.insert(post=post, body="hi").on_conflict(Comment.post_id, Comment.body, where=where).do_nothing() is None
    later = datetime(2030, 1, 1, tzinfo=timezone.utc)
    again = await (
        Comment.objects.insert(post=post, body="hi", created_at=later)
        .on_conflict(Comment.post_id, Comment.body, where=where)
        .do_update(Comment.created_at)
    )
    assert again.id == first.id and again.created_at == later
    # The other rows of the index predicate's complement are not unique.
    await Comment.objects.insert_many([{"post": post, "author": alice, "body": "hi"}] * 2)
    assert await Comment.objects.count() == 3


async def test_on_conflict_where_with_set_parameters(anon_index):
    alice = await User.objects.insert(email="a@x.io", name="A")
    post = await Post.objects.insert(author=alice, title="t", body="b")
    await Comment.objects.insert(post=post, body="hi")
    row = await (
        Comment.objects.insert(post=post, body="hi")
        .on_conflict(Comment.post_id, Comment.body, where=Comment.author_id.is_null())
        .do_update(body="hi again")
    )
    assert row.body == "hi again"


async def test_on_conflict_where_reads_own_columns_only(anon_index):
    # Postgres refuses a relation path (a subquery) in an index predicate.
    with pytest.raises(orm.DatabaseError, match="subquery in index predicate"):
        await Comment.objects.insert(post_id=1, body="x").on_conflict(Comment.post_id, where=Comment.post.title == "t").do_nothing()


# -- get_or_insert ------------------------------------------------------------------------------


async def test_get_or_insert(clean):
    user, created = await User.objects.get_or_insert(email="a@x.io", defaults={"name": "A"})
    assert created and user.name == "A"
    again, created = await User.objects.get_or_insert(email="a@x.io", defaults={"name": "Other"})
    assert not created and again.id == user.id and again.name == "A"
    # A to-one relation gives its key field.
    profile, created = await Profile.objects.get_or_insert(user=user)
    assert created and profile.user_id == user.id
    same, created = await Profile.objects.get_or_insert(user=user)
    assert not created and same.id == profile.id


async def test_get_or_insert_is_safe_under_concurrency(clean):
    results = await asyncio.gather(
        *(User.objects.get_or_insert(email="race@x.io", defaults={"name": f"U{i}"}) for i in range(20))
    )
    assert sum(created for _, created in results) == 1
    assert len({u.id for u, _ in results}) == 1
    assert await User.objects.count() == 1


async def test_get_or_insert_checks_the_lookup(clean):
    with pytest.raises(ValueError, match="NULL never conflicts"):
        await Comment.objects.get_or_insert(author_id=None, defaults={"body": "x", "post_id": 1})
    with pytest.raises(TypeError, match="unique constraint"):
        await User.objects.get_or_insert(defaults={"name": "A"})
    with pytest.raises(orm.DatabaseError, match="no unique or exclusion constraint"):
        await User.objects.get_or_insert(name="A", defaults={"email": "a@x.io"})


# -- many-to-many add() with through_defaults -------------------------------------------------


async def test_add_with_through_defaults(clean):
    alice = await User.objects.insert(email="a@x.io", name="A")
    post = await Post.objects.insert(author=alice, title="t", body="b")
    t1, t2, t3 = await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}, {"name": "c"}])
    await post.tags.add(t1, t2, through_defaults={"position": 1})
    # An existing link keeps its values.
    await post.tags.add(t2, t3, through_defaults={"position": 2})
    links = await PostTag.objects.filter(PostTag.post_id == post.id).order_by(PostTag.tag_id)
    assert [(link.tag_id, link.position) for link in links] == [(t1.id, 1), (t2.id, 1), (t3.id, 2)]
    await post.tags.set([t1, t3, (t4 := await Tag.objects.insert(name="d"))], through_defaults={"position": 3})
    links = await PostTag.objects.filter(PostTag.post_id == post.id).order_by(PostTag.tag_id)
    assert [(link.tag_id, link.position) for link in links] == [(t1.id, 1), (t3.id, 2), (t4.id, 3)]


async def test_through_defaults_cannot_set_the_link_keys(clean):
    alice = await User.objects.insert(email="a@x.io", name="A")
    post = await Post.objects.insert(author=alice, title="t", body="b")
    tag = await Tag.objects.insert(name="a")
    for key in ("tag_id", "post_id", "post"):
        with pytest.raises(TypeError, match="link's key"):
            await post.tags.add(tag, through_defaults={key: 1})
    assert await PostTag.objects.count() == 0


# -- COPY -------------------------------------------------------------------------------------


async def test_copy_loads_many_rows(clean):
    alice = await User.objects.insert(email="a@x.io", name="A")
    rows = [{"author": alice, "title": f"t{i}", "body": "b", "views": i} for i in range(100_000)]
    assert await Post.objects.insert_many(rows, copy=True) == 100_000
    assert await Post.objects.count() == 100_000
    last = await Post.objects.get(Post.title == "t99999")
    assert last.views == 99_999 and last.published is False and last.created_at is not None


async def test_copy_column_types(clean):
    # A native enum, a decimal, a text array, and an int-stored enum.
    alice = await User.objects.insert(email="a@x.io", name="A")
    bob = await User.objects.insert(email="b@x.io", name="B")
    rows = [
        {"user": alice, "role": Role.admin, "balance": Decimal("12.50"), "links": ["x", "y"]},
        {"user": bob, "role": Role.member, "balance": Decimal("0"), "links": []},
    ]
    assert await Profile.objects.insert_many(rows, copy=True) == 2
    p = await Profile.objects.get(Profile.user_id == alice.id)
    assert (p.role, p.balance, p.links) == (Role.admin, Decimal("12.50"), ["x", "y"])
    assert await Tag.objects.insert_many([{"name": "a", "priority": Priority.high}], copy=True) == 1
    assert (await Tag.objects.get(Tag.name == "a")).priority == Priority.high


async def test_copy_stops_at_a_duplicate_key(clean):
    await Tag.objects.insert(name="taken")
    rows = [{"name": f"n{i}"} for i in range(1000)] + [{"name": "taken"}]
    with pytest.raises(orm.IntegrityError):
        await Tag.objects.insert_many(rows, copy=True)
    assert await Tag.objects.count() == 1


async def test_copy_in_a_transaction(clean):
    with pytest.raises(RuntimeError):
        async with clean.transaction():
            assert await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}], copy=True) == 2
            assert await Tag.objects.count() == 2
            raise RuntimeError
    assert await Tag.objects.count() == 0


async def test_copy_rejections(clean):
    with pytest.raises(TypeError, match="on_conflict"):
        Tag.objects.insert_many([{"name": "a"}], copy=True).on_conflict(Tag.name)
    with pytest.raises(TypeError, match="batch_size"):
        Tag.objects.insert_many([{"name": "a"}], copy=True, batch_size=2)  # type: ignore[call-overload]
    with pytest.raises(orm.QueryError, match="some rows only"):
        await Tag.objects.insert_many([{"name": "a"}, {"name": "b", "priority": Priority.low}], copy=True)
    assert await Tag.objects.insert_many([], copy=True) == 0
