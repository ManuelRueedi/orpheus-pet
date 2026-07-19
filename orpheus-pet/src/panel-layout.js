export function panelVisibility(state, engineReady) {
  const idle = state === "idle" && engineReady;
  return {
    pickers: state !== "downloading",
    text: idle,
    speak: idle,
    setup: state === "setup",
    confirm: state === "confirm",
    download: state === "downloading",
    utilities: idle,
  };
}
