"""SQL shape tests: no database needed."""

from datetime import datetime, timezone

import pytest
from blog.models import Comment, Post, User

from orm import QueryError, and_, excluded, or_

Y = datetime(2026, 10, 1, tzinfo=timezone.utc)
USER_COLS = 'SELECT "users"."id", "users"."email", "users"."name", "users"."created_at" FROM "users"'


def where(qs) -> str:
    sql = qs.sql()
    return sql.split(" WHERE ", 1)[1] if " WHERE " in sql else ""


def test_to_many_filter_is_exists():
    sql = User.objects.filter(User.posts.created_at < Y).sql()
    assert sql == (
        f"{USER_COLS} WHERE EXISTS(SELECT 1 FROM \"posts\" AS \"t1\" "
        "WHERE \"t1\".\"author_id\" = \"users\".\"id\" "
        "AND \"t1\".\"created_at\" < '2026-10-01 00:00:00.000000 +00:00')"
    )


def test_same_filter_call_means_same_related_row():
    w = where(User.objects.filter(User.posts.views > 10, User.posts.published == True))  # noqa: E712
    assert w.count("EXISTS") == 1
    assert '"t1"."views" > 10 AND "t1"."published" = TRUE' in w


def test_separate_filter_calls_are_independent():
    w = where(User.objects.filter(User.posts.views > 10).filter(User.posts.published == True))  # noqa: E712
    assert w.count("EXISTS") == 2


def test_or_inside_one_relation_shares_subquery():
    w = where(User.objects.filter((User.posts.views > 10) | (User.posts.title == "x")))
    assert w.count("EXISTS") == 1 and " OR " in w


def test_or_with_local_column_keeps_rows_without_related():
    w = where(User.objects.filter(or_(User.email == "a", User.posts.views > 10)))
    assert w.startswith('"users"."email" = \'a\' OR EXISTS(')


def test_exclude_is_not_exists():
    w = where(User.objects.exclude(User.posts.published == False))  # noqa: E712
    assert w.startswith("NOT EXISTS(")


def test_negation_stays_outside_subquery():
    w = where(User.objects.filter(and_(User.posts.views > 10, ~(User.posts.published == True))))  # noqa: E712
    assert w.count("EXISTS") == 2 and "NOT EXISTS" in w


def test_nested_relation_nests_subqueries():
    w = where(User.objects.filter(User.posts.comments.body.contains("50%")))
    assert w.count("EXISTS") == 2
    assert '"t2"."post_id" = "t1"."id"' in w
    assert "LIKE E'%50\\\\%%'" in w  # escaped % (debug rendering inlines values)


def test_to_one_filter():
    w = where(Post.objects.filter(Post.author.email == "a@b.c"))
    assert w == 'EXISTS(SELECT 1 FROM "users" AS "t1" WHERE "t1"."id" = "posts"."author_id" AND "t1"."email" = \'a@b.c\')'


def test_column_to_column_across_relation():
    w = where(Post.objects.filter(Post.comments.created_at < Post.created_at))
    assert '"t1"."created_at" < "posts"."created_at"' in w


def test_select_related_and_order_use_left_joins():
    sql = Comment.objects.select_related(Comment.post.author).order_by(Comment.post.title.desc()).sql()
    assert 'LEFT JOIN "posts" AS "j1" ON "j1"."id" = "comments"."post_id"' in sql
    assert 'LEFT JOIN "users" AS "j2" ON "j2"."id" = "j1"."author_id"' in sql
    assert sql.endswith('ORDER BY "j1"."title" DESC')


def test_slicing():
    assert Post.objects.all()[5:10].sql().endswith("LIMIT 5 OFFSET 5")
    assert Post.objects.all()[5:10][1:3].sql().endswith("LIMIT 2 OFFSET 6")


def test_null_comparisons():
    assert where(Comment.objects.filter(Comment.author_id == None)) == '"comments"."author_id" IS NULL'  # noqa: E711
    assert where(Comment.objects.filter(Comment.author_id != None)) == '"comments"."author_id" IS NOT NULL'  # noqa: E711


def test_in_and_empty_in():
    assert where(User.objects.filter(User.id.in_([1, 2]))) == '"users"."id" IN (1, 2)'
    assert where(User.objects.filter(User.id.in_([]))) == "FALSE"


def test_order_by_to_many_is_rejected():
    with pytest.raises(QueryError, match="to-one"):
        User.objects.order_by(User.posts.views).sql()


def test_column_from_other_model_is_rejected():
    with pytest.raises(ValueError, match="belongs to User"):
        Post.objects.filter(User.email == "x").sql()


def test_python_boolean_operators_are_rejected():
    with pytest.raises(TypeError, match="& | ~"):
        (User.email == "a") and (User.name == "b")  # noqa: B015


def test_unknown_relation_attribute():
    with pytest.raises(AttributeError, match="no field or relation 'nope'"):
        User.posts.nope  # noqa: B018


def test_lock():
    assert User.objects.lock().sql() == f'{USER_COLS} FOR UPDATE OF "users"'
    assert User.objects.lock(exclusive=False).sql().endswith('FOR SHARE OF "users"')
    assert User.objects.lock(nowait=True).sql().endswith('FOR UPDATE OF "users" NOWAIT')
    assert User.objects.lock(False, skip_locked=True).sql().endswith('FOR SHARE OF "users" SKIP LOCKED')
    # Only the model's rows: rows joined by select_related stay unlocked.
    sql = Post.objects.select_related(Post.author).filter(Post.views > 1)[:5].lock().sql()
    assert sql.endswith('LIMIT 5 FOR UPDATE OF "posts"')
    with pytest.raises(ValueError):
        User.objects.lock(nowait=True, skip_locked=True)


def test_lock_rejected_where_meaningless():
    with pytest.raises(QueryError):
        User.objects.lock().update(name="x")
    with pytest.raises(QueryError):
        User.objects.lock().delete()


def test_writes_validate_when_built():
    with pytest.raises(TypeError):
        User.objects.update(nope=1)
    with pytest.raises(QueryError):
        User.objects.all()[:3].delete()
    with pytest.raises(TypeError):
        excluded(User.posts.views)
    with pytest.raises(TypeError):
        User.objects.insert(email="a", name="b").on_conflict(User.email).do_update(User.name, name="x")
