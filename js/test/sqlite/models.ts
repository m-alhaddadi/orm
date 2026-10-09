// Generated from examples/sqlite/schema.prisma; test fixture uses an isolated registry.
//
// The models are built at runtime from the compiled schema below; the declarations
// give them their static types. Per model: the row type (`User`), the shapes insert /
// update / updateMany take, the columns and relation paths, and the model object.

/* eslint-disable */
import { define, Registry, type Column, type Compat, type Expression, type Hop, type In, type Instance, type JsonValue, type Many, type ManyRelatedSet, type ModelClass, type QuerySetOf, type RelatedSet, type RelationPath, type SchemaIR, type Via } from "../../src/index.js";

const SCHEMA: SchemaIR = {
  "dialect": "sqlite",
  "models": [
    {
      "name": "Author",
      "table": "author",
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
          "unique": true
        },
        {
          "name": "name",
          "column": "name",
          "type": "string"
        },
        {
          "name": "active",
          "column": "active",
          "type": "bool",
          "default": true
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
          "name": "books",
          "kind": "many",
          "target": "Book",
          "from": "id",
          "to": "author_id"
        }
      ]
    },
    {
      "name": "Book",
      "table": "book",
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
          "type": "string"
        },
        {
          "name": "pages",
          "column": "pages",
          "type": "int",
          "default": 0,
          "check": "pages >= 0"
        },
        {
          "name": "status",
          "column": "status",
          "type": "text",
          "enum": "Status",
          "default": "draft"
        },
        {
          "name": "metadata",
          "column": "metadata",
          "type": "json",
          "nullable": true
        }
      ],
      "relations": [
        {
          "name": "author",
          "kind": "one",
          "target": "Author",
          "from": "author_id",
          "to": "id",
          "foreign_key": true,
          "on_delete": "cascade"
        }
      ],
      "indexes": [
        {
          "columns": [
            {
              "field": "title"
            }
          ],
          "where": "pages > 0"
        }
      ]
    }
  ],
  "enums": [
    {
      "name": "Status",
      "db_name": "status",
      "storage": "text",
      "values": [
        {
          "name": "draft",
          "value": "draft"
        },
        {
          "name": "published",
          "value": "published"
        }
      ]
    }
  ]
};

// Bun loads all test files in one process, alongside PostgreSQL models.
export const sqliteRegistry = new Registry();
const models = define(SCHEMA, { registry: sqliteRegistry, requiredCapabilities: ["reference-loading"] });

// -- Status ----------------------------------------------------------------------------

export const Status = {
  draft: "draft",
  published: "published",
} as const;
export type Status = (typeof Status)[keyof typeof Status];

// -- Author ----------------------------------------------------------------------------

/** The column values of a Author row. */
export interface AuthorData {
  readonly id: bigint;
  readonly email: string;
  readonly name: string;
  readonly active: boolean;
  readonly createdAt: Date;
}

/** A Author row. To-one relations are typed on rows of queries that load them. */
export interface Author extends AuthorData, Instance<AuthorSpec> {
  readonly books: RelatedSet<BookSpec, "authorId" | "author">;
}

export type AuthorInsert = {
  id?: In<bigint>;
  email: In<string>;
  name: In<string>;
  active?: In<boolean>;
  createdAt?: In<Date>;
};

export interface AuthorUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Author" | "~Author", {}>;
  email?: In<string> | Expression<Compat<string>, "Author" | "~Author", {}>;
  name?: In<string> | Expression<Compat<string>, "Author" | "~Author", {}>;
  active?: In<boolean> | Expression<Compat<boolean>, "Author" | "~Author", {}>;
  createdAt?: In<Date> | Expression<Compat<Date>, "Author" | "~Author", {}>;
}

export interface AuthorUpdateRow {
  id: In<bigint>;
  email?: In<string>;
  name?: In<string>;
  active?: In<boolean>;
  createdAt?: In<Date>;
}

export interface AuthorSpec {
  readonly name: "Author";
  readonly row: Author;
  readonly data: AuthorData;
  readonly insert: AuthorInsert;
  readonly update: AuthorUpdate;
  readonly updateRow: AuthorUpdateRow;
  readonly pk: bigint;
}

