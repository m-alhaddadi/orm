# Generated from schema.prisma by `python -m orm generate`. Do not edit.
#
# Runtime half of the generated module: the model classes are built from the compiled
# schema below. models.pyi carries the static types (columns, relation paths, typed
# inserts / updates and query sets) for editors and type checkers.

from orm import QuerySet, define

_SCHEMA = r"""
{"models":[{"name":"User","table":"users","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"email","column":"email","type":"string","unique":true,"max_length":254},{"name":"name","column":"name","type":"string","max_length":100},{"name":"created_at","column":"created_at","type":"date_time","default_now":true}],"relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"author_id"},{"name":"comments","kind":"many","target":"Comment","from":"id","to":"author_id"},{"name":"profile","kind":"one","target":"Profile","from":"id","to":"user_id"}]},{"name":"Profile","table":"profiles","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"user_id","column":"user_id","type":"big_int","unique":true},{"name":"role","column":"role","type":"string","enum":"Role","default":"member","db_type":"\"role\"","write_sql":"CAST({} AS \"role\")"},{"name":"balance","column":"balance","type":"decimal","default":0,"db_type":"numeric(12, 2)"},{"name":"links","column":"links","type":"string","array":true,"default":[]}],"relations":[{"name":"user","kind":"one","target":"User","from":"user_id","to":"id","foreign_key":true,"on_delete":"cascade"}]},{"name":"Post","table":"posts","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"author_id","column":"author_id","type":"big_int","index":true},{"name":"title","column":"title","type":"string","max_length":200},{"name":"body","column":"body","type":"text"},{"name":"views","column":"views","type":"int","default":0},{"name":"published","column":"published","type":"bool","default":false},{"name":"created_at","column":"created_at","type":"date_time","default_now":true}],"relations":[{"name":"author","kind":"one","target":"User","from":"author_id","to":"id","foreign_key":true,"on_delete":"cascade"},{"name":"comments","kind":"many","target":"Comment","from":"id","to":"post_id"},{"name":"tags","kind":"many","target":"Tag","from":"id","to":"id","through":{"model":"PostTag","source":"post_id","target":"tag_id"}},{"name":"post_tags","kind":"many","target":"PostTag","from":"id","to":"post_id"}],"indexes":[{"columns":[{"field":"author_id"},{"field":"created_at","desc":true}],"where":"published"},{"columns":[{"field":"title","opclass":"gin_trgm_ops"}],"method":"gin"}],"constraints":[{"kind":"check","name":"posts_views_not_negative","expr":"views >= 0"}]},{"name":"Comment","table":"comments","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"post_id","column":"post_id","type":"big_int","index":true},{"name":"author_id","column":"author_id","type":"big_int","nullable":true,"index":true},{"name":"body","column":"body","type":"text"},{"name":"created_at","column":"created_at","type":"date_time","default_now":true}],"relations":[{"name":"post","kind":"one","target":"Post","from":"post_id","to":"id","foreign_key":true,"on_delete":"cascade"},{"name":"author","kind":"one","target":"User","from":"author_id","to":"id","foreign_key":true,"on_delete":"set_null"}]},{"name":"Tag","table":"tags","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"name","column":"name","type":"string","unique":true,"max_length":50},{"name":"priority","column":"priority","type":"int","enum":"Priority","default":2}],"relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"id","through":{"model":"PostTag","source":"tag_id","target":"post_id"}},{"name":"post_tags","kind":"many","target":"PostTag","from":"id","to":"tag_id"}]},{"name":"PostTag","table":"post_tags","fields":[{"name":"id","column":"id","type":"big_int","primary_key":true,"auto_increment":true},{"name":"post_id","column":"post_id","type":"big_int"},{"name":"tag_id","column":"tag_id","type":"big_int","index":true},{"name":"position","column":"position","type":"int","nullable":true}],"relations":[{"name":"post","kind":"one","target":"Post","from":"post_id","to":"id","foreign_key":true,"on_delete":"cascade"},{"name":"tag","kind":"one","target":"Tag","from":"tag_id","to":"id","foreign_key":true,"on_delete":"cascade"}],"constraints":[{"kind":"unique","fields":["post_id","tag_id"]}]}],"enums":[{"name":"Role","db_name":"role","storage":"native","values":[{"name":"member","value":"member"},{"name":"editor","value":"editor"},{"name":"admin","value":"admin"}]},{"name":"Priority","db_name":"priority","storage":"int","values":[{"name":"low","value":1},{"name":"normal","value":2},{"name":"high","value":3}]}]}
"""

_models = define(_SCHEMA, module=__name__, required_capabilities=("reference-loading",))
Role = _models["Role"]
Priority = _models["Priority"]
User = _models["User"]
Profile = _models["Profile"]
Post = _models["Post"]
Comment = _models["Comment"]
Tag = _models["Tag"]
PostTag = _models["PostTag"]

# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.
UserQuerySet = ProfileQuerySet = PostQuerySet = CommentQuerySet = TagQuerySet = PostTagQuerySet = QuerySet
UserInsert = UserUpdate = UserUpdateRow = ProfileInsert = ProfileUpdate = ProfileUpdateRow = PostInsert = PostUpdate = PostUpdateRow = CommentInsert = CommentUpdate = CommentUpdateRow = TagInsert = TagUpdate = TagUpdateRow = PostTagInsert = PostTagUpdate = PostTagUpdateRow = dict

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
