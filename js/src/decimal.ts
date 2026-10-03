/**
 * `Decimal`: exact decimal values for `Decimal` columns, as in Prisma (the class is
 * `decimal.js`'s). They are read as `Decimal`s and written from a `Decimal`, a finite
 * `number` or decimal text.
 */

import DecimalJs from "decimal.js";

export const Decimal = DecimalJs;
export type Decimal = DecimalJs;
