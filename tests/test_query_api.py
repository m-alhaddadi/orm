"""Query-set API: custom query-set classes, prefetch on loaded instances, only() through
to-one paths, OR of query sets, single-row Prefetch, column paths and model metadata."""

import os
import sys

import pytest
from blog.models import Comment, Post, PostTag, Profile, Tag, User

import orm
from orm import QuerySet, use_query_set


@pytest.fixture
def custom():
    use_query_set(Post, "_blog_queries:PostQueries")
    use_query_set(Tag, "_blog_queries:TagQueries")
    yield sys.modules.get("_blog_queries")
    use_query_set(Post, QuerySet)
    use_query_set(Tag, QuerySet)


async def seed():
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "draft", "body": "...", "views": 50},
            {"author": alice, "title": "hit", "body": "...", "published": True, "views": 90},
            {"author": alice, "title": "quiet", "body": "...", "published": True, "views": 1},
            {"author": bob, "title": "bob", "body": "...", "published": True, "views": 70},
        ]
    ).returning()
    return alice, bob, posts


# -- custom query-set classes (F4) ---------------------------------------------------------


async def test_custom_methods_chain_with_builder_methods(clean, custom):
    alice, _, _ = await seed()
    qs = Post.objects.published().filter(Post.author_id == alice.id).popular(10)
    assert type(qs).__name__ == "PostQueries"
    assert [p.title for p in await qs] == ["hit"]
    assert [p.title for p in await Post.objects.order_by(Post.views).popular(60).published()] == ["bob", "hit"]
    assert await Post.objects.published().count() == 3


async def test_class_path_is_imported_on_first_use(clean):
    sys.modules.pop("_blog_queries", None)
    use_query_set(Post, "_blog_queries:PostQueries")
    try:
        assert "_blog_queries" not in sys.modules
        assert type(Post.objects).__name__ == "PostQueries"
        assert "_blog_queries" in sys.modules
        assert Post.objects is Post.objects  # the root query set replaces the lazy one
    finally:
        use_query_set(Post, QuerySet)
    assert type(Post.objects) is QuerySet


async def test_relation_sets_have_the_custom_methods(clean, custom):
    alice, _, posts = await seed()
    assert [p.title for p in await alice.posts.published().order_by(Post.views)] == ["quiet", "hit"]
    assert [p.title for p in await alice.posts.filter(Post.views > 5).published()] == ["hit"]
    assert isinstance(alice.posts, orm.RelatedSet)
    tags = await Tag.objects.insert_many([{"name": "python"}, {"name": "rust"}]).returning()
    await posts[1].tags.add(*tags)
    assert [t.name for t in await posts[1].tags.named("py")] == ["python"]
    assert isinstance(posts[1].tags, orm.ManyRelatedSet)


async def test_prefetch_query_set_uses_custom_methods(clean, custom):
    await seed()
    users = await User.objects.order_by(User.name).prefetch_related(orm.Prefetch(User.posts, Post.objects.published()))
    assert [[p.title for p in u.posts.cached] for u in users] == [["hit", "quiet"], ["bob"]]


def test_query_set_class_is_checked():
    with pytest.raises(TypeError, match="subclass of orm.QuerySet"):
        use_query_set(Post, dict)  # type: ignore[arg-type]

    class Slotted(QuerySet[Post]):
        __slots__ = ("x",)

    with pytest.raises(TypeError, match="__slots__"):
        use_query_set(Post, Slotted)
    use_query_set(Post, "_blog_queries")
    try:
        with pytest.raises(TypeError, match="module:Class"):
            Post.objects
    finally:
        use_query_set(Post, QuerySet)


CHECK_QUERY_SETS = '''\
from typing_extensions import assert_type

from app.models import Post, User, UserQuerySet
from app.queries import PostQueries

assert_type(Post.objects, PostQueries)
assert_type(Post.objects.published().filter(Post.views > 1).order_by(Post.id).published(), PostQueries)
assert_type(User.objects.filter(User.id == 1), UserQuerySet)
Post.objects.insert(title="t", body="b", author_id=1)
Post.objects.nope()  # E
Post.objects.published().insert(title=1)  # E
User.objects.published()  # E
'''


