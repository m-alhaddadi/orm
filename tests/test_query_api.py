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
    )
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
    tags = await Tag.objects.insert_many([{"name": "python"}, {"name": "rust"}])
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
    tags = await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}])
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
