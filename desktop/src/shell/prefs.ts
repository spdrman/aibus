// Persisted UI preferences. One versioned key per value, JSON inside, and every read tolerates a
// missing, corrupt or blocked store: a bad value falls back to the default instead of breaking
// start-up. Nothing in here is a credential or a run's content, only layout and theme choices.
const PREFIX = "aibus.desktop.v1.";

export function loadPref<T>(key: string, fallback: T, valid: (v: unknown) => v is T = (_v: unknown): _v is T => true): T {
  try {
    const raw = window.localStorage.getItem(PREFIX + key);
    if (raw === null) return fallback;
    const parsed: unknown = JSON.parse(raw);
    return valid(parsed) ? parsed : fallback;
  } catch {
    return fallback;
  }
}

export function savePref(key: string, value: unknown): void {
  try {
    window.localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // Storage full or blocked: the preference is lost for the next start, the session carries on.
  }
}
