import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { panelVisibility } from "../src/panel-layout.js";

const panelHtml = readFileSync(new URL("../panel.html", import.meta.url), "utf8");

test("compact setup has two selectors and confirmation has an escape action", () => {
  const picker = panelHtml.match(/id="pickerRows"[\s\S]*?<\/div>/)?.[0] || "";
  const confirmation = panelHtml.match(/id="confirmRow"[\s\S]*?<\/div>\s*<\/div>/)?.[0] || "";
  assert.equal((picker.match(/<select\b/g) || []).length, 2);
  assert.match(panelHtml, /id="setupAction"/);
  assert.match(panelHtml, /id="confirmDl"/);
  assert.match(confirmation, /id="confirmCancel"/);
  assert.doesNotMatch(panelHtml, /id="setupTitle"|id="setupHint"/);
});

test("setup shows only the two model pickers and setup action area", () => {
  assert.deepEqual(panelVisibility("setup", false), {
    pickers: true,
    text: false,
    speak: false,
    setup: true,
    confirm: false,
    download: false,
    utilities: false,
  });
});

test("model confirmation keeps the pair editable without idle utilities", () => {
  assert.deepEqual(panelVisibility("confirm", true), {
    pickers: true,
    text: false,
    speak: false,
    setup: false,
    confirm: true,
    download: false,
    utilities: false,
  });
});

test("download collapses to progress and its stop action", () => {
  assert.deepEqual(panelVisibility("downloading", false), {
    pickers: false,
    text: false,
    speak: false,
    setup: false,
    confirm: false,
    download: true,
    utilities: false,
  });
});

test("speech controls and utilities return only when idle and ready", () => {
  assert.equal(panelVisibility("idle", false).utilities, false);
  assert.deepEqual(panelVisibility("idle", true), {
    pickers: true,
    text: true,
    speak: true,
    setup: false,
    confirm: false,
    download: false,
    utilities: true,
  });
});
