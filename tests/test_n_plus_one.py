"""`orm.debug.n_plus_one`: find N+1 query patterns by statement shape."""
import asyncio
import warnings

import pytest
import orm
from orm import debug

pytest_plugins = ["orm.testing", "pytester"]

SOURCE = '''datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String
  customers Customer[]
}
model Customer {
  id        Int    @id
  person_id Int
  person    Person @relation(fields: [person_id], references: [id])
}
'''


@pytest.fixture
async def shop():
    registry = orm.Registry()
    models = orm.loads(SOURCE, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    Person, Customer = models["Person"], models["Customer"]
    await Person.objects.using(db).insert_many([{"id": i, "name": f"p{i}"} for i in range(8)])
    await Customer.objects.using(db).insert_many([{"id": i, "person_id": i} for i in range(8)])
    yield Person, Customer, db
    await db.close()


async def test_a_loader_in_a_loop_raises_with_the_shape_the_site_and_the_fix(shop):
    Person, Customer, db = shop
    customers = await Customer.objects.using(db).order_by(Customer.id)
    with pytest.raises(debug.NPlusOne) as raised:
        with debug.n_plus_one(threshold=5, fail=True):
            for c in customers:
                await c.load_person()  # the call site
    report = raised.value.report
    [shape] = report.repeated
    assert shape.count == 8
    assert shape.sql.startswith('SELECT ') and '"person"' in shape.sql and "?" in shape.sql and "7" not in shape.sql
    assert shape.site.endswith("test_n_plus_one.py:46")
    assert shape.fix == "select_related(Customer.person)"
    assert "8 queries with one shape `SELECT " in str(raised.value)
    assert "use select_related(Customer.person)" in str(raised.value)


async def test_a_related_query_in_a_loop_names_prefetch_related(shop):
    Person, Customer, db = shop
    people = await Person.objects.using(db).order_by(Person.id)
    with pytest.warns(debug.NPlusOneWarning, match=r"use prefetch_related\(Person.customers\)"):
        with debug.n_plus_one(threshold=3) as report:
            for p in people:
                await p.customers.using(db)
    assert report.repeated[0].count == 8


async def test_queries_under_the_threshold_tasks_and_distinct_shapes(shop):
    Person, Customer, db = shop
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        with debug.n_plus_one(threshold=8, fail=True) as report:
            for i in range(8):
                await Person.objects.using(db).get(Person.id == i)
            await Person.objects.using(db).count()
        assert sorted(s.count for s in report.shapes.values()) == [1, 8]
    # Tasks started in the scope count, and so do inserts by model and fields.
    with pytest.warns(debug.NPlusOneWarning, match="3 queries"), debug.n_plus_one(threshold=2) as report:
        await asyncio.gather(*(Person.objects.using(db).filter(Person.id == i).exists() for i in range(3)))
        for i in range(3):
            await Person.objects.using(db).insert(id=100 + i, name="n")
    assert sorted(s.count for s in report.repeated) == [3, 3]
    # Outside a scope nothing is counted.
    await Person.objects.using(db).count()
    assert sum(s.count for s in report.shapes.values()) == 6
    with pytest.raises(ValueError):
        with debug.n_plus_one(threshold=0):
            pass


async def test_the_pytest_fixture_counts_the_test(shop, n_plus_one):
    Person, Customer, db = shop
    for i in range(3):
        await Person.objects.using(db).get(Person.id == i)
    assert [s.count for s in n_plus_one.shapes.values()] == [3]
    n_plus_one.threshold = 3


def test_the_pytest_fixture_fails_a_test_with_an_n_plus_one(pytester):
    pytester.makeconftest('pytest_plugins = ["orm.testing"]')
    pytester.makepyfile(f'''
import orm

SOURCE = {SOURCE!r}

async def test_loop(n_plus_one):
    registry = orm.Registry()
    Person = orm.loads(SOURCE, registry=registry)["Person"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    for i in range(6):
        await Person.objects.using(db).filter(Person.id == i).first()
    await db.close()
''')
    result = pytester.runpytest("-p", "no:cacheprovider", "-o", "asyncio_mode=auto")
    result.assert_outcomes(passed=1, errors=1)
    result.stdout.fnmatch_lines(["*N+1 queries in the test:*", "*6 queries with one shape*"])
