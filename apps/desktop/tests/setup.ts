/**
 * Vitest setup: this jsdom configuration exposes no localStorage (opaque
 * origin), but prefs.ts / primitives/panel.ts persist through it. Install
 * a spec-faithful in-memory Storage so persistence paths are testable;
 * suites call localStorage.clear() between cases.
 */
function memoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
    key: (i: number) => [...map.keys()][i] ?? null,
    removeItem: (k: string) => {
      map.delete(k);
    },
    setItem: (k: string, v: string) => {
      map.set(k, String(v));
    },
  };
}

// Probe instead of typeof: Node 25's global localStorage EXISTS but is
// inert without --localstorage-file (clear/setItem are undefined), which
// the old undefined-check waved through.
function storageUsable(): boolean {
  try {
    const s = globalThis.localStorage;
    if (typeof s?.clear !== "function") return false;
    s.setItem("__setup_probe__", "1");
    s.removeItem("__setup_probe__");
    return true;
  } catch {
    return false;
  }
}

if (!storageUsable()) {
  Object.defineProperty(globalThis, "localStorage", {
    value: memoryStorage(),
    configurable: true,
  });
}
