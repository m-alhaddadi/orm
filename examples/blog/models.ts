// Generated from schema.prisma by `orm generate typescript`. Do not edit.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, type Column, type Compat, type Decimal, type Expression, type Hop, type In, type Instance, type Many, type ManyRelatedSet, type ModelClass, type RelatedSet, type RelationPath, type SchemaIR } from "orm";

const SCHEMA: SchemaIR = {
  "models": [
    {
      "name": "User",
      "table": "users",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "email",
          "column": "email",
          "type": "string",
          "unique": true,
          "max_length": 254
        },
        {
          "name": "name",
          "column": "name",
          "type": "string",
          "max_length": 100
        },
        {
          "name": "created_at",
          "column": "created_at",
          "type": "date_time",
          "default_now": true
        }
      ],
      "relations": [
        {
          "name": "posts",
          "kind": "many",
          "target": "Post",
          "from": "id",
          "to": "author_id"
        },
        {
          "name": "comments",
          "kind": "many",
          "target": "Comment",
          "from": "id",
          "to": "author_id"
        },
        {
          "name": "profile",
          "kind": "one",
          "target": "Profile",
          "from": "id",
          "to": "user_id"
        }
      ]
    },
    {
      "name": "Profile",
      "table": "profiles",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "user_id",
          "column": "user_id",
          "type": "big_int",
          "unique": true
        },
        {
          "name": "role",
          "column": "role",
          "type": "string",
          "enum": "Role",
          "default": "member",
          "db_type": "\"role\"",
          "write_sql": "CAST({} AS \"role\")"
        },
        {
          "name": "balance",
          "column": "balance",
          "type": "decimal",
          "default": 0,
          "db_type": "numeric(12, 2)"
        },
        {
          "name": "links",
          "column": "links",
          "type": "string",
          "array": true,
          "default": []
        }
      ],
      "relations": [
        {
          "name": "user",
          "kind": "one",
          "target": "User",
          "from": "user_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        }
      ]
    },
    {
      "name": "Post",
      "table": "posts",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "author_id",
          "column": "author_id",
          "type": "big_int",
          "index": true
        },
        {
          "name": "title",
          "column": "title",
          "type": "string",
          "max_length": 200
        },
        {
          "name": "body",
          "column": "body",
          "type": "text"
        },
        {
          "name": "views",
          "column": "views",
          "type": "int",
          "default": 0
        },
        {
          "name": "published",
          "column": "published",
          "type": "bool",
          "default": false
        },
        {
          "name": "created_at",
          "column": "created_at",
          "type": "date_time",
          "default_now": true
        }
      ],
      "relations": [
        {
          "name": "author",
          "kind": "one",
          "target": "User",
          "from": "author_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        },
        {
          "name": "comments",
          "kind": "many",
          "target": "Comment",
          "from": "id",
          "to": "post_id"
        },
        {
          "name": "tags",
          "kind": "many",
          "target": "Tag",
          "from": "id",
          "to": "id",
          "through": {
            "model": "PostTag",
            "source": "post_id",
            "target": "tag_id"
          }
        },
        {
          "name": "post_tags",
          "kind": "many",
          "target": "PostTag",
          "from": "id",
          "to": "post_id"
        }
      ],
      "indexes": [
        {
          "columns": [
            {
              "field": "author_id"
            },
            {
              "field": "created_at",
              "desc": true
            }
          ],
          "where": "published"
        },
        {
          "columns": [
            {
              "field": "title",
              "opclass": "gin_trgm_ops"
            }
          ],
          "method": "gin"
        }
      ],
      "constraints": [
        {
          "kind": "check",
          "name": "posts_views_not_negative",
          "expr": "views >= 0"
        }
      ]
    },
    {
      "name": "Comment",
      "table": "comments",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "post_id",
          "column": "post_id",
          "type": "big_int",
          "index": true
        },
        {
          "name": "author_id",
          "column": "author_id",
          "type": "big_int",
          "nullable": true,
          "index": true
        },
        {
          "name": "body",
          "column": "body",
          "type": "text"
        },
        {
          "name": "created_at",
          "column": "created_at",
          "type": "date_time",
          "default_now": true
        }
      ],
      "relations": [
        {
          "name": "post",
          "kind": "one",
          "target": "Post",
          "from": "post_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        },
        {
          "name": "author",
          "kind": "one",
          "target": "User",
          "from": "author_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "set_null"
        }
      ]
    },
    {
      "name": "Tag",
      "table": "tags",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "name",
          "column": "name",
          "type": "string",
          "unique": true,
          "max_length": 50
        },
        {
          "name": "priority",
          "column": "priority",
          "type": "int",
          "enum": "Priority",
          "default": 2
        }
      ],
      "relations": [
        {
          "name": "posts",
          "kind": "many",
          "target": "Post",
          "from": "id",
          "to": "id",
          "through": {
            "model": "PostTag",
            "source": "tag_id",
            "target": "post_id"
          }
        },
        {
          "name": "post_tags",
          "kind": "many",
          "target": "PostTag",
          "from": "id",
          "to": "tag_id"
        }
      ]
    },
    {
      "name": "PostTag",
      "table": "post_tags",
      "fields": [
        {
          "name": "id",
          "column": "id",
          "type": "big_int",
          "primary_key": true,
          "auto_increment": true
        },
        {
          "name": "post_id",
          "column": "post_id",
          "type": "big_int"
        },
        {
          "name": "tag_id",
          "column": "tag_id",
          "type": "big_int",
          "index": true
        }
      ],
      "relations": [
        {
          "name": "post",
          "kind": "one",
          "target": "Post",
          "from": "post_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        },
        {
          "name": "tag",
          "kind": "one",
          "target": "Tag",
          "from": "tag_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        }
      ],
      "constraints": [
        {
          "kind": "unique",
          "fields": [
            "post_id",
            "tag_id"
          ]
        }
      ]
    }
  ],
  "enums": [
    {
      "name": "Role",
      "db_name": "role",
      "storage": "native",
      "values": [
        {
          "name": "member",
          "value": "member"
        },
        {
          "name": "editor",
          "value": "editor"
        },
        {
          "name": "admin",
          "value": "admin"
        }
      ]
    },
    {
      "name": "Priority",
      "db_name": "priority",
      "storage": "int",
      "values": [
        {
          "name": "low",
          "value": 1
        },
        {
          "name": "normal",
          "value": 2
        },
        {
          "name": "high",
          "value": 3
        }
      ]
    }
  ]
};