export interface AuthorFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S, "id", H>;
  readonly email: Column<O extends true ? string | null : string, S, "email", H>;
  readonly name: Column<O extends true ? string | null : string, S, "name", H>;
  readonly active: Column<O extends true ? boolean | null : boolean, S, "active", H>;
  readonly createdAt: Column<O extends true ? Date | null : Date, S, "createdAt", H>;
  readonly books: BookPath<S | Many, [...H, Hop<"books", "many", BookSpec>], O>;
}

export interface AuthorPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<AuthorSpec, S, H>,
    AuthorFields<S, H, O> {
  /** The relation query set for `load()`: `Author.objects` bound to this relation. */
  readonly objects: QuerySetOf<AuthorSpec, Author, "Author", {}, never, Via<H, undefined, false, S>>;
}

export interface AuthorModel extends ModelClass<AuthorSpec>, AuthorFields<"Author", [], false> {}

export const Author = models["Author"] as unknown as AuthorModel;

// -- Book ------------------------------------------------------------------------------

/** The column values of a Book row. */
export interface BookData {
  readonly id: bigint;
  readonly authorId: bigint;
  readonly title: string;
  readonly pages: number;
  readonly status: Status;
  readonly metadata: JsonValue | null;
}

/** A Book row. To-one relations are typed on rows of queries that load them. */
export interface Book extends BookData, Instance<BookSpec> {
  loadAuthor(options?: { readonly reload?: boolean }): Promise<Author>;
}

export type BookInsert = {
  id?: In<bigint>;
  title: In<string>;
  pages?: In<number>;
  status?: In<Status>;
  metadata?: In<JsonValue | null>;
} & (
  | { authorId: In<bigint>; author?: never }
  | { author: { readonly id: In<bigint> }; authorId?: never }
);

export interface BookUpdate {
  id?: In<bigint> | Expression<Compat<bigint>, "Book" | "~Book", {}>;
  authorId?: In<bigint> | Expression<Compat<bigint>, "Book" | "~Book", {}>;
  author?: { readonly id: In<bigint> };
  title?: In<string> | Expression<Compat<string>, "Book" | "~Book", {}>;
  pages?: In<number> | Expression<Compat<number>, "Book" | "~Book", {}>;
  status?: In<Status> | Expression<Compat<Status>, "Book" | "~Book", {}>;
  metadata?: In<JsonValue | null> | Expression<Compat<JsonValue | null>, "Book" | "~Book", {}>;
}

export interface BookUpdateRow {
  id: In<bigint>;
  authorId?: In<bigint>;
  author?: { readonly id: In<bigint> };
  title?: In<string>;
  pages?: In<number>;
  status?: In<Status>;
  metadata?: In<JsonValue | null>;
}

export interface BookSpec {
  readonly name: "Book";
  readonly row: Book;
  readonly data: BookData;
  readonly insert: BookInsert;
  readonly update: BookUpdate;
  readonly updateRow: BookUpdateRow;
  readonly pk: bigint;
}

export interface BookFields<S extends string, H extends readonly Hop[], O extends boolean> {
  readonly id: Column<O extends true ? bigint | null : bigint, S, "id", H>;
  readonly authorId: Column<O extends true ? bigint | null : bigint, S, "authorId", H>;
  readonly title: Column<O extends true ? string | null : string, S, "title", H>;
  readonly pages: Column<O extends true ? number | null : number, S, "pages", H>;
  readonly status: Column<O extends true ? Status | null : Status, S, "status", H>;
  readonly metadata: Column<O extends true ? JsonValue | null | null : JsonValue | null, S, "metadata", H>;
  readonly author: AuthorPath<S, [...H, Hop<"author", "one", AuthorSpec>], O>;
}

export interface BookPath<S extends string, H extends readonly Hop[], O extends boolean>
  extends RelationPath<BookSpec, S, H>,
    BookFields<S, H, O> {
  /** The relation query set for `load()`: `Book.objects` bound to this relation. */
  readonly objects: QuerySetOf<BookSpec, Book, "Book", {}, never, Via<H, undefined, false, S>>;
}

export interface BookModel extends ModelClass<BookSpec>, BookFields<"Book", [], false> {}

export const Book = models["Book"] as unknown as BookModel;

