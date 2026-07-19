import assert from "node:assert/strict";
import test from "node:test";

import {
  createPanelWindowController,
  FIRST_LAUNCH_PANEL_KEY,
  openPanelOnFirstLaunch,
} from "../src/panel-window.js";

function fakePanel({ visible = false, showError = null, hideError = null, focusError = null } = {}) {
  return {
    visible,
    showCalls: 0,
    hideCalls: 0,
    focusCalls: 0,
    async show() {
      this.showCalls += 1;
      if (showError) throw showError;
      this.visible = true;
    },
    async hide() {
      this.hideCalls += 1;
      if (hideError) throw hideError;
      this.visible = false;
    },
    async setFocus() {
      this.focusCalls += 1;
      if (focusError) throw focusError;
    },
    async isVisible() {
      return this.visible;
    },
  };
}

const immediateWait = async () => {};
const ignoreWarning = () => {};

function fakeStorage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem: (key) => values.get(key) || null,
    setItem: (key, value) => values.set(key, String(value)),
  };
}

test("first launch opens the panel and records only a confirmed show", async () => {
  const storage = fakeStorage();
  let opens = 0;
  assert.equal(await openPanelOnFirstLaunch({
    storage,
    open: async () => { opens += 1; return true; },
    retryDelays: [0],
  }), true);
  assert.equal(opens, 1);
  assert.equal(storage.getItem(FIRST_LAUNCH_PANEL_KEY), "1");

  assert.equal(await openPanelOnFirstLaunch({
    storage,
    open: async () => { opens += 1; return true; },
    retryDelays: [0],
  }), false);
  assert.equal(opens, 1);
});

test("first launch retries a failed early show without consuming the marker", async () => {
  const storage = fakeStorage();
  let opens = 0;
  const opened = await openPanelOnFirstLaunch({
    storage,
    open: async () => ++opens === 2,
    retryDelays: [0, 1],
    wait: immediateWait,
  });
  assert.equal(opened, true);
  assert.equal(opens, 2);
  assert.equal(storage.getItem(FIRST_LAUNCH_PANEL_KEY), "1");
});

test("first launch remains retryable when every bounded show fails", async () => {
  const storage = fakeStorage();
  assert.equal(await openPanelOnFirstLaunch({
    storage,
    open: async () => false,
    retryDelays: [0, 1],
    wait: immediateWait,
  }), false);
  assert.equal(storage.getItem(FIRST_LAUNCH_PANEL_KEY), null);
});

test("retries a startup lookup race instead of caching a missing panel forever", async () => {
  const panel = fakePanel();
  let lookups = 0;
  let placements = 0;
  const controller = createPanelWindowController({
    lookup: async () => (++lookups < 3 ? null : panel),
    place: async () => { placements += 1; },
    retryDelays: [0, 1, 1],
    wait: immediateWait,
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), true);
  assert.equal(lookups, 3);
  assert.equal(placements, 1);
  assert.equal(panel.showCalls, 1);
  assert.equal(panel.focusCalls, 1);
  assert.equal(controller.isOpen(), true);
});

test("recovers when the first window enumeration throws", async () => {
  const panel = fakePanel();
  let lookups = 0;
  const controller = createPanelWindowController({
    lookup: async () => {
      if (++lookups === 1) throw new Error("window registry not ready");
      return panel;
    },
    place: async () => {},
    retryDelays: [0, 1],
    wait: immediateWait,
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), true);
  assert.equal(lookups, 2);
  assert.equal(panel.showCalls, 1);
});

test("a later open retries after an earlier lookup exhausted its attempts", async () => {
  const panel = fakePanel();
  let available = false;
  let lookups = 0;
  const controller = createPanelWindowController({
    lookup: async () => {
      lookups += 1;
      return available ? panel : null;
    },
    place: async () => {},
    retryDelays: [0],
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), false);
  available = true;
  assert.equal(await controller.open(), true);
  assert.equal(lookups, 2);
  assert.equal(panel.showCalls, 1);
});

test("shows the popup even when monitor placement fails", async () => {
  const panel = fakePanel();
  const warnings = [];
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => { throw new Error("monitor unavailable"); },
    warn: (...args) => warnings.push(args),
  });

  assert.equal(await controller.open(), true);
  assert.equal(panel.showCalls, 1);
  assert.equal(panel.visible, true);
  assert.equal(warnings.length, 1);
});

test("keeps the popup open when focus fails", async () => {
  const panel = fakePanel({ focusError: new Error("focus denied") });
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => {},
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), true);
  assert.equal(panel.visible, true);
  assert.equal(controller.isOpen(), true);
});

test("uses actual visibility when an external close desynchronizes local state", async () => {
  const panel = fakePanel();
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => {},
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), true);
  panel.visible = false; // Rust-side close-to-hide does not update main.js state.
  assert.equal(await controller.toggle(), true);
  assert.equal(panel.showCalls, 2);
  assert.equal(panel.hideCalls, 0);

  assert.equal(await controller.toggle(), true);
  assert.equal(panel.hideCalls, 1);
  assert.equal(controller.isOpen(), false);
});

test("drops a stale handle and reacquires the panel after show fails", async () => {
  const stale = fakePanel({ showError: new Error("stale handle") });
  const healthy = fakePanel();
  let lookups = 0;
  const controller = createPanelWindowController({
    lookup: async () => (++lookups === 1 ? stale : healthy),
    place: async () => {},
    retryDelays: [0],
    warn: ignoreWarning,
  });

  assert.equal(await controller.open(), true);
  assert.equal(lookups, 2);
  assert.equal(stale.showCalls, 1);
  assert.equal(healthy.showCalls, 1);
  assert.equal(healthy.focusCalls, 1);
});

test("force-close hides an externally visible panel without trusting cached state", async () => {
  const panel = fakePanel({ visible: true });
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => {},
    warn: ignoreWarning,
  });

  assert.equal(await controller.toggle(false), true);
  assert.equal(panel.hideCalls, 1);
  assert.equal(panel.visible, false);
});

test("does not report a failed hide as closed", async () => {
  const panel = fakePanel({ visible: true, hideError: new Error("hide denied") });
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => {},
    warn: ignoreWarning,
  });

  assert.equal(await controller.toggle(false), false);
  assert.equal(panel.visible, true);
  assert.equal(controller.isOpen(), true);
});

test("serializes an open followed immediately by close", async () => {
  const panel = fakePanel();
  const controller = createPanelWindowController({
    lookup: async () => panel,
    place: async () => {},
    warn: ignoreWarning,
  });

  const opening = controller.open();
  const closing = controller.close();
  assert.deepEqual(await Promise.all([opening, closing]), [true, true]);
  assert.equal(panel.showCalls, 1);
  assert.equal(panel.hideCalls, 1);
  assert.equal(panel.visible, false);
  assert.equal(controller.isOpen(), false);
});
