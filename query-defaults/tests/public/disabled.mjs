import assert from 'node:assert/strict';
import { define, Registry, SchemaError } from '../../../js/dist/src/index.js';
const registry = new Registry();
const field = { name: 'id', column: 'id', type: 'int', primary_key: true };
define({ models: [{ name: 'Plain', table: 'plain', fields: [field] }] }, { registry });
const before = registry.ir();
assert.throws(() => define({ models: [{ name: 'Bad', table: 'bad', fields: [field] }], behavior: { schema_contract: 1, query_defaults: [{ model: 'Bad', fields: [] }] } }, { registry }), error => error instanceof SchemaError && /query-defaults.*rebuild/.test(error.message));
assert.deepEqual(registry.ir(), before);
console.log('Disabled Node atomic definition: passed');
