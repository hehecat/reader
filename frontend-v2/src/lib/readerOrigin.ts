const ORIGIN_STORAGE_KEY = "reader.lastOrigin.v1";

export interface ReaderOrigin {
  path: string;
  label: string;
}

let memoryOrigin: ReaderOrigin | null = null;

export function resolveOriginLabel(pathname: string, search = ""): string {
  if (pathname === "/") return "书架";
  if (pathname.startsWith("/discover") || pathname.startsWith("/search")) return "搜索";
  if (pathname.startsWith("/explore") || pathname.startsWith("/library")) return "书海";
  if (pathname.startsWith("/studio/sources") || pathname.startsWith("/sources")) return "书源";
  if (pathname.startsWith("/studio/workbench") || pathname.startsWith("/workbench")) {
    return "调试台";
  }
  if (pathname.startsWith("/studio/purify") || pathname.startsWith("/purify")) return "净化";
  if (pathname.startsWith("/me")) return "我的";
  if (pathname.startsWith("/settings") || pathname.startsWith("/users")) {
    return search.includes("tab=account") || pathname.startsWith("/users") ? "用户管理" : "设置";
  }
  return "书架";
}

export function recordReaderOrigin(pathname: string, search = ""): void {
  if (!pathname || pathname.startsWith("/reader") || pathname.startsWith("/login")) {
    return;
  }
  const entry: ReaderOrigin = {
    path: `${pathname}${search}`,
    label: resolveOriginLabel(pathname, search),
  };
  memoryOrigin = entry;
  try {
    sessionStorage.setItem(ORIGIN_STORAGE_KEY, JSON.stringify(entry));
  } catch {
    // ignore storage access errors
  }
}

export function getReaderOrigin(): ReaderOrigin | null {
  if (memoryOrigin) return memoryOrigin;
  try {
    const raw = sessionStorage.getItem(ORIGIN_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<ReaderOrigin>;
    if (
      parsed &&
      typeof parsed.path === "string" &&
      parsed.path.startsWith("/") &&
      !parsed.path.startsWith("/reader") &&
      !parsed.path.startsWith("/login") &&
      typeof parsed.label === "string" &&
      parsed.label
    ) {
      memoryOrigin = { path: parsed.path, label: parsed.label };
      return memoryOrigin;
    }
  } catch {
    // ignore malformed sessionStorage
  }
  return null;
}

export function clearReaderOrigin(): void {
  memoryOrigin = null;
  try {
    sessionStorage.removeItem(ORIGIN_STORAGE_KEY);
  } catch {
    // ignore storage access errors
  }
}
