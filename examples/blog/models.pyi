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
from decimal import Decimal
from enum import IntEnum, StrEnum
from typing import ClassVar, NotRequired, Required, TypedDict

from typing_extensions import Unpack

from orm import ColumnRef, Expression, InsertMany, InsertOne, Model, QuerySet, RelationPath, Update, UpdateMany
from orm import fields as f

# -- Role -------------------------------------------------------------------------------

class Role(StrEnum):
    member = "member"
    editor = "editor"
    admin = "admin"

# -- Priority ---------------------------------------------------------------------------

class Priority(IntEnum):
    low = 1
    normal = 2
    high = 3

# -- User -------------------------------------------------------------------------------

class User(Model):
    id: f.BigInt[int]
    email: f.String[str]
    name: f.String[str]
    created_at: f.DateTime[datetime]

    posts: f.HasMany[Post, _PostPath]
    comments: f.HasMany[Comment, _CommentPath]
    profile: f.HasOne[Profile | None, _ProfilePath]

    objects: ClassVar[UserQuerySet]

    async def update(self, **values: Unpack[UserUpdate]) -> None: ...  # type: ignore[override]

class _UserPath(RelationPath[User]):
    id: ColumnRef[int]
    email: ColumnRef[str]
    name: ColumnRef[str]
    created_at: ColumnRef[datetime]
    posts: _PostPath
    comments: _CommentPath
    profile: _ProfilePath

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

# -- Profile ----------------------------------------------------------------------------

class Profile(Model):
    id: f.BigInt[int]
    user_id: f.BigInt[int]
    role: f.Enum[Role]
    balance: f.Decimal[Decimal]
    links: f.Array[list[str]]

    user: f.BelongsTo[User, _UserPath]

    objects: ClassVar[ProfileQuerySet]

    async def update(self, **values: Unpack[ProfileUpdate]) -> None: ...  # type: ignore[override]

class _ProfilePath(RelationPath[Profile]):
    id: ColumnRef[int]
    user_id: ColumnRef[int]
    role: ColumnRef[Role]
    balance: ColumnRef[Decimal]
    links: ColumnRef[list[str]]
    user: _UserPath

class ProfileInsert(TypedDict):
    id: NotRequired[int]
    # One of user_id / user is required (checked at runtime).
    user_id: NotRequired[int]
    user: NotRequired[User]
    role: NotRequired[Role]
    balance: NotRequired[Decimal]
    links: NotRequired[list[str]]

class ProfileUpdate(TypedDict, total=False):
    id: int | Expression[int]
    user_id: int | Expression[int]
    user: User
    role: Role | Expression[Role]
    balance: Decimal | Expression[Decimal]
    links: list[str] | Expression[list[str]]

class ProfileUpdateRow(TypedDict, total=False):
    id: Required[int]
    user_id: int
    user: User
    role: Role
    balance: Decimal
    links: list[str]

class ProfileQuerySet(QuerySet[Profile]):
    def insert(self, **values: Unpack[ProfileInsert]) -> InsertOne[Profile]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[ProfileInsert]) -> InsertMany[Profile]: ...  # type: ignore[override]
    def update(self, **values: Unpack[ProfileUpdate]) -> Update[Profile]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[ProfileUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Profile]: ...  # type: ignore[override]

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
    tags: f.ManyToMany[Tag, _TagPath]
    post_tags: f.HasMany[PostTag, _PostTagPath]

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
    tags: _TagPath
    post_tags: _PostTagPath

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

# -- Tag --------------------------------------------------------------------------------

class Tag(Model):
    id: f.BigInt[int]
    name: f.String[str]
    priority: f.Enum[Priority]

    posts: f.ManyToMany[Post, _PostPath]
    post_tags: f.HasMany[PostTag, _PostTagPath]

    objects: ClassVar[TagQuerySet]

    async def update(self, **values: Unpack[TagUpdate]) -> None: ...  # type: ignore[override]

