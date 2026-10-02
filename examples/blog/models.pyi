# Generated from schema.prisma by `python -m orm generate`. Do not edit.
#
# Static half of the generated module. Each model gets:
#   * the model class: column descriptors (`User.email` is a ColumnRef[str] on the class,
#     a read-only str on an instance), relation descriptors and typed `update()`;
#   * a path class (`_UserPath`): what a relation to the model evaluates to on the class
#     side, so `User.posts.created_at` autocompletes and type-checks as ColumnRef[datetime];
#   * `UserInsert` / `UserUpdate` / `UserUpdateRow` TypedDicts: the row shapes accepted by
#     insert / update / update_many;
#   * a query set class (`UserQuerySet`): typed `insert()`, `insert_many()`, `update()`,
#     `update_many()`.

from collections.abc import Iterable
from datetime import datetime
from typing import ClassVar, NotRequired, Required, TypedDict

from typing_extensions import Unpack

from orm import ColumnRef, Expression, InsertMany, InsertOne, Model, QuerySet, RelationPath, Update, UpdateMany
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

    async def update(self, **values: Unpack[UserUpdate]) -> None: ...  # type: ignore[override]

class _UserPath(RelationPath[User]):
    id: ColumnRef[int]
    email: ColumnRef[str]
    name: ColumnRef[str]
    created_at: ColumnRef[datetime]
    posts: _PostPath
    comments: _CommentPath

class UserInsert(TypedDict):
    id: NotRequired[int]
    email: str
    name: str
    created_at: NotRequired[datetime]

class UserUpdate(TypedDict, total=False):
    id: int | Expression[int]
    email: str | Expression[str]
    name: str | Expression[str]
    created_at: datetime | Expression[datetime]

class UserUpdateRow(TypedDict, total=False):
    id: Required[int]
    email: str
    name: str
    created_at: datetime

class UserQuerySet(QuerySet[User]):
    def insert(self, **values: Unpack[UserInsert]) -> InsertOne[User]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[UserInsert]) -> InsertMany[User]: ...  # type: ignore[override]
    def update(self, **values: Unpack[UserUpdate]) -> Update[User]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[UserUpdateRow], *, batch_size: int | None = None) -> UpdateMany[User]: ...  # type: ignore[override]

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

    async def update(self, **values: Unpack[PostUpdate]) -> None: ...  # type: ignore[override]

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

class PostInsert(TypedDict):
    id: NotRequired[int]
    # One of author_id / author is required (checked at runtime).
    author_id: NotRequired[int]
    author: NotRequired[User]
    title: str
    body: str
    views: NotRequired[int]
    published: NotRequired[bool]
    created_at: NotRequired[datetime]

class PostUpdate(TypedDict, total=False):
    id: int | Expression[int]
    author_id: int | Expression[int]
    author: User
    title: str | Expression[str]
    body: str | Expression[str]
    views: int | Expression[int]
    published: bool | Expression[bool]
    created_at: datetime | Expression[datetime]

class PostUpdateRow(TypedDict, total=False):
    id: Required[int]
    author_id: int
    author: User
    title: str
    body: str
    views: int
    published: bool
    created_at: datetime

class PostQuerySet(QuerySet[Post]):
    def insert(self, **values: Unpack[PostInsert]) -> InsertOne[Post]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[PostInsert]) -> InsertMany[Post]: ...  # type: ignore[override]
    def update(self, **values: Unpack[PostUpdate]) -> Update[Post]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[PostUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Post]: ...  # type: ignore[override]

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

    async def update(self, **values: Unpack[CommentUpdate]) -> None: ...  # type: ignore[override]

class _CommentPath(RelationPath[Comment]):
    id: ColumnRef[int]
    post_id: ColumnRef[int]
    author_id: ColumnRef[int | None]
    body: ColumnRef[str]
    created_at: ColumnRef[datetime]
    post: _PostPath
    author: _UserPath

class CommentInsert(TypedDict):
    id: NotRequired[int]
    # One of post_id / post is required (checked at runtime).
    post_id: NotRequired[int]
    post: NotRequired[Post]
    author_id: NotRequired[int | None]
    author: NotRequired[User | None]
    body: str
    created_at: NotRequired[datetime]

class CommentUpdate(TypedDict, total=False):
    id: int | Expression[int]
    post_id: int | Expression[int]
    post: Post
    author_id: int | None | Expression[int | None]
    author: User | None
    body: str | Expression[str]
    created_at: datetime | Expression[datetime]

class CommentUpdateRow(TypedDict, total=False):
    id: Required[int]
    post_id: int
    post: Post
    author_id: int | None
    author: User | None
    body: str
    created_at: datetime

class CommentQuerySet(QuerySet[Comment]):
    def insert(self, **values: Unpack[CommentInsert]) -> InsertOne[Comment]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[CommentInsert]) -> InsertMany[Comment]: ...  # type: ignore[override]
    def update(self, **values: Unpack[CommentUpdate]) -> Update[Comment]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[CommentUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Comment]: ...  # type: ignore[override]

__all__ = [
    "User",
    "Post",
    "Comment",
    "UserInsert",
    "UserUpdate",
    "UserUpdateRow",
    "PostInsert",
    "PostUpdate",
    "PostUpdateRow",
    "CommentInsert",
    "CommentUpdate",
    "CommentUpdateRow",
    "UserQuerySet",
    "PostQuerySet",
    "CommentQuerySet",
]