const models = define(SCHEMA);

// -- Role ------------------------------------------------------------------------------

export const Role = {
  member: "member",
  editor: "editor",
  admin: "admin",
} as const;
export type Role = (typeof Role)[keyof typeof Role];

// -- Priority --------------------------------------------------------------------------

export const Priority = {
  low: 1,
  normal: 2,
  high: 3,
} as const;
export type Priority = (typeof Priority)[keyof typeof Priority];

// -- User ------------------------------------------------------------------------------

/** The column values of a User row. */
export interface UserData {
  readonly id: bigint;
  readonly email: string;
  readonly name: string;
  readonly createdAt: Date;
}

/** A User row. To-one relations are typed on rows of queries that load them. */
export interface User extends UserData, Instance<UserSpec> {
  readonly posts: RelatedSet<PostSpec, "authorId" | "author">;
  readonly comments: RelatedSet<CommentSpec, "authorId" | "author">;
}

export type UserInsert = {
  id?: In<bigint>;
  email: In<string>;
  name: In<string>;
  createdAt?: In<Date>;
};

export interface UserUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "User" | "~User", {}>;
  email?: In<string> | Expression<Compat<string>, "User" | "~User", {}>;
  name?: In<string> | Expression<Compat<string>, "User" | "~User", {}>;
  createdAt?: In<Date> | Expression<Compat<Date>, "User" | "~User", {}>;
}

export interface UserUpdateRow {
  id: In<bigint>;
  email?: In<string>;
  name?: In<string>;
  createdAt?: In<Date>;
}

export interface UserSpec {
  readonly name: "User";
  readonly row: User;
  readonly data: UserData;
  readonly insert: UserInsert;
  readonly update: UserUpdate;
  readonly updateRow: UserUpdateRow;
  readonly pk: bigint;
}

export interface UserFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly email: Column<O extends true ? string | null : string, S>;
  readonly name: Column<O extends true ? string | null : string, S>;
  readonly createdAt: Column<O extends true ? Date | null : Date, S>;
  readonly posts: PostPath<S | Many, [...H, Hop<"posts", "many", PostSpec>], O>;
  readonly comments: CommentPath<S | Many, [...H, Hop<"comments", "many", CommentSpec>], O>;
  readonly profile: ProfilePath<S, [...H, Hop<"profile", "opt", ProfileSpec>], true>;
}

