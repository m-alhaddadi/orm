// Original public lock implementation retained as the benchmark control.
import { blake2b } from "../../js/dist/src/blake2b.js";
import { QueryError, TransactionRequired } from "../../js/dist/src/errors.js";
export async function lock(key, options = {}) {
    if (this.url.startsWith("sqlite://")) {
        throw new QueryError("sqlite does not support advisory locks");
    }
    if (this.tx() === null) {
        throw new TransactionRequired("db.lock() outside a transaction would release the lock at once; run it inside `db.transaction(...)`");
    }
    let k;
    if (typeof key === "string") {
        k = BigInt.asIntN(64, BigInt("0x" + Buffer.from(blake2b(new TextEncoder().encode(key), 8)).toString("hex")));
    }
    else if (typeof key === "bigint" || Number.isSafeInteger(key)) {
        k = BigInt(key);
        if (k !== BigInt.asIntN(64, k)) {
            throw new RangeError("lock key must fit in 64 bits");
        }
    }
    else {
        throw new TypeError(`lock key must be an integer or a string, got ${String(key)}`);
    }
    const { exclusive = true, nowait = false } = options;
    const fn = "pg_" + (nowait ? "try_" : "") + "advisory_xact_lock" + (exclusive ? "" : "_shared");
    const rows = await this.fetchText(`SELECT ${fn}(${k})::text`);
    return !nowait || rows[0]?.[0] === "true";
}
