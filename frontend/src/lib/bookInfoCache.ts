import type { Book } from "@/types/api";

/**
 * 书籍详情客户端持久缓存 (localStorage, TTL 24h, LRU 60 条).
 *
 * warp 服务端不缓存书籍详情 (getBookInfo 每次实时抓源站 ~1-2s), 冷刷新后
 * 首次点击会等这一次抓取. 本缓存让冷开秒渲染: 命中即作 react-query
 * initialData 立即出画面, 后台静默刷新写回; 多设备首开仍需一次实时抓取.
 */
const STORAGE_KEY = "reader.bookInfoCache";
const TTL_MS = 24 * 60 * 60 * 1000;
const CAP = 60;

interface Entry {
  book: Book;
  ts: number;
}

type Cache = Record<string, Entry>;

function load(): Cache {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Cache;
  } catch {
    return {};
  }
}

function save(cache: Cache): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(cache));
  } catch {
    // 隐私模式/配额满: 静默降级为无缓存
  }
}

/** 命中且未过期返回 Book, 否则 undefined */
export function getBookInfoCache(url: string): Book | undefined {
  return getBookInfoCacheEntry(url)?.book;
}

/**
 * 命中且未过期返回「书籍 + 写入时间」.
 * 时间戳给 react-query 的 `initialDataUpdatedAt` 用: 只有报了真实年龄, 缓存才能
 * 「立刻出画面 + 立刻 stale 后台刷新」, 否则会被当成刚取到的新数据而挡住刷新.
 */
export function getBookInfoCacheEntry(url: string): { book: Book; ts: number } | undefined {
  if (url.length === 0) {
    return undefined;
  }
  const entry = load()[url];
  if (entry === undefined || Date.now() - entry.ts > TTL_MS) {
    return undefined;
  }
  return entry;
}

/** 写入/更新缓存, 超容量按最旧淘汰 */
export function rememberBookInfo(book: Book | null | undefined): void {
  if (book === null || book === undefined || book.bookUrl.length === 0) {
    return;
  }
  const cache = load();
  cache[book.bookUrl] = { book, ts: Date.now() };
  const keys = Object.keys(cache);
  if (keys.length > CAP) {
    const oldest = keys
      .sort((a, b) => (cache[a]?.ts ?? 0) - (cache[b]?.ts ?? 0))
      .slice(0, keys.length - CAP);
    for (const key of oldest) {
      delete cache[key];
    }
  }
  save(cache);
}
