const DEFAULT_RETRY_DELAYS = [0, 40, 80, 160, 320];

const defaultWait = (milliseconds) => new Promise((resolve) => {
  setTimeout(resolve, milliseconds);
});

export const FIRST_LAUNCH_PANEL_KEY = "pet.firstLaunchPanel.v2";

export async function openPanelOnFirstLaunch({
  storage,
  open,
  key = FIRST_LAUNCH_PANEL_KEY,
  retryDelays = [0, 750, 1_500],
  wait = defaultWait,
}) {
  try {
    if (storage.getItem(key)) return false;
  } catch { /* opening is still more useful than failing on storage access */ }

  for (const delay of retryDelays) {
    if (delay > 0) await wait(delay);
    let opened = false;
    try { opened = await open(); }
    catch { /* the next bounded attempt can reacquire the panel */ }
    if (!opened) continue;
    try { storage.setItem(key, "1"); }
    catch { /* the panel is open even if persistence is unavailable */ }
    return true;
  }
  return false;
}

export function createPanelWindowController({
  lookup,
  place,
  onOpened = () => {},
  retryDelays = DEFAULT_RETRY_DELAYS,
  wait = defaultWait,
  warn = (...args) => console.warn(...args),
}) {
  let panel = null;
  let open = false;
  let transition = Promise.resolve();

  async function resolvePanel(retry = true) {
    if (panel) return panel;

    let lastError = null;
    const delays = retry ? retryDelays : [0];
    for (const delay of delays) {
      if (delay > 0) await wait(delay);
      try {
        panel = await lookup();
      } catch (error) {
        lastError = error;
        panel = null;
      }
      if (panel) return panel;
    }

    if (lastError) warn("panel window lookup failed:", lastError);
    else if (retry) warn("panel window is not available yet");
    return null;
  }

  function forgetPanel(target) {
    if (panel === target) panel = null;
  }

  async function placeBestEffort(target) {
    try {
      await place(target);
      return true;
    } catch (error) {
      // A monitor query or stale position must not make an otherwise healthy
      // controls window impossible to open.
      warn("panel placement failed; showing at its last position:", error);
      return false;
    }
  }

  async function showPanel(target) {
    await placeBestEffort(target);
    try {
      await target.show();
    } catch (error) {
      // Tauri window handles can go stale across early window creation. Drop
      // the cached handle and resolve the label again before giving up.
      warn("panel show failed; reacquiring its window handle:", error);
      forgetPanel(target);
      target = await resolvePanel(true);
      if (!target) return false;
      await placeBestEffort(target);
      try {
        await target.show();
      } catch (retryError) {
        warn("panel show failed after reacquiring its handle:", retryError);
        forgetPanel(target);
        return false;
      }
    }

    open = true;
    try {
      await target.setFocus();
    } catch (error) {
      warn("panel opened but could not take focus:", error);
    }
    try {
      await onOpened();
    } catch (error) {
      warn("panel opened event failed:", error);
    }
    return true;
  }

  async function openPanel() {
    const target = await resolvePanel(true);
    return target ? showPanel(target) : false;
  }

  async function closePanel(target = null) {
    target ||= await resolvePanel(false);
    if (!target) return false;
    try {
      await target.hide();
      open = false;
      return true;
    } catch (error) {
      warn("panel hide failed:", error);
      forgetPanel(target);
      try {
        open = await target.isVisible();
        return !open;
      } catch {
        open = true; // hiding was not confirmed
        return false;
      }
    }
  }

  async function togglePanel(force) {
    let target = await resolvePanel(true);
    if (!target) return false;

    let visible = open;
    try {
      visible = await target.isVisible();
      open = visible;
    } catch (error) {
      warn("panel visibility check failed; reacquiring its window handle:", error);
      forgetPanel(target);
      target = await resolvePanel(true);
      if (!target) return false;
      try {
        visible = await target.isVisible();
        open = visible;
      } catch (retryError) {
        warn("panel visibility check failed after reacquiring its handle:", retryError);
      }
    }

    return (force ?? !visible) ? showPanel(target) : closePanel(target);
  }

  async function placePanel() {
    if (!open) return false;
    const target = await resolvePanel(false);
    return target ? placeBestEffort(target) : false;
  }

  function enqueue(action) {
    const result = transition.then(action, action).catch((error) => {
      warn("panel transition failed:", error);
      return false;
    });
    transition = result.then(() => undefined, () => undefined);
    return result;
  }

  return {
    open: () => enqueue(openPanel),
    close: () => enqueue(() => closePanel()),
    toggle: (force) => enqueue(() => togglePanel(force)),
    place: () => enqueue(placePanel),
    isOpen: () => open,
  };
}
