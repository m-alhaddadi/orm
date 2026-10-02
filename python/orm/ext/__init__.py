"""Database extensions: field types, index helpers and the ``Extension`` they need.

Each module wraps one Postgres extension::

    from orm.ext import citext, pg_trgm, pgvector

    class Doc(Model, table="docs"):
        email = citext.CIText(unique=True)
        title = f.Text()
        embedding = pgvector.Vector(3, nullable=True)

        class Meta:
            indexes = [
                pg_trgm.TrigramIndex("title"),
                pgvector.HnswIndex("embedding", ops="vector_cosine_ops"),
            ]

Using a type, index method or operator class of an extension is enough for the next
migration to ``CREATE EXTENSION`` it: the engine knows which extension provides what
(``native/src/ext.rs``). An extension it doesn't know is declared with
:class:`orm.schema.Extension` and its ``types`` / ``index_methods`` / ``opclasses`` /
``functions``; a module here is the template for adding a new one.
"""
