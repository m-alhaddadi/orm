# Generated from schema.orm by `python -m orm generate`. Do not edit.
#
# Runtime half of the generated module: the model classes are built from the compiled
# schema below. models.pyi carries the static types (columns, relation paths, typed
# inserts / updates and query sets) for editors and type checkers.

from orm import QuerySet, define

_SCHEMA = r"""
{
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
    }
  ]
}
"""

_models = define(_SCHEMA, module=__name__)
User = _models["User"]
Post = _models["Post"]
Comment = _models["Comment"]

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
