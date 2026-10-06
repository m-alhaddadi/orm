import importlib.util
import json
from pathlib import Path
import sys
spec = importlib.util.spec_from_file_location("orm._native", Path(sys.argv[1]).resolve())
module = importlib.util.module_from_spec(spec)
sys.modules["orm._native"] = module
spec.loader.exec_module(module)
import orm
assert "query-defaults" not in json.loads(module.native_artifact())["capabilities"]
registry = orm.Registry()
plain = {"models": [{"name": "Plain", "table": "plain", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": True}]}]}
orm.define(plain, registry=registry)
before = registry.ir()
bad = {"models": [{"name": "Bad", "table": "bad", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": True}]}], "behavior": {"schema_contract": 1, "query_defaults": [{"model": "Bad", "fields": []}]}}
try: orm.define(bad, registry=registry)
except orm.SchemaError as error: assert "query-defaults" in str(error) and "rebuild" in str(error)
else: raise AssertionError("disabled artifact accepted policies")
assert registry.ir() == before
print("Disabled Python capability/atomic definition: passed")
