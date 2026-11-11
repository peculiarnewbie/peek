/** Settings and UI state that survive a reload, per browser. */
export const store = {
  get: (key: string): string | null => localStorage.getItem('peek-' + key),
  set: (key: string, value: string | number | boolean): void => {
    localStorage.setItem('peek-' + key, String(value));
  },
};

export const clamp01 = (v: number) => Math.min(Math.max(v, 0), 1);

export const perSec = (kb: number) =>
  kb >= 1024 ? (kb / 1024).toFixed(1) + ' MB/s' : kb.toFixed(kb < 10 ? 1 : 0) + ' KB/s';

export const bytes = (b: number) =>
  b >= 1 << 30 ? (b / (1 << 30)).toFixed(2) + ' GB'
  : b >= 1 << 20 ? (b / (1 << 20)).toFixed(1) + ' MB'
  : (b / 1024).toFixed(b < 10240 ? 1 : 0) + ' KB';

export const ms = (v: number | null) => v == null ? '–' : (v < 10 ? v.toFixed(1) : v.toFixed(0)) + ' ms';

export const duration = (t: number) => {
  const s = Math.floor(t / 1000), m = Math.floor(s / 60), h = Math.floor(m / 60);
  return h ? `${h}h ${m % 60}m` : m ? `${m}m ${s % 60}s` : `${s}s`;
};