class _TagPath(RelationPath[Tag]):
    id: ColumnRef[int]
    name: ColumnRef[str]
    priority: ColumnRef[Priority]
    posts: _PostPath
    post_tags: _PostTagPath

class TagInsert(TypedDict):
    id: NotRequired[int]
    name: str
    priority: NotRequired[Priority]

class TagUpdate(TypedDict, total=False):
    id: int | Expression[int]
    name: str | Expression[str]
    priority: Priority | Expression[Priority]

class TagUpdateRow(TypedDict, total=False):
    id: Required[int]
    name: str
    priority: Priority

class TagQuerySet(QuerySet[Tag]):
    def insert(self, **values: Unpack[TagInsert]) -> InsertOne[Tag]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[TagInsert]) -> InsertMany[Tag]: ...  # type: ignore[override]
    def update(self, **values: Unpack[TagUpdate]) -> Update[Tag]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[TagUpdateRow], *, batch_size: int | None = None) -> UpdateMany[Tag]: ...  # type: ignore[override]

# -- PostTag ----------------------------------------------------------------------------

class PostTag(Model):
    id: f.BigInt[int]
    post_id: f.BigInt[int]
    tag_id: f.BigInt[int]

    post: f.BelongsTo[Post, _PostPath]
    tag: f.BelongsTo[Tag, _TagPath]

    objects: ClassVar[PostTagQuerySet]

    async def update(self, **values: Unpack[PostTagUpdate]) -> None: ...  # type: ignore[override]

class _PostTagPath(RelationPath[PostTag]):
    id: ColumnRef[int]
    post_id: ColumnRef[int]
    tag_id: ColumnRef[int]
    post: _PostPath
    tag: _TagPath

class PostTagInsert(TypedDict):
    id: NotRequired[int]
    # One of post_id / post is required (checked at runtime).
    post_id: NotRequired[int]
    post: NotRequired[Post]
    # One of tag_id / tag is required (checked at runtime).
    tag_id: NotRequired[int]
    tag: NotRequired[Tag]

class PostTagUpdate(TypedDict, total=False):
    id: int | Expression[int]
    post_id: int | Expression[int]
    post: Post
    tag_id: int | Expression[int]
    tag: Tag

class PostTagUpdateRow(TypedDict, total=False):
    id: Required[int]
    post_id: int
    post: Post
    tag_id: int
    tag: Tag

class PostTagQuerySet(QuerySet[PostTag]):
    def insert(self, **values: Unpack[PostTagInsert]) -> InsertOne[PostTag]: ...  # type: ignore[override]
    def insert_many(self, rows: Iterable[PostTagInsert]) -> InsertMany[PostTag]: ...  # type: ignore[override]
    def update(self, **values: Unpack[PostTagUpdate]) -> Update[PostTag]: ...  # type: ignore[override]
    def update_many(self, rows: Iterable[PostTagUpdateRow], *, batch_size: int | None = None) -> UpdateMany[PostTag]: ...  # type: ignore[override]

__all__ = [
    "Role",
    "Priority",
    "User",
    "Profile",
    "Post",
    "Comment",
    "Tag",
    "PostTag",
    "UserInsert",
    "UserUpdate",
    "UserUpdateRow",
    "ProfileInsert",
    "ProfileUpdate",
    "ProfileUpdateRow",
    "PostInsert",
    "PostUpdate",
    "PostUpdateRow",
    "CommentInsert",
    "CommentUpdate",
    "CommentUpdateRow",
    "TagInsert",
    "TagUpdate",
    "TagUpdateRow",
    "PostTagInsert",
    "PostTagUpdate",
    "PostTagUpdateRow",
    "UserQuerySet",
    "ProfileQuerySet",
    "PostQuerySet",
    "CommentQuerySet",
    "TagQuerySet",
    "PostTagQuerySet",
]
