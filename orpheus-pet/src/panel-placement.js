export const PET_SIZE = Object.freeze({ width: 148, height: 280 });
export const DEFAULT_PANEL_SIZE = Object.freeze({ width: 320, height: 260 });
export const PANEL_GAP = 8;
export const PANEL_SHADOW_PAD = 16;

function validDimension(value, fallback) {
  const dimension = Number(value);
  return Number.isFinite(dimension) && dimension > 0 ? dimension : fallback;
}

export function normalizePanelSize(size, fallback = DEFAULT_PANEL_SIZE) {
  return {
    width: validDimension(size?.width, fallback.width),
    height: validDimension(size?.height, fallback.height),
  };
}

function clampNum(value, low, high) {
  // A tiny work area can be narrower than the popup. Pinning to the first safe
  // edge is less surprising than producing an invalid clamp range.
  if (high < low) return low;
  return Math.max(low, Math.min(high, value));
}

// Horizontal sides win near corners. The dimensions are the card's current
// painted size, so a compact task can use space that the full editor cannot.
export function choosePanelSide(pet, area, size = DEFAULT_PANEL_SIZE) {
  const panel = normalizePanelSize(size);
  const petWidth = validDimension(pet.w, PET_SIZE.width);
  const petHeight = validDimension(pet.h, PET_SIZE.height);
  const above = pet.y - area.top;
  const below = area.bottom - (pet.y + petHeight);
  const left = pet.x - area.left;
  const right = area.right - (pet.x + petWidth);
  const candidates = [
    { side: "right", near: left, room: right, need: PANEL_GAP + panel.width },
    { side: "left", near: right, room: left, need: PANEL_GAP + panel.width },
    { side: "below", near: above, room: below, need: PANEL_GAP + panel.height },
    { side: "above", near: below, room: above, need: PANEL_GAP + panel.height },
  ];
  candidates.sort((a, b) => a.near - b.near);
  for (const candidate of candidates) {
    if (candidate.room >= candidate.need) return candidate.side;
  }
  return candidates.reduce((best, candidate) => (
    candidate.room - candidate.need > best.room - best.need ? candidate : best
  )).side;
}

// Return the visible card's top-left. Compact cards are centred along the pet
// instead of staying at the top of a formerly full-height transparent window.
export function panelContentRectFor(pet, side, area, size = DEFAULT_PANEL_SIZE) {
  const panel = normalizePanelSize(size);
  const petWidth = validDimension(pet.w, PET_SIZE.width);
  const petHeight = validDimension(pet.h, PET_SIZE.height);
  let x;
  let y;

  if (side === "right") {
    x = pet.x + petWidth + PANEL_GAP;
    y = pet.y + ((petHeight - panel.height) / 2);
  } else if (side === "left") {
    x = pet.x - PANEL_GAP - panel.width;
    y = pet.y + ((petHeight - panel.height) / 2);
  } else if (side === "below") {
    x = pet.x + ((petWidth - panel.width) / 2);
    y = pet.y + petHeight + PANEL_GAP;
  } else {
    x = pet.x + ((petWidth - panel.width) / 2);
    y = pet.y - PANEL_GAP - panel.height;
  }

  if (!area) return { x, y };
  return {
    x: clampNum(x, area.left + PANEL_SHADOW_PAD, area.right - panel.width - PANEL_SHADOW_PAD),
    y: clampNum(y, area.top + PANEL_SHADOW_PAD, area.bottom - panel.height - PANEL_SHADOW_PAD),
  };
}
