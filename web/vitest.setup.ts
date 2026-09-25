// Node >= 23 exposes experimental localStorage/sessionStorage globals (method-less
// stubs on Node 25) that shadow jsdom's Storage; install in-memory ones when broken.

function inMemoryStorage(): Storage {
  const store = new Map<string, string>();
  return {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => {
      store.set(key, String(value));
    },
    removeItem: (key: string) => {
      store.delete(key);
    },
    clear: () => {
      store.clear();
    },
    key: (index: number) => [...store.keys()][index] ?? null,
    get length() {
      return store.size;
    },
  };
}

for (const name of ["localStorage", "sessionStorage"] as const) {
  const existing = (globalThis as Record<string, unknown>)[name] as
    | Storage
    | undefined;
  if (typeof existing?.getItem === "function") continue;
  Object.defineProperty(globalThis, name, {
    value: inMemoryStorage(),
    writable: true,
    enumerable: true,
    configurable: true,
  });
}
