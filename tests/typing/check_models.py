"""Static typing checks for the generated stubs; run by tests/test_typing.py with mypy
and pyright. Never executed."""

from datetime import datetime, timedelta, timezone
from typing import assert_type

from blog.models import Comment, Post, User, UserQuerySet

from orm import ColumnRef, Condition, RelatedSet


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
    created = await User.objects.create(email="a@b.c", name="A")
    assert_type(created, User)
    await Post.objects.filter(Post.id == 1).update(views=Post.views + 1)
    async for p in Post.objects.order_by(Post.created_at.desc())[:10]:
        assert_type(p, Post)


async def errors() -> None:
    User.email < 1  # E: ordering a str column against an int
    User.posts.nope  # E: no such column
    User.objects.filter(User.posts.views.like("x"))  # E: like on int column
    await User.objects.create(email="a@b.c")  # E: missing name
    await Post.objects.update(views="many")  # E: wrong type
    u = User(email="a", name="b")
    u.posts = []  # E: read-only relation
