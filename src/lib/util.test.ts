import { describe, expect, it } from 'vitest';
import { countActions, formatBytes, formatDate, shortPath } from './util';

describe('formatBytes', () => {
  it('formats bytes across units', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
    expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe('3.0 GB');
  });
  it('handles invalid input', () => {
    expect(formatBytes(NaN)).toBe('0 B');
    expect(formatBytes(-5)).toBe('0 B');
  });
});

describe('formatDate', () => {
  it('returns unknown for invalid input', () => {
    expect(formatDate(0)).toBe('unknown date');
    expect(formatDate(NaN)).toBe('unknown date');
  });
});

describe('shortPath', () => {
  it('keeps short paths intact', () => {
    expect(shortPath('/etc/nginx.conf')).toBe('/etc/nginx.conf');
  });
  it('truncates long paths with an ellipsis', () => {
    const long = '/home/user/very/deeply/nested/project/config/settings.json';
    const out = shortPath(long, 30);
    expect(out).toContain('…');
    expect(out.length).toBeLessThanOrEqual(31);
  });
});

describe('countActions', () => {
  it('counts create vs overwrite', () => {
    const actions = [
      { kind: 'create' },
      { kind: 'overwrite' },
      { kind: 'overwrite' },
      { kind: 'skip' },
    ];
    expect(countActions(actions)).toEqual({ create: 1, overwrite: 2 });
  });
});
