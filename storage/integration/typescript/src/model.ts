/** Selected model setup, shared by generated and runtime-defined hosts. */
import { Registry } from "@orm/storage";
import { FileField, FileFieldError, PreparedFileWrite } from "./index.js";
import { installQueries, type HostModel } from "./orm.js";
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

export function installModel(prototype: object, fields: ReadonlyMap<string, FileField>, registry?: Registry, host?: HostModel): ModelAdapter {
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

// Loaded file fields read as a Reference: the host runs the decoder once per materialized row, without I/O.
function installInstances(meta: HostModel, adapter: ModelAdapter): void {
  const fields = [...adapter.fields];
  meta.addRowDecoder(row => {
    // A partial row has only its loaded fields as own properties.
    for (const [name, field] of fields) if (Object.hasOwn(row, name)) row[name] = field.decode(row[name]);
  });
}
