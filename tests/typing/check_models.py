"""Static typing checks for the generated stubs; run by tests/test_typing.py with mypy
and pyright. Never executed."""

from datetime import datetime, timedelta, timezone
from typing import assert_type

from blog.models import Comment, Post, PostInsert, PostQuerySet, User, UserQuerySet

from orm import ColumnRef, Condition, RelatedSet, Row, excluded, func
from orm import get_database as orm_db


async def check() -> None:
    yesterday = datetime.now(timezone.utc) - timedelta(days=1)

    # Class access gives columns, relation paths continue into the related model.
    assert_type(User.email, ColumnRef[str])
    assert_type(User.posts.created_at, ColumnRef[datetime])
    assert_type(User.posts.author.email, ColumnRef[str])
    assert_type(Comment.author_id, ColumnRef[int | None])
    assert_type(User.posts.created_at < yesterday, Condition)

    qs = User.objects.filter(User.posts.created_at < yesterday)
    assert_type(qs, UserQuerySet)
    users = await qs
    assert_type(users, list[User])

    # Instance access gives values and related objects.
    u = users[0]
    assert_type(u.email, str)
    assert_type(u.posts, RelatedSet[Post])
    assert_type(await u.posts, list[Post])
    post = await Post.objects.select_related(Post.author).get(Post.id == 1)
    assert_type(post.author, User)
    c = await Comment.objects.first()
    if c is not None:
        assert_type(c.author, User | None)
    assert_type(await User.objects.count(), int)
    async for p in Post.objects.order_by(Post.created_at.desc())[:10]:
        assert_type(p, Post)

    # Writes are explicit statements.
    alice = await User.objects.insert(email="a@b.c", name="A")
    assert_type(alice, User)
    rows: list[PostInsert] = [{"author": alice, "title": "t", "body": "b"}]
    assert_type(await Post.objects.insert_many(rows), list[Post])
    upserted = await User.objects.insert(email="a@b.c", name="A2").on_conflict(User.email).do_update()
    assert_type(upserted, User)
    skipped = await User.objects.insert(email="a@b.c", name="A").on_conflict(User.email).do_nothing()
    assert_type(skipped, User | None)
    assert_type(await Post.objects.filter(Post.id == 1).update(views=Post.views + 1), int)
    assert_type(await Post.objects.filter(Post.id == 1).update(views=1).returning(), list[Post])
    assert_type(await Post.objects.filter(Post.id == 1).delete(), int)
    assert_type(await Post.objects.filter(Post.id == 1).delete().returning(), list[Post])
    bumped = await Post.objects.insert(author=alice, title="t", body="b").on_conflict(Post.id).do_update(
        views=Post.views + excluded(Post.views)
    )
    assert_type(bumped, Post)
    assert_type(await Post.objects.update_many([{"id": 1, "views": 2}]), int)
    assert_type(await Post.objects.update_many([{"id": 1, "author": alice}]).returning(), list[Post])
    grouped = await Post.objects.select(Post.author_id, func.count(), func.sum(Post.views)).group_by(Post.author_id)
    assert_type(grouped, list[Row[int, int, int | None]])
    aid, n, total = grouped[0]
    assert_type((aid, n, total), tuple[int, int, int | None])
    assert_type(grouped[0][1], int)
    grouped[0].anything  # untyped name access
    assert_type(await Post.objects.select(func.max(Post.views)).scalar(), int | None)
    assert_type(await Post.objects.select(Post.title).scalars(), list[str])
    pairs = await User.objects.select(User, func.count(User.posts))
    assert_type(pairs[0][0], User)
    async for batch in Post.objects.batches(100):
        assert_type(batch, list[Post])
    async for one in Post.objects.iterate():
        assert_type(one, Post)
    Post.objects.filter(Post.author_id.in_(User.objects.select(User.id)))
    assert_type(Post.objects.lock(exclusive=False, skip_locked=True), PostQuerySet)
    assert_type(await Post.objects.lock().get(Post.id == 1), Post)
    assert_type(await orm_db().lock("key", nowait=True), bool)
    await post.update(title="new", views=Post.views + 1)
    await alice.posts.insert(title="t", body="b")
    await post.delete()


async def errors() -> None:
    User.email < 1  # E: ordering a str column against an int
    User.posts.nope  # E: no such column
    User.objects.filter(User.posts.views.like("x"))  # E: like on int column
    await User.objects.insert(email="a@b.c")  # E: missing name
    await User.objects.insert(email="a@b.c", name="A", nope=1)  # E: unknown field
    await Post.objects.update(views="many")  # E: wrong type
    await Post.objects.update(nope=1).returning()  # E: unknown field
    await Post.objects.update_many([{"views": 2}])  # E: missing primary key
    await Post.objects.select(Post.id, Post.title).scalars()  # E: scalars() needs one column
    func.lower(Post.views)  # E: lower() of an int column
    await Post.objects.update_many([{"id": 1, "views": Post.views + 1}])  # E: no expressions
    u = await User.objects.get(User.id == 1)
    u.name = "B"  # E: instances are read-only
    await u.update(name=1)  # E: wrong type
    u.posts = []  # E: read-only relation