export interface UserPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<UserSpec, S, H>,
    UserFields<S, H, O> {}

export interface UserModel extends ModelClass<UserSpec>, UserFields<"User", [], false> {}

export const User = models["User"] as unknown as UserModel;

// -- Profile ---------------------------------------------------------------------------

/** The column values of a Profile row. */
export interface ProfileData {
  readonly id: bigint;
  readonly userId: bigint;
  readonly role: Role;
  readonly balance: Decimal;
  readonly links: string[];
}

/** A Profile row. To-one relations are typed on rows of queries that load them. */
export interface Profile extends ProfileData, Instance<ProfileSpec> {
}

export type ProfileInsert = {
  id?: In<bigint>;
  role?: In<Role>;
  balance?: In<Decimal>;
  links?: In<string[]>;
} & (
  | { userId: In<bigint>; user?: never }
  | { user: { readonly id: In<bigint> }; userId?: never }
);

export interface ProfileUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Profile" | "~Profile", {}>;
  userId?: In<bigint> | Expression<Compat<bigint>, "Profile" | "~Profile", {}>;
  user?: { readonly id: In<bigint> };
  role?: In<Role> | Expression<Compat<Role>, "Profile" | "~Profile", {}>;
  balance?: In<Decimal> | Expression<Compat<Decimal>, "Profile" | "~Profile", {}>;
  links?: In<string[]> | Expression<Compat<string[]>, "Profile" | "~Profile", {}>;
}

export interface ProfileUpdateRow {
  id: In<bigint>;
  userId?: In<bigint>;
  user?: { readonly id: In<bigint> };
  role?: In<Role>;
  balance?: In<Decimal>;
  links?: In<string[]>;
}

export interface ProfileSpec {
  readonly name: "Profile";
  readonly row: Profile;
  readonly data: ProfileData;
  readonly insert: ProfileInsert;
  readonly update: ProfileUpdate;
  readonly updateRow: ProfileUpdateRow;
  readonly pk: bigint;
}

export interface ProfileFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly userId: Column<O extends true ? bigint | null : bigint, S>;
  readonly role: Column<O extends true ? Role | null : Role, S>;
  readonly balance: Column<O extends true ? Decimal | null : Decimal, S>;
  readonly links: Column<O extends true ? string[] | null : string[], S>;
  readonly user: UserPath<S, [...H, Hop<"user", "one", UserSpec>], O>;
}

export interface ProfilePath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<ProfileSpec, S, H>,
    ProfileFields<S, H, O> {}

export interface ProfileModel extends ModelClass<ProfileSpec>, ProfileFields<"Profile", [], false> {}

export const Profile = models["Profile"] as unknown as ProfileModel;

// -- Post ------------------------------------------------------------------------------

/** The column values of a Post row. */
export interface PostData {
  readonly id: bigint;
  readonly authorId: bigint;
  readonly title: string;
  readonly body: string;
  readonly views: number;
  readonly published: boolean;
  readonly createdAt: Date;
}

/** A Post row. To-one relations are typed on rows of queries that load them. */
export interface Post extends PostData, Instance<PostSpec> {
  readonly comments: RelatedSet<CommentSpec, "postId" | "post">;
  readonly tags: ManyRelatedSet<TagSpec>;
  readonly postTags: RelatedSet<PostTagSpec, "postId" | "post">;
}

export type PostInsert = {
  id?: In<bigint>;
  title: In<string>;
  body: In<string>;
  views?: In<number>;
  published?: In<boolean>;
  createdAt?: In<Date>;
} & (
  | { authorId: In<bigint>; author?: never }
  | { author: { readonly id: In<bigint> }; authorId?: never }
);

export interface PostUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Post" | "~Post", {}>;
  authorId?: In<bigint> | Expression<Compat<bigint>, "Post" | "~Post", {}>;
  author?: { readonly id: In<bigint> };
  title?: In<string> | Expression<Compat<string>, "Post" | "~Post", {}>;
  body?: In<string> | Expression<Compat<string>, "Post" | "~Post", {}>;
  views?: In<number> | Expression<Compat<number>, "Post" | "~Post", {}>;
  published?: In<boolean> | Expression<Compat<boolean>, "Post" | "~Post", {}>;
  createdAt?: In<Date> | Expression<Compat<Date>, "Post" | "~Post", {}>;
}

