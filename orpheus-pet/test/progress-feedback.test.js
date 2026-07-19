import assert from "node:assert/strict";
import test from "node:test";

import {
  formatElapsed,
  modelDownloadMessage,
  withElapsed,
} from "../src/progress-feedback.js";

test("elapsed feedback stays compact while continuing to change", () => {
  assert.equal(formatElapsed(9_900), "9s");
  assert.equal(formatElapsed(123_000), "2m 03s");
  assert.equal(withElapsed("Verifying runtime", 500), "Verifying runtime");
  assert.equal(withElapsed("Verifying runtime", 123_000), "Verifying runtime · 2m 03s");
});

test("a larger-than-estimated model no longer shows a frozen impossible fraction", () => {
  assert.equal(
    modelDownloadMessage({
      name: "English",
      pct: 99,
      received: 3_800_000_000,
      total: 3_500_000_000,
    }),
    "Finishing English download · 3.8 GB",
  );
});

test("normal model progress keeps percentage and byte feedback", () => {
  assert.equal(
    modelDownloadMessage({
      name: "English",
      pct: 94,
      received: 3_800_000_000,
      total: 4_028_683_104,
    }),
    "Downloading English · 94% · 3.8/4.0 GB",
  );
});