def _query_set_project(tmp_path):
    from pathlib import Path

    app = tmp_path / "app"
    app.mkdir()
    (app / "__init__.py").touch()
    (app / "schema.prisma").write_text((Path(__file__).parent.parent / "examples/blog/schema.prisma").read_text())
    (app / "queries.py").write_text(
        "from typing_extensions import Self\n\nfrom app.models import Post, PostQuerySet\n\n\n"
        "class PostQueries(PostQuerySet):\n    def published(self) -> Self:\n        return self.filter(Post.published)\n"
    )
    code = orm._native.cli(["--schema", str(app / "schema.prisma"), "generate", "python", "-o", str(app / "models.py"),
                            "--query-set", "Post=app.queries:PostQueries"])
    assert code == 0
    (tmp_path / "check.py").write_text(CHECK_QUERY_SETS)
    return {i for i, line in enumerate(CHECK_QUERY_SETS.splitlines(), 1) if "# E" in line}


def test_generated_query_set_types(tmp_path):
    import re
    import shutil
    import subprocess
    from pathlib import Path

    pytest.importorskip("mypy")
    expected = _query_set_project(tmp_path)
    root = Path(__file__).parent.parent
    out = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", "check.py"], capture_output=True, text=True,
        env={"MYPYPATH": f"{root / 'python'}:{tmp_path}", "PATH": ""}, cwd=tmp_path,
    ).stdout
    assert {int(n) for n in re.findall(r"^check\.py:(\d+): error", out, re.M)} == expected, out
    exe = shutil.which("pyright")
    if exe is not None:
        (tmp_path / "pyrightconfig.json").write_text(
            f'{{"extraPaths": ["{root / "python"}"], "typeCheckingMode": "strict", "reportUnusedExpression": false}}'
        )
        out = subprocess.run([exe, "check.py"], capture_output=True, text=True, cwd=tmp_path).stdout
        assert {int(n) for n in re.findall(r"check\.py:(\d+):\d+ - error", out)} == expected, out
    run = subprocess.run(
        [sys.executable, "-c", "from app.models import Post; print(type(Post.objects).__name__)"],
        capture_output=True, text=True, cwd=tmp_path,
        env={**os.environ, "PYTHONPATH": os.pathsep.join(filter(None, [str(tmp_path), os.environ.get("PYTHONPATH")]))},
    )
    assert run.stdout.strip() == "PostQueries", run.stderr


def test_cli_reads_query_sets_from_pyproject(tmp_path, monkeypatch, capfd):
    (tmp_path / "schema.prisma").write_text("model Book {\n id Int @id\n}\n")
    (tmp_path / "pyproject.toml").write_text('[tool.orm.query_sets]\nBook = "app.queries:BookQueries"\n')
    monkeypatch.chdir(tmp_path)
    assert orm._native.cli(["generate", "python", "-o", "models.py"]) == 0
    assert 'use_query_set(Book, "app.queries:BookQueries")' in (tmp_path / "models.py").read_text()
    assert "objects: ClassVar[_BookObjects]" in (tmp_path / "models.pyi").read_text()
    assert orm._native.cli(["generate", "python", "-o", "models.py", "--query-set", "Book=nomodule"]) == 1
    assert "expected module:Class" in capfd.readouterr().err


# -- prefetch on loaded instances (F10) -----------------------------------------------------


async def test_prefetch_onto_loaded_instances(clean):
    alice, bob, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[1], "body": "a"}, {"post": posts[1], "body": "b"}])
    users = await User.objects.order_by(User.id)
    await orm.prefetch(users, User.posts.comments, orm.Prefetch(User.posts, Post.objects.filter(Post.published)[:1], to_attr="top"))
    assert [p.title for p in users[0].posts.cached] == ["draft", "hit", "quiet"]
    assert [c.body for c in users[0].posts.cached[1].comments.cached] == ["a", "b"]
    assert users[0].posts.cached[0].author is users[0]
    assert [[p.title for p in u.top] for u in users] == [["hit"], ["bob"]]


