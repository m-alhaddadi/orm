"""SQL shape tests: no database needed."""

from datetime import datetime, timezone

import pytest
from blog.models import Comment, Post, Profile, User

from orm import QueryError, and_, excluded, func, or_

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
        User.objects.insert(email="a", name="b").on_conflict(User.email, update=True, update_fields=[User.name], update_values={"name": "x"})


def native():
    return Post._meta.registry.native()


def test_update_many_joins_a_values_list():
    (sql,) = native().update_many_sql("Post", ["id", "title", "views"], [[1, "a", 10], [2, "b", 20]])
    assert sql == (
        'UPDATE "posts" SET "title" = "v"."column2", "views" = "v"."column3" '
        "FROM (VALUES (1, 'a', 10), (2, 'b', 20)) AS \"v\" WHERE \"posts\".\"id\" = \"v\".\"column1\""
    )


def test_update_many_falls_back_to_case():
    (sql,) = native().update_many_sql("Post", ["id", "views"], [[1, 10], [2, 20]], disable=["update_from_values"])
    assert sql == (
        'UPDATE "posts" SET "views" = (CASE WHEN ("posts"."id" = 1) THEN 10 '
        'WHEN ("posts"."id" = 2) THEN 20 END) WHERE "posts"."id" IN (1, 2)'
    )


def test_update_many_batches():
    rows = [[i, "x"] for i in range(5)]
    assert len(native().update_many_sql("Post", ["id", "title"], rows, batch_size=2)) == 3
    # Without batch_size, as many rows as Postgres' 65535 parameters allow.
    rows = [[i, i] for i in range(40_000)]
    assert len(native().update_many_sql("Post", ["id", "views"], rows)) == 2


def test_update_many_validation():
    with pytest.raises(ValueError, match="has no id"):
        Post.objects.update_many([{"title": "a"}])
    with pytest.raises(ValueError, match="appears twice"):
        Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 1, "title": "b"}])
    with pytest.raises(ValueError, match="same fields"):
        Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "views": 3}])
    with pytest.raises(ValueError, match="besides id"):
        Post.objects.update_many([{"id": 1}])
    with pytest.raises(TypeError, match="plain values"):
        Post.objects.update_many([{"id": 1, "views": Post.views + 1}])
    with pytest.raises(TypeError, match="no field"):
        Post.objects.update_many([{"id": 1, "nope": 1}])
    with pytest.raises(QueryError):
        Post.objects.all()[:2].update_many([{"id": 1, "views": 1}])
    with pytest.raises(ValueError):
        Post.objects.update_many([{"id": 1, "views": 1}], batch_size=0)


def test_select_group_by_having():
    sql = (
        Post.objects.filter(Post.published)
        .select(Post.author_id, func.count().label("n"), func.sum(Post.views))
        .group_by(Post.author_id)
        .having(func.count() > 2)
        .sql()
    )
    assert sql == (
        'SELECT "posts"."author_id", COUNT(*), CAST(SUM("posts"."views") AS BIGINT) FROM "posts" '
        'WHERE "posts"."published" = TRUE GROUP BY "posts"."author_id" HAVING (COUNT(*)) > 2'
    )


def test_aggregate_over_relation_is_a_correlated_subquery():
    sql = User.objects.select(User.id, func.count(User.posts), func.max(User.posts.views)).sql()
    assert sql == (
        'SELECT "users"."id", '
        '(SELECT COUNT(*) FROM "posts" AS "a1" WHERE "a1"."author_id" = "users"."id"), '
        '(SELECT MAX("a2"."views") FROM "posts" AS "a2" WHERE "a2"."author_id" = "users"."id") FROM "users"'
    )
    # Two hops join inside the subquery; in WHERE it filters per row, no EXISTS / JOIN outside.
    w = where(User.objects.filter(func.count(User.posts.comments) > 3))
    assert w == (
        '(SELECT COUNT(*) FROM "posts" AS "a1" INNER JOIN "comments" AS "a2" ON "a2"."post_id" = "a1"."id" '
        'WHERE "a1"."author_id" = "users"."id") > 3'
    )


