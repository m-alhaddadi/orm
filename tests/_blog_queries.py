"""Query set classes for tests/test_query_api.py; imported lazily by `use_query_set`."""

from __future__ import annotations

from typing_extensions import Self

from blog.models import Post, PostQuerySet, Tag, TagQuerySet


class PostQueries(PostQuerySet):
    def published(self) -> Self:
        return self.filter(Post.published)

    def popular(self, views: int = 10) -> Self:
        return self.filter(Post.views >= views)


class TagQueries(TagQuerySet):
    def named(self, prefix: str) -> Self:
        return self.filter(Tag.name.startswith(prefix))
