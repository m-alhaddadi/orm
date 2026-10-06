/** Selected model setup, shared by generated and runtime-defined hosts. */
import { Registry } from "@orm/storage";
import { FileField, FileFieldError, PreparedFileWrite } from "./index.js";
import { installQueries } from "./orm.js";
import { prepareDecoder } from "./decoder.js";

export class ModelAdapter {
  readonly fields: ReadonlyMap<string, FileField>;
  constructor(fields: ReadonlyMap<string, FileField>, private registry?: Registry) { this.fields = new Map(fields); }
  configure(registry: Registry): void { this.registry = registry; }
  prepareWrite(values: Readonly<Record<string, unknown>>, shape: string): PreparedFileWrite {
    if (!this.registry) throw new FileFieldError("configure file storage before preparing uploads");
    return new PreparedFileWrite(values, this.fields, this.registry, { shape });
  }
  decoder(publicFields: readonly (readonly [string, number])[]) { return prepareDecoder(this.fields, publicFields); }
  client(reference: Parameters<Registry["resolve"]>[0]) {
    if (!this.registry) throw new FileFieldError("configure file storage before file operations");
    return this.registry.resolve(reference);
  }
}

export function installModel(prototype: object, fields: ReadonlyMap<string, FileField>, registry?: Registry, host?: unknown): ModelAdapter {
  const adapter = new ModelAdapter(fields, registry);
  const methods = new Map<string, (this: Record<string, unknown>, options?: { expiresIn?: number }) => unknown>();
  for (const [name, field] of fields) {
    methods.set(`${name}SignedUrl`, function(options = {}) {
      const reference = field.reference(this);
      return adapter.client(reference).signedUrl(reference, options);
    });
    methods.set(`${name}Open`, function() {
      const reference = field.reference(this);
      return adapter.client(reference).open(reference);
    });
  }
  for (const name of methods.keys()) if (name in prototype) throw new FileFieldError(`${name}: file method collision`);
  for (const [name, value] of methods) Object.defineProperty(prototype, name, { value });
  if (host) {
    installInstances(host, adapter);
    installQueries(host, adapter);
  }
  return adapter;
}

// Loaded file fields read as a Reference: the prepared decoder runs once per materialized row, without I/O.
function installInstances(meta: any, adapter: ModelAdapter): void {
  const decode = adapter.decoder(meta.fieldList.map((field: { name: string }, position: number) => [field.name, position] as const));
  const instance = meta.instance.bind(meta);
  meta.instance = (values: unknown[], start: number, db: unknown) => {
    const row = instance(values, start, db);
    decode(values, start, row);
    return row;
  };
}
