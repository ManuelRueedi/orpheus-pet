import assert from "node:assert/strict";
import test from "node:test";

import {
  choosePanelSide,
  normalizePanelSize,
  PANEL_GAP,
  panelContentRectFor,
} from "../src/panel-placement.js";

const pet = { x: 500, y: 300, w: 148, h: 280 };
const roomyArea = { left: 0, top: 0, right: 1_600, bottom: 1_200 };

test("compact popup stays adjacent and centers vertically beside the pet", () => {
  const compact = { width: 320, height: 92 };
  const rect = panelContentRectFor(pet, "right", roomyArea, compact);

  assert.equal(rect.x - (pet.x + pet.w), PANEL_GAP);
  assert.equal(rect.y, pet.y + ((pet.h - compact.height) / 2));
});

test("full popup uses the same centered anchor without a visible jump", () => {
  const full = { width: 320, height: 260 };
  const rect = panelContentRectFor(pet, "left", roomyArea, full);

  assert.equal(pet.x - (rect.x + full.width), PANEL_GAP);
  assert.equal(rect.y, pet.y + 10);
});

test("above and below placements center the popup across the pet", () => {
  const compact = { width: 200, height: 80 };
  const below = panelContentRectFor(pet, "below", roomyArea, compact);
  const above = panelContentRectFor(pet, "above", roomyArea, compact);

  assert.equal(below.x, pet.x + ((pet.w - compact.width) / 2));
  assert.equal(below.y - (pet.y + pet.h), PANEL_GAP);
  assert.equal(pet.y - (above.y + compact.height), PANEL_GAP);
});

test("side choice uses the compact card's live height", () => {
  const area = { left: 0, top: 0, right: 800, bottom: 500 };
  const nearTopPet = { x: 200, y: 0, w: 148, h: 280 };

  assert.equal(choosePanelSide(nearTopPet, area, { width: 320, height: 92 }), "below");
  assert.equal(choosePanelSide(nearTopPet, area, { width: 320, height: 260 }), "right");
});

test("invalid measurements retain the last trustworthy dimensions", () => {
  assert.deepEqual(
    normalizePanelSize({ width: 0, height: Number.NaN }, { width: 320, height: 96 }),
    { width: 320, height: 96 },
  );
});