def test_select_to_one_columns_join_and_to_many_is_rejected():
    sql = Post.objects.select(Post.title, Post.author.name.label("author")).sql()
    assert sql == (
        'SELECT "posts"."title", "j1"."name" FROM "posts" LEFT JOIN "users" AS "j1" ON "j1"."id" = "posts"."author_id"'
    )
    with pytest.raises(QueryError, match="aggregate it"):
        User.objects.select(User.posts.title).sql()


def test_subquery_and_distinct():
    inner = User.objects.filter(User.name == "A").select(User.id)
    assert where(Post.objects.filter(Post.author_id.in_(inner))) == (
        '"posts"."author_id" IN (SELECT "users"."id" FROM "users" WHERE "users"."name" = \'A\')'
    )
    assert "NOT IN (SELECT" in where(Post.objects.filter(Post.author_id.not_in(inner)))
    with pytest.raises(QueryError, match="exactly one column"):
        Post.objects.filter(Post.author_id.in_(User.objects.select(User.id, User.name))).sql()
    assert Post.objects.select(Post.author_id).distinct().sql().startswith('SELECT DISTINCT "posts"."author_id"')
    sql = Post.objects.select(Post.title).distinct(Post.author_id).order_by(Post.author_id, Post.views.desc()).sql()
    assert sql.startswith('SELECT DISTINCT ON ("posts"."author_id") "posts"."title" FROM "posts"')


def test_select_validation():
    with pytest.raises(ValueError, match="label"):
        Post.objects.select(Post.id, User.id)  # both named "id"
    with pytest.raises(TypeError):
        Post.objects.select(User)
    with pytest.raises(TypeError):
        Post.objects.select(1)
    with pytest.raises(QueryError):
        Post.objects.select_related(Post.author).select(Post.id)
    with pytest.raises(QueryError, match="unknown function"):
        Post.objects.select(orm_func("nope", Post.id)).sql()
    with pytest.raises(QueryError, match="lock"):
        Post.objects.lock().select(func.count()).sql()


def orm_func(name, *args):
    from orm.expr import Func

    return Func(name, args)


# -- subqueries, windows, CTEs ------------------------------------------------------------------


def test_exists_with_outer_correlates():
    from orm import exists, outer

    w = where(User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id)))))
    assert w == 'EXISTS(SELECT 1 FROM "posts" WHERE "posts"."author_id" = "users"."id")'


def test_subquery_over_the_same_table_gets_an_alias():
    from orm import outer

    avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
    w = where(Post.objects.filter(Post.views > avg))
    assert 'FROM "posts" AS "s1" WHERE "s1"."author_id" = "posts"."author_id"' in w


def test_window_function_sql():
    sql = Post.objects.select(
        func.sum(Post.views).over(partition_by=Post.author_id, order_by=Post.created_at.desc(), rows=(-2, 0))
    ).sql()
    assert sql.startswith(
        'SELECT CAST(SUM("posts"."views") OVER (PARTITION BY "posts"."author_id" '
        'ORDER BY "posts"."created_at" DESC ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS BIGINT)'
    )
    assert "NTILE(4) OVER ()" in Post.objects.select(func.ntile(4).over()).sql()


def test_cte_sql():
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    sql = User.objects.filter(User.id.in_(totals.select(totals.c.author_id).filter(totals.c.n > 2))).sql()
    assert sql.startswith(
        'WITH "totals" ("author_id", "n") AS (SELECT "posts"."author_id" AS "author_id", COUNT(*) AS "n" '
        'FROM "posts" GROUP BY "posts"."author_id") SELECT'
    )
    assert sql.endswith('IN (SELECT "totals"."author_id" FROM "totals" WHERE "totals"."n" > 2)')


