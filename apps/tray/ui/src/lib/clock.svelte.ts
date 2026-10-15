class Clock {
  now = $state(Date.now());
  constructor() {
    if (typeof window !== 'undefined') setInterval(() => (this.now = Date.now()), 250);
  }
}
export const clock = new Clock();

export function pad(n: number): string {
  return n < 10 ? '0' + n : String(n);
}
export function hms(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`;
}
export function secsLeft(deadline: number): number {
  return Math.max(0, Math.ceil((deadline - clock.now) / 1000));
}
