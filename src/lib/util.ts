/** Formatting helpers for ConfigVault (pure, unit-tested). */

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '0 B';
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

export function formatDate(tsSec: number): string {
  if (!Number.isFinite(tsSec) || tsSec <= 0) return 'unknown date';
  return new Date(tsSec * 1000).toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function shortPath(p: string, max = 52): string {
  if (p.length <= max) return p;
  const parts = p.split('/');
  const tail = parts.slice(-2).join('/');
  return `…/${tail.length < max - 2 ? tail : p.slice(p.length - max)}`;
}

export function countActions(actions: { kind: string }[]): { create: number; overwrite: number } {
  return {
    create: actions.filter((a) => a.kind === 'create').length,
    overwrite: actions.filter((a) => a.kind === 'overwrite').length,
  };
}