def test_recursive_cte_joins_itself():
    chain = User.objects.filter(User.id == 1).cte("chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1))
    sql = User.objects.from_(chain).sql()
    assert sql.startswith('WITH RECURSIVE "chain"')
    assert 'FROM "users", "chain" WHERE "users"."id" = "chain"."id" + 1' in sql
    assert sql.endswith('FROM "chain"')


def test_many_to_many_filter_is_one_exists_through_the_join_table():
    from blog.models import Tag

    w = where(Post.objects.filter(Post.tags.name == "x"))
    assert w == (
        'EXISTS(SELECT 1 FROM "tags" AS "t1" INNER JOIN "post_tags" AS "t2" ON "t2"."tag_id" = "t1"."id" '
        "WHERE \"t2\".\"post_id\" = \"posts\".\"id\" AND \"t1\".\"name\" = 'x')"
    )
    sql = Post.objects.select(func.count(Post.tags)).sql()
    assert '(SELECT COUNT(*) FROM "tags" AS "a1" INNER JOIN "post_tags" AS "a2" ON "a2"."tag_id" = "a1"."id" ' \
           'WHERE "a2"."post_id" = "posts"."id")' in sql
    assert Tag.objects.filter(Tag.posts.author.name == "A").sql().count("EXISTS") == 2


def test_has_one_joins_on_the_other_side():
    sql = User.objects.select_related(User.profile).sql()
    assert 'LEFT JOIN "profiles" AS "j1" ON "j1"."user_id" = "users"."id"' in sql


def test_cte_reads_each_column_once(tmp_path):
    import orm

    (tmp_path / "loud.toml").write_text('name = "hstore"\n[types.loud]\nsql = "text"\nvalue = "text"\nread = "({} || \'!\')"\n')
    source = (
        f'import "{tmp_path / "loud.toml"}"\n'
        'datasource db {\n  provider = "postgresql"\n  extensions = [hstore]\n}\n'
        'model Doc {\n  id Int @id\n  word Unsupported("loud")\n}\n'
    )
    Doc = orm.loads(source, registry=orm.Registry())["Doc"]
    sql = Doc.objects.from_(Doc.objects.filter(Doc.id > 0).cte("recent")).sql()
    assert sql.count("|| '!'") == 1, sql


def test_minus_column_is_a_descending_order():
    assert Post.objects.order_by(-Post.created_at, Post.id).sql() == Post.objects.order_by(Post.created_at.desc(), Post.id).sql()
    assert Comment.objects.order_by(-Comment.post.title).sql().endswith('ORDER BY "j1"."title" DESC')
    assert repr(-Post.views) == "Post.views.desc()"
    with pytest.raises(TypeError, match="expected a condition"):
        Post.objects.filter(-Post.published)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="select\\(\\) takes columns and expressions"):
        Post.objects.select(-Post.views)  # type: ignore[call-overload]
    with pytest.raises(TypeError):
        -(Post.views + 1)  # type: ignore[operator]


def test_nulls_first_and_last():
    assert Comment.objects.order_by(Comment.author_id.desc(nulls="last")).sql().endswith('ORDER BY "comments"."author_id" DESC NULLS LAST')
    assert Comment.objects.order_by(Comment.author_id.asc(nulls="first"), Comment.id).sql().endswith('ORDER BY "comments"."author_id" ASC NULLS FIRST, "comments"."id" ASC')
    assert repr(Comment.author_id.asc(nulls="first").reversed()) == "Comment.author_id.desc(nulls='last')"
    with pytest.raises(ValueError, match="nulls is 'first' or 'last'"):
        Comment.author_id.desc(nulls="middle")  # type: ignore[arg-type]


def test_string_functions_and_concatenation():
    sql = Post.objects.select(
        func.concat(Post.title, " by ", Post.views), Post.title.concat("!"), func.trim(Post.title),
        func.ltrim(Post.title), func.rtrim(Post.title), func.replace(Post.title, "a", "b"),
        func.substr(Post.title, 2, 3), func.strpos(Post.title, "x"),
    ).sql()
    assert sql == (
        "SELECT CONCAT(\"posts\".\"title\", ' by ', \"posts\".\"views\"), \"posts\".\"title\" || '!', "
        "TRIM(\"posts\".\"title\"), LTRIM(\"posts\".\"title\"), RTRIM(\"posts\".\"title\"), "
        "REPLACE(\"posts\".\"title\", 'a', 'b'), SUBSTR(\"posts\".\"title\", 2, 3), "
        "STRPOS(\"posts\".\"title\", 'x') FROM \"posts\""
    )
    assert where(Post.objects.filter(Post.title.concat(Post.body) == "ab")) == "(\"posts\".\"title\" || \"posts\".\"body\") = 'ab'"
    with pytest.raises(TypeError, match="at least one"):
        func.concat()