export interface PostUpdateRow {
  id: In<bigint>;
  authorId?: In<bigint>;
  author?: { readonly id: In<bigint> };
  title?: In<string>;
  body?: In<string>;
  views?: In<number>;
  published?: In<boolean>;
  createdAt?: In<Date>;
}

export interface PostSpec {
  readonly name: "Post";
  readonly row: Post;
  readonly data: PostData;
  readonly insert: PostInsert;
  readonly update: PostUpdate;
  readonly updateRow: PostUpdateRow;
  readonly pk: bigint;
}

export interface PostFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly authorId: Column<O extends true ? bigint | null : bigint, S>;
  readonly title: Column<O extends true ? string | null : string, S>;
  readonly body: Column<O extends true ? string | null : string, S>;
  readonly views: Column<O extends true ? number | null : number, S>;
  readonly published: Column<O extends true ? boolean | null : boolean, S>;
  readonly createdAt: Column<O extends true ? Date | null : Date, S>;
  readonly author: UserPath<S, [...H, Hop<"author", "one", UserSpec>], O>;
  readonly comments: CommentPath<S | Many, [...H, Hop<"comments", "many", CommentSpec>], O>;
  readonly tags: TagPath<S | Many, [...H, Hop<"tags", "m2m", TagSpec>], O>;
  readonly postTags: PostTagPath<S | Many, [...H, Hop<"postTags", "many", PostTagSpec>], O>;
}

export interface PostPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<PostSpec, S, H>,
    PostFields<S, H, O> {}

export interface PostModel extends ModelClass<PostSpec>, PostFields<"Post", [], false> {}

export const Post = models["Post"] as unknown as PostModel;

// -- Comment ---------------------------------------------------------------------------

/** The column values of a Comment row. */
export interface CommentData {
  readonly id: bigint;
  readonly postId: bigint;
  readonly authorId: bigint | null;
  readonly body: string;
  readonly createdAt: Date;
}

/** A Comment row. To-one relations are typed on rows of queries that load them. */
export interface Comment extends CommentData, Instance<CommentSpec> {
}

export type CommentInsert = {
  id?: In<bigint>;
  authorId?: In<bigint | null>;
  author?: { readonly id: In<bigint> } | null;
  body: In<string>;
  createdAt?: In<Date>;
} & (
  | { postId: In<bigint>; post?: never }
  | { post: { readonly id: In<bigint> }; postId?: never }
);

export interface CommentUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Comment" | "~Comment", {}>;
  postId?: In<bigint> | Expression<Compat<bigint>, "Comment" | "~Comment", {}>;
  post?: { readonly id: In<bigint> };
  authorId?: In<bigint | null> | Expression<Compat<bigint | null>, "Comment" | "~Comment", {}>;
  author?: { readonly id: In<bigint> } | null;
  body?: In<string> | Expression<Compat<string>, "Comment" | "~Comment", {}>;
  createdAt?: In<Date> | Expression<Compat<Date>, "Comment" | "~Comment", {}>;
}

export interface CommentUpdateRow {
  id: In<bigint>;
  postId?: In<bigint>;
  post?: { readonly id: In<bigint> };
  authorId?: In<bigint | null>;
  author?: { readonly id: In<bigint> } | null;
  body?: In<string>;
  createdAt?: In<Date>;
}

export interface CommentSpec {
  readonly name: "Comment";
  readonly row: Comment;
  readonly data: CommentData;
  readonly insert: CommentInsert;
  readonly update: CommentUpdate;
  readonly updateRow: CommentUpdateRow;
  readonly pk: bigint;
}

export interface CommentFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly postId: Column<O extends true ? bigint | null : bigint, S>;
  readonly authorId: Column<O extends true ? bigint | null | null : bigint | null, S>;
  readonly body: Column<O extends true ? string | null : string, S>;
  readonly createdAt: Column<O extends true ? Date | null : Date, S>;
  readonly post: PostPath<S, [...H, Hop<"post", "one", PostSpec>], O>;
  readonly author: UserPath<S, [...H, Hop<"author", "opt", UserSpec>], true>;
}

export interface CommentPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<CommentSpec, S, H>,
    CommentFields<S, H, O> {}

