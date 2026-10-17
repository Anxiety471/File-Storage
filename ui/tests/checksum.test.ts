import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { hashFile } from "../src/checksum.ts";
for (const size of [0, 3, 4 * 1024 * 1024 + 17]) {
  test(`SHA-256 agrees with Node for ${size} bytes`, async () => {
    const bytes = Buffer.alloc(size, 0x61);
    assert.equal(await hashFile(new Blob([bytes])), createHash("sha256").update(bytes).digest("hex"));
  });
}
