// Query-set classes of the generated models.ts next to this file (`--query-set`).

import { QuerySet } from "../../../src/index.js";
import { Post, Tag, type PostSpec, type TagSpec } from "./models.js";

export class PostQueries extends QuerySet<PostSpec> {
  published(): this {
    return this.filter(Post.published.eq(true)) as this;
  }

  popular(views = 10): this {
    return this.filter(Post.views.gte(views)) as this;
  }
}

export class TagQueries extends QuerySet<TagSpec> {
  named(prefix: string): this {
    return this.filter(Tag.name.startsWith(prefix)) as this;
  }
}