def test_array_element_and_unnest():
    sql = Profile.objects.select(Profile.links[1], func.unnest(Profile.links)).sql()
    assert sql == "SELECT (\"profiles\".\"links\")[1], UNNEST(\"profiles\".\"links\") FROM \"profiles\""
    with pytest.raises(QueryError, match="only be a select\\(\\) column"):
        Profile.objects.filter(func.unnest(Profile.links) == "x").sql()
    with pytest.raises(QueryError, match="an index needs an array"):
        Post.objects.select(Post.title[1]).sql()  # type: ignore[index]
    with pytest.raises(TypeError, match="not iterable"):
        list(Profile.links)


def test_case_expression():
    sql = Post.objects.select(Post.author_id, func.sum(func.case((Post.published, 1), default=0)).label("n")).group_by(Post.author_id).sql()
    assert sql == (
        'SELECT "posts"."author_id", CAST(SUM(CASE WHEN "posts"."published" = TRUE THEN 1 ELSE 0 END) AS BIGINT) '
        'FROM "posts" GROUP BY "posts"."author_id"'
    )
    heat = func.case((Post.views > 100, "hot"), (Post.views > 10, "warm"), default="cold")
    assert where(Post.objects.filter(heat == "hot")) == (
        '(CASE WHEN "posts"."views" > 100 THEN \'hot\' WHEN "posts"."views" > 10 THEN \'warm\' ELSE \'cold\' END) = \'hot\''
    )
    assert Post.objects.order_by(func.case((Post.published, 0), default=1)).sql().endswith(
        'ORDER BY CASE WHEN "posts"."published" = TRUE THEN 0 ELSE 1 END ASC'
    )
    # without a default: ELSE NULL; a to-many path in a condition is an EXISTS
    assert where(User.objects.filter(func.case((User.posts.views > 3, User.name)) == "x")) == (
        'EXISTS(SELECT 1 FROM "posts" AS "t1" WHERE "t1"."author_id" = "users"."id" '
        'AND (CASE WHEN "t1"."views" > 3 THEN "users"."name" END) = \'x\')'
    )
    with pytest.raises(TypeError, match="at least one"):
        func.case(default=1)
    with pytest.raises(TypeError, match="tuples"):
        func.case(Post.published)  # type: ignore[arg-type]


def test_aggregate_filter():
    sql = Post.objects.select(
        Post.author_id, func.count(filter=Post.published), func.sum(Post.views, filter=Post.views > 10)
    ).group_by(Post.author_id).sql()
    assert sql == (
        'SELECT "posts"."author_id", COUNT(*) FILTER (WHERE "posts"."published" = TRUE), '
        'CAST(SUM("posts"."views") FILTER (WHERE "posts"."views" > 10) AS BIGINT) FROM "posts" GROUP BY "posts"."author_id"'
    )
    # over a relation, the filter is planned inside the correlated subquery
    assert User.objects.select(User.id, func.count(User.posts, filter=User.posts.published)).sql() == (
        'SELECT "users"."id", (SELECT COUNT(*) FILTER (WHERE "a1"."published" = TRUE) FROM "posts" AS "a1" '
        'WHERE "a1"."author_id" = "users"."id") FROM "users"'
    )
    assert 'COUNT(*) FILTER (WHERE "a1"."views" > 3) FROM "posts" AS "a1"' in User.objects.select(func.count(filter=User.posts.views > 3)).sql()
    # before OVER in a window
    assert 'SUM("posts"."views") FILTER (WHERE "posts"."published" = TRUE) OVER (PARTITION BY "posts"."author_id")' in (
        Post.objects.select(func.sum(Post.views, filter=Post.published).over(partition_by=Post.author_id)).sql()
    )
    with pytest.raises(QueryError, match="one relation path"):
        User.objects.select(func.count(User.posts, filter=User.comments.body == "x")).sql()
