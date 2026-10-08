/** Static checks of generated query-set classes (`--query-set`); never run. */

import { Post, Tag, User, type PostSpec } from "./models.js";
import type { PostQueries, TagQueries } from "./queries.js";
import type { QuerySet } from "../../../src/index.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
declare function same<A, B>(...check: Equal<A, B> extends true ? [] : [error: "types differ"]): void;
declare function is<T>(value: T): void;
declare const user: User;
declare const post: Post;

export async function chains() {
  same<ReturnType<typeof Post.objects.published>, PostQueries>();
  const q = Post.objects.filter(Post.views.gt(1)).published().orderBy("-views").popular(5);
  is<PostQueries>(q);
  is<QuerySet<PostSpec>>(q);
  const rows = await q;
  same<typeof rows, Post[]>();
  is<PostQueries>(user.posts.published());
  is<PostQueries>(user.posts.filter(Post.views.gt(1)).popular());
  is<TagQueries>(post.tags.named("py"));
  await Post.objects.published().insert({ title: "t", body: "b", authorId: 1n });
  // @ts-expect-error a model without a query-set class has no custom methods
  User.objects.published();
  // @ts-expect-error unknown custom method
  Post.objects.published().nope();
  // @ts-expect-error typed insert stays
  await Post.objects.published().insert({ title: 1 });
}
