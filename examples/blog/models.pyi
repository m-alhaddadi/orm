# Generated from schema.orm. Do not edit.
#
# Static half of the generated module. Each model gets:
#   * the model class: column descriptors (`User.email` is a ColumnRef[str] on the class,
#     a str on an instance), relation descriptors and a typed constructor;
#   * a path class (`_UserPath`): what a relation to the model evaluates to on the class
#     side, so `User.posts.created_at` autocompletes and type-checks as ColumnRef[datetime];
#   * a query set class (`UserQuerySet`): typed `create()` / `update()` keyword arguments.

from datetime import datetime
from typing import ClassVar

from orm import ColumnRef, Expression, Model, QuerySet, RelationPath
from orm import fields as f

# -- User -------------------------------------------------------------------------------

class User(Model):
    id: f.BigInt[int]
    email: f.String[str]
    name: f.String[str]
    created_at: f.DateTime[datetime]

    posts: f.HasMany[Post, _PostPath]
    comments: f.HasMany[Comment, _CommentPath]

    objects: ClassVar[UserQuerySet]

    def __init__(
        self,
        *,
        id: int = ...,
        email: str,
        name: str,
        created_at: datetime = ...,
    ) -> None: ...

class _UserPath(RelationPath[User]):
    id: ColumnRef[int]
    email: ColumnRef[str]
    name: ColumnRef[str]
    created_at: ColumnRef[datetime]
    posts: _PostPath
    comments: _CommentPath

class UserQuerySet(QuerySet[User]):
    async def create(  # type: ignore[override]
        self,
        *,
        id: int = ...,
        email: str,
        name: str,
        created_at: datetime = ...,
    ) -> User: ...
    async def update(  # type: ignore[override]
        self,
        *,
        id: int | Expression[int] = ...,
        email: str | Expression[str] = ...,
        name: str | Expression[str] = ...,
        created_at: datetime | Expression[datetime] = ...,
    ) -> int: ...

# -- Post -------------------------------------------------------------------------------

class Post(Model):
    id: f.BigInt[int]
    author_id: f.BigInt[int]
    title: f.String[str]
    body: f.Text[str]
    views: f.Integer[int]
    published: f.Boolean[bool]
    created_at: f.DateTime[datetime]

    author: f.BelongsTo[User, _UserPath]
    comments: f.HasMany[Comment, _CommentPath]

    objects: ClassVar[PostQuerySet]

    def __init__(
        self,
        *,
        id: int = ...,
        author_id: int = ...,
        author: User = ...,
        title: str,
        body: str,
        views: int = ...,
        published: bool = ...,
        created_at: datetime = ...,
    ) -> None: ...

class _PostPath(RelationPath[Post]):
    id: ColumnRef[int]
    author_id: ColumnRef[int]
    title: ColumnRef[str]
    body: ColumnRef[str]
    views: ColumnRef[int]
    published: ColumnRef[bool]
    created_at: ColumnRef[datetime]
    author: _UserPath
    comments: _CommentPath

class PostQuerySet(QuerySet[Post]):
    async def create(  # type: ignore[override]
        self,
        *,
        id: int = ...,
        author_id: int = ...,
        author: User = ...,
        title: str,
        body: str,
        views: int = ...,
        published: bool = ...,
        created_at: datetime = ...,
    ) -> Post: ...
    async def update(  # type: ignore[override]
        self,
        *,
        id: int | Expression[int] = ...,
        author_id: int | Expression[int] = ...,
        author: User = ...,
        title: str | Expression[str] = ...,
        body: str | Expression[str] = ...,
        views: int | Expression[int] = ...,
        published: bool | Expression[bool] = ...,
        created_at: datetime | Expression[datetime] = ...,
    ) -> int: ...

# -- Comment ----------------------------------------------------------------------------

class Comment(Model):
    id: f.BigInt[int]
    post_id: f.BigInt[int]
    author_id: f.BigInt[int | None]
    body: f.Text[str]
    created_at: f.DateTime[datetime]

    post: f.BelongsTo[Post, _PostPath]
    author: f.BelongsTo[User | None, _UserPath]

    objects: ClassVar[CommentQuerySet]

    def __init__(
        self,
        *,
        id: int = ...,
        post_id: int = ...,
        post: Post = ...,
        author_id: int | None = ...,
        author: User | None = ...,
        body: str,
        created_at: datetime = ...,
    ) -> None: ...

class _CommentPath(RelationPath[Comment]):
    id: ColumnRef[int]
    post_id: ColumnRef[int]
    author_id: ColumnRef[int | None]
    body: ColumnRef[str]
    created_at: ColumnRef[datetime]
    post: _PostPath
    author: _UserPath

class CommentQuerySet(QuerySet[Comment]):
    async def create(  # type: ignore[override]
        self,
        *,
        id: int = ...,
        post_id: int = ...,
        post: Post = ...,
        author_id: int | None = ...,
        author: User | None = ...,
        body: str,
        created_at: datetime = ...,
    ) -> Comment: ...
    async def update(  # type: ignore[override]
        self,
        *,
        id: int | Expression[int] = ...,
        post_id: int | Expression[int] = ...,
        post: Post = ...,
        author_id: int | None | Expression[int | None] = ...,
        author: User | None = ...,
        body: str | Expression[str] = ...,
        created_at: datetime | Expression[datetime] = ...,
    ) -> int: ...

__all__ = ["User", "Post", "Comment", "UserQuerySet", "PostQuerySet", "CommentQuerySet"]
