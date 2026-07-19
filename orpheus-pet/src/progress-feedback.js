const GB = 1_000_000_000;

export function formatElapsed(milliseconds) {
  const seconds = Math.max(0, Math.floor((Number(milliseconds) || 0) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
}

export function withElapsed(message, milliseconds) {
  return Number(milliseconds) < 1000
    ? message
    : `${message} · ${formatElapsed(milliseconds)}`;
}

export function modelDownloadMessage({ name, pct, received, total }) {
  const downloaded = Math.max(0, Number(received) || 0);
  const expected = Math.max(0, Number(total) || 0);
  const percent = Math.max(0, Math.min(99, Number(pct) || 0));
  const downloadedGb = (downloaded / GB).toFixed(1);

  // The backend total is an estimate for artifacts whose exact size is not yet
  // known. Never show the nonsensical "3.8/3.5 GB" or a frozen 99% when a
  // release is larger than its estimate.
  if (expected > 0 && downloaded >= expected && percent >= 99) {
    return `Finishing ${name} download · ${downloadedGb} GB`;
  }

  const amount = expected > 0
    ? `${downloadedGb}/${(expected / GB).toFixed(1)} GB`
    : `${downloadedGb} GB`;
  return `Downloading ${name} · ${percent}% · ${amount}`;
}
