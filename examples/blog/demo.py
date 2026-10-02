"""End-to-end tour of the API. Run from the repo root:

    python examples/blog/demo.py postgres://postgres:postgres@localhost/orm_test

Drops and recreates the users / posts / comments tables in that database.
"""

import asyncio
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from blog.models import Comment, Post, User  # noqa: E402

import orm  # noqa: E402


async def main(url: str) -> None:
    db = await orm.connect(url)
    await db.drop_tables()
    await db.create_tables()

    now = datetime.now(timezone.utc)
    yesterday = now - timedelta(days=1)

    # Writes are explicit statements: INSERT ... RETURNING gives back the stored rows.
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    old, new = await Post.objects.insert_many(
        [
            {"author": alice, "title": "Hello", "body": "...", "created_at": now - timedelta(days=3)},
            {"author": bob, "title": "Fresh", "body": "...", "published": True},
        ]
    )
    await new.comments.insert(body="first!", author=alice)
    # Upsert on the unique email.
    bob = await User.objects.insert(email="bob@example.com", name="Robert").on_conflict(User.email).do_update()
    print("upserted:", bob)

    # Filters follow relations; to-many hops compile to EXISTS (no duplicate rows).
    q = User.objects.filter(User.posts.created_at < yesterday)
    print(q.sql())
    print("posted before yesterday:", [u.name for u in await q])

    # Conditions in one filter() call must match the same post.
    print("same post old & published:", await User.objects.filter(
        User.posts.created_at < yesterday, User.posts.published == True  # noqa: E712
    ).count())

    # Eager loading: JOIN for to-one, one extra IN query for to-many.
    for c in await Comment.objects.select_related(Comment.post.author, Comment.author):
        print(f"{c.author.name if c.author else '?'} on {c.post.author.name}'s {c.post.title!r}: {c.body}")
    for u in await User.objects.prefetch_related(User.posts).order_by(User.name):
        print(u.name, [p.title for p in u.posts.cached])

    # Set-based UPDATE, and a single-row update that refreshes the instance.
    await Post.objects.filter(Post.author_id == alice.id).update(views=Post.views + 1)
    async with db.transaction():
        await old.update(title="Hello, world", views=Post.views + 10)
    print(old)  # views=11: the database's value, read back with RETURNING

    await db.drop_tables()
    await db.close()


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "postgres://postgres:postgres@localhost/orm_test"))