export interface CommentModel extends ModelClass<CommentSpec>, CommentFields<"Comment", [], false> {}

export const Comment = models["Comment"] as unknown as CommentModel;

// -- Tag -------------------------------------------------------------------------------

/** The column values of a Tag row. */
export interface TagData {
  readonly id: bigint;
  readonly name: string;
  readonly priority: Priority;
}

/** A Tag row. To-one relations are typed on rows of queries that load them. */
export interface Tag extends TagData, Instance<TagSpec> {
  readonly posts: ManyRelatedSet<PostSpec>;
  readonly postTags: RelatedSet<PostTagSpec, "tagId" | "tag">;
}

export type TagInsert = {
  id?: In<bigint>;
  name: In<string>;
  priority?: In<Priority>;
};

export interface TagUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Tag" | "~Tag", {}>;
  name?: In<string> | Expression<Compat<string>, "Tag" | "~Tag", {}>;
  priority?: In<Priority> | Expression<Compat<Priority>, "Tag" | "~Tag", {}>;
}

export interface TagUpdateRow {
  id: In<bigint>;
  name?: In<string>;
  priority?: In<Priority>;
}

export interface TagSpec {
  readonly name: "Tag";
  readonly row: Tag;
  readonly data: TagData;
  readonly insert: TagInsert;
  readonly update: TagUpdate;
  readonly updateRow: TagUpdateRow;
  readonly pk: bigint;
}

export interface TagFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly name: Column<O extends true ? string | null : string, S>;
  readonly priority: Column<O extends true ? Priority | null : Priority, S>;
  readonly posts: PostPath<S | Many, [...H, Hop<"posts", "m2m", PostSpec>], O>;
  readonly postTags: PostTagPath<S | Many, [...H, Hop<"postTags", "many", PostTagSpec>], O>;
}

export interface TagPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<TagSpec, S, H>,
    TagFields<S, H, O> {}

export interface TagModel extends ModelClass<TagSpec>, TagFields<"Tag", [], false> {}

export const Tag = models["Tag"] as unknown as TagModel;

// -- PostTag ---------------------------------------------------------------------------

/** The column values of a PostTag row. */
export interface PostTagData {
  readonly id: bigint;
  readonly postId: bigint;
  readonly tagId: bigint;
}

/** A PostTag row. To-one relations are typed on rows of queries that load them. */
export interface PostTag extends PostTagData, Instance<PostTagSpec> {
}

export type PostTagInsert = {
  id?: In<bigint>;
} & (
  | { postId: In<bigint>; post?: never }
  | { post: { readonly id: In<bigint> }; postId?: never }
) & (
  | { tagId: In<bigint>; tag?: never }
  | { tag: { readonly id: In<bigint> }; tagId?: never }
);

export interface PostTagUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "PostTag" | "~PostTag", {}>;
  postId?: In<bigint> | Expression<Compat<bigint>, "PostTag" | "~PostTag", {}>;
  post?: { readonly id: In<bigint> };
  tagId?: In<bigint> | Expression<Compat<bigint>, "PostTag" | "~PostTag", {}>;
  tag?: { readonly id: In<bigint> };
}

export interface PostTagUpdateRow {
  id: In<bigint>;
  postId?: In<bigint>;
  post?: { readonly id: In<bigint> };
  tagId?: In<bigint>;
  tag?: { readonly id: In<bigint> };
}

export interface PostTagSpec {
  readonly name: "PostTag";
  readonly row: PostTag;
  readonly data: PostTagData;
  readonly insert: PostTagInsert;
  readonly update: PostTagUpdate;
  readonly updateRow: PostTagUpdateRow;
  readonly pk: bigint;
}

export interface PostTagFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S>;
  readonly postId: Column<O extends true ? bigint | null : bigint, S>;
  readonly tagId: Column<O extends true ? bigint | null : bigint, S>;
  readonly post: PostPath<S, [...H, Hop<"post", "one", PostSpec>], O>;
  readonly tag: TagPath<S, [...H, Hop<"tag", "one", TagSpec>], O>;
}

export interface PostTagPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<PostTagSpec, S, H>,
    PostTagFields<S, H, O> {}

export interface PostTagModel extends ModelClass<PostTagSpec>, PostTagFields<"PostTag", [], false> {}

export const PostTag = models["PostTag"] as unknown as PostTagModel;

