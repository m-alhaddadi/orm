/** Selected shape preparation. Public pairs only; helper slots stay untouched. */
import { FileField } from "./index.js";

export function prepareDecoder(fields: ReadonlyMap<string, FileField>, publicFields: readonly (readonly [string, number])[]):
  (values: readonly unknown[], base: number, destination: Record<string, unknown>) => void {
  const slots = publicFields.flatMap(([name, position]) => {
    const field = fields.get(name);
    if (!field) return [];
    if (!Number.isSafeInteger(position) || position < 0) throw new RangeError("public file row slots must be nonnegative integers");
    return [{ name, position, field }];
  });
  return (values, base, destination) => {
    for (const { name, position, field } of slots) destination[name] = field.decode(values[base + position]);
  };
}