async def test_prefetch_to_one_and_many_to_many(clean):
    _, _, posts = await seed()
    tags = await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}]).returning()
    await posts[0].tags.add(*tags)
    loaded = await Post.objects.order_by(Post.id)
    await orm.prefetch(loaded, Post.author, Post.tags)
    assert [p.author.name for p in loaded] == ["Alice", "Alice", "Alice", "Bob"]
    assert [t.name for t in loaded[0].tags.cached] == ["a", "b"] and loaded[1].tags.cached == []


async def test_prefetch_reads_keys_from_the_instances(clean):
    _, _, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[0], "body": "a"}, {"post": posts[3], "body": "b"}])
    comments = await Comment.objects.order_by(Comment.id)
    await Comment.objects.delete()  # the instances' own rows are not read again
    await orm.prefetch(comments, Comment.post.author)
    assert [(c.post.title, c.post.author.name) for c in comments] == [("draft", "Alice"), ("bob", "Bob")]


async def test_prefetch_checks_its_input(clean):
    alice, _, posts = await seed()
    await orm.prefetch([], User.posts)
    with pytest.raises(TypeError, match="one model"):
        await orm.prefetch([alice, posts[0]], User.posts)
    with pytest.raises(ValueError, match="does not start at User"):
        await orm.prefetch([alice], Post.author)


# -- model metadata for factories (item 12) ---------------------------------------------------


def test_describe_gives_fields_relations_and_unique_keys():
    from decimal import Decimal

    from blog.models import Role

    post = orm.describe(Post)
    assert post["name"] == "Post" and post["table"] == "posts" and post["primary_key"] == "id"
    fields = {f["name"]: f for f in post["fields"]}
    assert fields["id"]["default"] == "database" and fields["id"]["primary_key"]
    assert fields["title"] == {
        "name": "title", "column": "title", "type": "string", "python_type": str, "element_type": None,
        "nullable": False, "array": False, "enum": None, "max_length": 200, "primary_key": False,
        "unique": False, "default": None, "insert": True,
    }
    relations = {r["name"]: r for r in post["relations"]}
    assert relations["author"]["kind"] == "belongs_to" and relations["author"]["target"] is User
    assert relations["author"]["from"] == "author_id" and not relations["author"]["nullable"]
    assert relations["tags"]["kind"] == "many_to_many" and relations["tags"]["through"] is PostTag
    assert orm.describe(PostTag)["unique"] == [("id",), ("post_id", "tag_id")]
    assert orm.describe(User)["unique"] == [("id",), ("email",)]
    profile = {f["name"]: f for f in orm.describe(Profile)["fields"]}
    assert profile["role"]["enum"] is Role and profile["role"]["python_type"] is Role
    assert profile["balance"]["python_type"] is Decimal
    assert profile["links"]["array"] and profile["links"]["element_type"] is str
    comment = {f["name"]: f for f in orm.describe(Comment)["fields"]}
    assert comment["author_id"]["nullable"]


async def make(model, n=[0], **values):
    """A factory built only on describe() and objects.insert(): required fields get
    generated values, a required to-one relation gets a new related row."""
    info = orm.describe(model)
    n[0] += 1
    for rel in info["relations"]:
        if rel["kind"] == "belongs_to" and not rel["nullable"] and rel["name"] not in values and rel["from"] not in values:
            values[rel["name"]] = await make(rel["target"])
    samples = {str: lambda f: f"{f['name']}-{n[0]}"[: f["max_length"] or None], int: lambda f: n[0], bool: lambda f: False}
    for f in info["fields"]:
        if f["insert"] and f["default"] is None and not f["nullable"] and f["name"] not in values:
            if not any(r["from"] == f["name"] and r["name"] in values for r in info["relations"]):
                values[f["name"]] = samples[f["python_type"]](f)
    return await model.objects.insert(**values)


