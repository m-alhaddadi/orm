"""Model composition through the public API; needs an artifact with orm-model-composition selected."""
import json
import os

import pytest
import orm
from orm import _native

pytestmark = pytest.mark.skipif("model-composition" not in json.loads(_native.native_artifact())["capabilities"],
                                reason="requires a model-composition native artifact")

SOURCE = '''
model Person {
 id Int @id @default(autoincrement())
 name String @check("name <> 'BADROOT'")
 @@map("composition06_people")
}
model Employee {
 salary Int @check("salary > 0")
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_employees")
}
model Manager {
 level Int @check("level > 0")
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("composition06_managers")
}
model Customer {
 points Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "customer")
 @@map("composition06_customers")
}
'''


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_composed_create_attach_update_delete(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = orm.Registry()
    models = orm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Person, Employee, Manager, Customer = (models[k] for k in ("Person", "Employee", "Manager", "Customer"))
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        alice = await Manager.objects.using(db).insert(name="Alice", salary=20, level=1)
        assert (alice.name, alice.salary, alice.level) == ("Alice", 20, 1)
        counts = [await m.objects.using(db).count() for m in (Person, Employee, Manager)]
        assert counts == [1, 1, 1]

        # One person is an Employee and a Customer at the same time.
        customer = await Customer.objects.using(db).attach(alice.id, {"points": 5})
        assert (customer.id, customer.name, customer.points) == (alice.id, "Alice", 5)
        with pytest.raises(orm.IntegrityError):
            await Customer.objects.using(db).attach(alice.id, {"points": 6})
        with pytest.raises(orm.IntegrityError):
            await Customer.objects.using(db).attach(alice.id + 100, {"points": 6})
        assert await Customer.objects.using(db).count() == 1

        # A failed leaf insert rolls back its root and middle rows.
        with pytest.raises(orm.IntegrityError):
            await Manager.objects.using(db).insert(name="Bob", salary=10, level=0)
        with pytest.raises(orm.IntegrityError):
            await Manager.objects.using(db).insert(name="BADROOT", salary=10, level=1)
        assert await Person.objects.using(db).count() == 1

        # An inherited-field filter selects the composed rows to update.
        assert await Manager.objects.using(db).filter(Manager.salary == 20).update(level=4) == 1
        assert await Manager.objects.using(db).filter(Manager.salary == 99).update(level=5) == 0
        assert (await Manager.objects.using(db).get(Manager.id == alice.id)).level == 4
        assert (await Employee.objects.using(db).filter(Employee.name == "Alice").get()).salary == 20

        person = await Person.objects.using(db).select_related(Person.employee, Person.customer).get()
        assert (person.employee.salary, person.customer.points) == (20, 5)

        # Deleting a child keeps its parent and the sibling child.
        await Manager.objects.using(db).filter(Manager.id == alice.id).delete()
        counts = [await m.objects.using(db).count() for m in (Person, Employee, Manager, Customer)]
        assert counts == [1, 1, 0, 1]
    finally:
        await db.drop_tables()
        await db.close()
