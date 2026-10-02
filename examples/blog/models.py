# Generated from schema.orm. Do not edit.
#
# Runtime half of the generated module: declares the models with the `orm.fields`
# descriptors. models.pyi carries the static types (columns, relation paths, typed
# constructors and query sets) for editors and type checkers.

from orm import Model, QuerySet
from orm import fields as f


class User(Model, table="users"):
    id = f.BigInt(primary_key=True, auto_increment=True)
    email = f.String(254, unique=True)
    name = f.String(100)
    created_at = f.DateTime(default_now=True)

    posts = f.HasMany("Post", via="author_id")
    comments = f.HasMany("Comment", via="author_id")


class Post(Model, table="posts"):
    id = f.BigInt(primary_key=True, auto_increment=True)
    author_id = f.BigInt(index=True)
    title = f.String(200)
    body = f.Text()
    views = f.Integer(default=0)
    published = f.Boolean(default=False)
    created_at = f.DateTime(default_now=True)

    author = f.BelongsTo("User", via="author_id", on_delete="cascade")
    comments = f.HasMany("Comment", via="post_id")


class Comment(Model, table="comments"):
    id = f.BigInt(primary_key=True, auto_increment=True)
    post_id = f.BigInt(index=True)
    author_id = f.BigInt(nullable=True, index=True)
    body = f.Text()
    created_at = f.DateTime(default_now=True)

    post = f.BelongsTo("Post", via="post_id", on_delete="cascade")
    author = f.BelongsTo("User", via="author_id", on_delete="set_null")


# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.
UserQuerySet = PostQuerySet = CommentQuerySet = QuerySet
UserInsert = UserUpdate = PostInsert = PostUpdate = CommentInsert = CommentUpdate = dict

__all__ = [
    "User",
    "Post",
    "Comment",
    "UserInsert",
    "UserUpdate",
    "PostInsert",
    "PostUpdate",
    "CommentInsert",
    "CommentUpdate",
    "UserQuerySet",
    "PostQuerySet",
    "CommentQuerySet",
]
