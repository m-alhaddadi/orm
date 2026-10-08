"""Static typing checks for the generated stubs; run by tests/test_typing.py with mypy
and pyright. Never executed."""

from datetime import datetime, timedelta, timezone
from decimal import Decimal
from typing import Any, assert_type

from blog.models import Comment, Post, PostInsert, PostQuerySet, Priority, Profile, Role, Tag, User, UserQuerySet

from orm import (
    Case,
    ColumnRef,
    Expression,
    Func,
    Condition,
    ManyRelatedSet,
    Ordering,
    Page,
    Prefetch,
    Prepared,
    RelatedSet,
    Row,
    TsVector,
    Window,
    WindowDef,
    excluded,
    exists,
    func,
    outer,
    param,
    window,
)
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
    assert_type(-Post.created_at, Ordering)
    page = await Post.objects.order_by(-Post.created_at).paginate(first=20)
    assert_type(page, Page[Post])
    assert_type(page.next_cursor, str | None)
    Post.objects.order_by(-Post.created_at, Post.id)
    Post.objects.filter(-Post.published)  # E: an ordering is no condition
    -(Post.views + 1)  # E: only a column has a descending short form

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

    # in_bulk, subqueries, windows, CTEs, prefetch.
    assert_type(await Post.objects.in_bulk([1, 2]), dict[Any, Post])
    assert_type(exists(Post.objects.filter(Post.author_id == outer(User.id))), Condition)
    latest = Post.objects.filter(Post.author_id == outer(User.id)).select(Post.title)[:1].as_scalar()
    assert_type(await User.objects.select(User, latest), list[Row[User, str]])
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    assert_type(rank, Window[int])
    assert_type(func.lag(Post.views).over(order_by=Post.id), Window[int | None])
    ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
    assert_type(await Post.objects.from_(ranked).filter(ranked.c.rank <= 3), list[Post])
    await ranked.select(ranked.c.author_id).group_by(ranked.c.author_id)
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    assert_type(w, WindowDef)
    assert_type(func.sum(Post.views).over(w), Window[int | None])
    assert_type(func.row_number().over(w, rows=(None, 0)), Window[int])
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    assert_type(User.objects.join(totals, totals.c.author_id == User.id, outer=True), UserQuerySet)
    assert_type(
        await User.objects.prefetch_related(User.posts.comments, Prefetch(User.posts, Post.objects.all()[:3])),
        list[User],
    )

    # Decimal, enum and array columns; one-to-one and many-to-many relations.
    assert_type(Profile.balance, ColumnRef[Decimal])
    assert_type(User.profile.role, ColumnRef[Role])
    assert_type(Tag.priority, ColumnRef[Priority])
    assert_type(Profile.links, ColumnRef[list[str]])
    assert_type(Profile.links.has("x"), Condition)
    assert_type(Post.tags.name, ColumnRef[str])
    prof = await Profile.objects.insert(user=alice, balance=Decimal("1.50"), role=Role.admin, links=["a"])
    assert_type(prof.balance, Decimal)
    assert_type(prof.role, Role)
    assert_type(prof.links, list[str])
    assert_type(await Profile.objects.select(func.sum(Profile.balance)).scalar(), Decimal | None)
    assert_type(await Profile.objects.select(func.avg(Profile.balance)).scalar(), Decimal | None)
    assert_type(func.cardinality(Profile.links), Func[int])
    assert_type(Profile.links[1], Func[str | None])
    assert_type(func.unnest(Profile.links), Func[str])
    assert_type(func.concat(Post.title, " ", Post.views), Func[str])
    assert_type(Post.title.concat("!"), Expression[str])
    assert_type(func.strpos(Post.title, "x"), Func[int])
    Post.views.concat("!")  # E: concatenation takes strings
    assert_type(func.case((Post.published, 1), default=0), Case[int])
    assert_type(func.case((Post.views > 3, Post.title)), Case[str | None])
    assert_type(await Post.objects.select(func.case((Post.published, Post.views), default=0)).scalar(), int | None)
    assert_type(func.count(filter=Post.published), Func[int])
    assert_type(func.sum(Post.views, filter=Post.views > 3), Func[int | None])
    assert_type(func.to_tsvector("english", Post.title), Func[TsVector])
    assert_type(func.ts_rank(func.to_tsvector(Post.title), func.plainto_tsquery("dog")), Func[float])
    assert_type(func.to_tsvector(Post.title).matches("dog"), Condition)
    Post.title.matches("dog")  # E: matches() needs a tsvector
    assert_type(alice.profile, Profile | None)
    assert_type(post.tags, ManyRelatedSet[Tag])
    assert_type(await post.tags, list[Tag])
    await post.tags.add(await Tag.objects.get(Tag.id == 1))
    assert_type(await post.tags.remove(1), int)
    assert_type(await post.tags.insert(name="go"), Tag)
    assert_type(await Post.objects.prefetch_related(Post.tags), list[Post])

    # Prepared queries keep the model type.
    by_author = Post.objects.filter(Post.author_id == param("a")).limit(param("n")).prepare()
    assert_type(by_author, Prepared[Post])
    assert_type(await by_author(a=1, n=10), list[Post])
    assert_type(await by_author.get(a=1, n=10), Post)
    assert_type(await by_author.first(a=1, n=10), Post | None)
    assert_type(await by_author.count(a=1, n=10), int)


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
    Prefetch(User.posts, Comment.objects.all())  # E: query set of the wrong model
    func.ntile("2")  # E: buckets are ints
    func.sum(Post.views).over(window(), rows=(None, "x"))  # E: frame bounds are ints
    await Profile.objects.insert(user_id=1, role="boss")  # E: roles are Role members
    Post.views.has(1)  # E: not an array
    await Profile.objects.insert(user_id=1, balance=1.5)  # E: a Decimal
