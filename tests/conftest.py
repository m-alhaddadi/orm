import os

import pytest

from blog.models import Comment, Post, PostTag, Profile, Tag, User  # noqa: F401  (registers the models)

import orm

DATABASE_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


@pytest.fixture(scope="session")
async def db():
    try:
        database = await orm.connect(DATABASE_URL, max_connections=4)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    await database.drop_tables()
    await database.create_tables()
    yield database
    await database.drop_tables()
    await database.close()


@pytest.fixture
async def clean(db):
    yield db
    await db.execute("TRUNCATE post_tags, tags, profiles, comments, posts, users RESTART IDENTITY CASCADE")