async def test_a_factory_needs_only_describe_and_insert(clean):
    post = await make(Post)
    assert post.title.startswith("title-") and post.views == 0
    author = await User.objects.get(User.id == post.author_id)
    assert author.email.startswith("email-")
    tag_link = await make(PostTag, post=post)
    assert tag_link.post_id == post.id


# -- only() through to-one paths (F11) ------------------------------------------------------


async def test_only_through_a_to_one_path(clean):
    await seed()
    qs = Comment.objects.only(Comment.body, Comment.post.title)
    sql = qs.sql()
    assert '"title"' in sql and '"views"' not in sql and '"published"' not in sql
    comment = await Comment.objects.insert(post=(await Post.objects.get(Post.title == "hit")), body="x")
    (c,) = await qs.filter(Comment.id == comment.id)
    assert c.body == "x" and c.post.title == "hit" and c.post.pk is not None
    with pytest.raises(orm.NotLoaded):
        c.post.views
    with pytest.raises(orm.NotLoaded):
        c.created_at


async def test_only_a_path_column_leaves_the_root_without_public_fields(clean):
    # S4.4: Django's meaning of only("post__title")
    _, _, posts = await seed()
    await Comment.objects.insert(post=posts[0], body="x")
    (c,) = await Comment.objects.only(Comment.post.author.name)
    assert c.post.author.name == "Alice" and c.pk is not None
    with pytest.raises(orm.NotLoaded):
        c.body
    with pytest.raises(orm.NotLoaded):
        c.post.title


def test_only_rejects_a_to_many_path():
    with pytest.raises(TypeError, match="to-many relation User.posts"):
        User.objects.only(User.posts.title)


# -- qs1 | qs2, Prefetch(one=True), orm.column ----------------------------------------------


async def test_or_of_query_sets(clean):
    await seed()
    popular = Post.objects.order_by(Post.id).filter(Post.views >= 80)
    drafts = Post.objects.filter(~Post.published)
    assert [p.title for p in await (popular | drafts)] == ["draft", "hit"]
    assert len(await (popular | Post.objects)) == 4
    with pytest.raises(orm.QueryError, match="one filter"):
        popular.filter(Post.views < 100) | drafts
    with pytest.raises(orm.QueryError, match="sets order"):
        drafts | popular
    with pytest.raises(orm.QueryError, match="sliced"):
        popular[:1] | drafts


async def test_prefetch_one_stores_a_row_or_none(clean):
    await seed()
    await User.objects.insert(email="carol@example.com", name="Carol")
    users = await User.objects.order_by(User.id).prefetch_related(
        orm.Prefetch(User.posts, Post.objects.order_by(-Post.views), to_attr="best", one=True)
    )
    assert [u.best.title if u.best else None for u in users] == ["hit", "bob", None]
    with pytest.raises(ValueError, match="to_attr"):
        orm.Prefetch(User.posts, one=True)
    with pytest.raises(ValueError, match="slice"):
        orm.Prefetch(User.posts, Post.objects[:2], to_attr="x", one=True)
    with pytest.raises(orm.QueryError, match="to-many"):
        await Post.objects.prefetch_related(orm.Prefetch(Post.author, to_attr="writer", one=True))


async def test_column_from_a_dotted_path(clean):
    await seed()
    assert repr(orm.column(Comment, "post.author.name")) == repr(Comment.post.author.name)
    names = await User.objects.filter(orm.column(User, "posts.title").startswith("hi")).select(User.name).scalars()
    assert names == ["Alice"]
    assert [p.title for p in await Post.objects.order_by(-orm.column(Post, "views"))][:1] == ["hit"]
    with pytest.raises(LookupError, match="Post has no relation 'nope'"):
        orm.column(Post, "nope.title")
    with pytest.raises(LookupError, match="User has no field 'nope'"):
        orm.column(Post, "author.nope")
