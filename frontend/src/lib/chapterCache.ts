import type { BookContent } from "@/services/book";
import type { BookChapter } from "@/types/api";

/**
 * 目录 + 最近正文的本地持久缓存 (原生 IndexedDB, 不引第三方依赖).
 *
 * 背景: warp 的目录要回源抓 (慢源 30s 预算, 实测 41s 才失败), 而正文服务端其实早已缓存;
 * 前端此前只有内存 staleTime, 刷新/重进 App 一律冷启动 → 进书「卡死」、断网完全不可读.
 * 本模块把目录 (最近 20 本) 与最近阅读的 20 章正文落盘: 二次进书先用本地数据出画面,
 * 网络刷新在后台跑; 网络失败时缓存即兜底.
 *
 * 结构 (schema v1):
 * - `chapters`  store, keyPath `bookUrl`: { bookUrl, chapters, hash, ts }
 *   索引 `byTs`(ts) 用于「只留最近 20 本」的淘汰; `hash` 是目录指纹, 变了就丢弃该书正文
 *   (换源/大改后 index 与章节的对应关系失效, 串章比慢更糟).
 * - `contents`  store, keyPath `[bookUrl, index]`: { bookUrl, index, chapterUrl, title,
 *   content, nextContentUrl, ts }
 *   索引 `byBookTs`([bookUrl, ts]) 用于「每本只留最近 20 章」—— 裁剪走 getAllKeys
 *   (只取主键, 不解码几十 KB 的正文).
 *
 * 降级: 隐私模式/配额满/浏览器禁用时 `openDb` 解析为 undefined, 所有读写静默变成空操作;
 * 任何异常都在本模块内吞掉, 绝不抛到 UI (调用方一律按「无缓存」处理).
 * 接口/净化/进度这些语义完全不碰 —— 这里只是「同样一份数据的本地副本」.
 */

const DB_NAME = "reader.chapterCache";
/** schema 版本: store/索引结构变化时 +1; 老库在 onupgradeneeded 里按需补齐 */
const DB_VERSION = 1;
const CHAPTER_STORE = "chapters";
const CONTENT_STORE = "contents";
const CHAPTER_BY_TS = "byTs";
const CONTENT_BY_BOOK_TS = "byBookTs";
/** 目录最多保留 20 本 (每本 400~2000 章, 单本约 100~400KB) */
const BOOK_CAP = 20;
/** 每本只保留最近 20 章正文 (每章几十 KB) */
const CONTENT_CAP = 20;
/** 单章正文上限: 超过(异常超长页)就不落盘, 避免一条记录把配额吃满 */
const CONTENT_MAX_CHARS = 400_000;

interface ChapterRecord {
  bookUrl: string;
  chapters: BookChapter[];
  /** 目录指纹: 加更/换源都会变 */
  hash: string;
  /** 写入时间 (淘汰用) */
  ts: number;
}

/** 正文缓存条目 (对外形状) */
export interface CachedContent {
  content: string;
  title: string;
  nextContentUrl?: string;
  /** 章节页 URL: 同一个 index 换了源会指向别的章, 用于校验不串章 */
  chapterUrl: string;
}

interface ContentRecord extends CachedContent {
  bookUrl: string;
  index: number;
  ts: number;
}

let dbPromise: Promise<IDBDatabase | undefined> | undefined;

/** 打开/建库 (幂等): 不可用或出错一律解析为 undefined, 绝不 reject */
function openDb(): Promise<IDBDatabase | undefined> {
  if (dbPromise !== undefined) {
    return dbPromise;
  }
  dbPromise = new Promise((resolve) => {
    try {
      if (typeof indexedDB === "undefined") {
        resolve(undefined);
        return;
      }
      const request = indexedDB.open(DB_NAME, DB_VERSION);
      request.onupgradeneeded = () => {
        const db = request.result;
        if (!db.objectStoreNames.contains(CHAPTER_STORE)) {
          const store = db.createObjectStore(CHAPTER_STORE, { keyPath: "bookUrl" });
          store.createIndex(CHAPTER_BY_TS, "ts");
        }
        if (!db.objectStoreNames.contains(CONTENT_STORE)) {
          const store = db.createObjectStore(CONTENT_STORE, { keyPath: ["bookUrl", "index"] });
          store.createIndex(CONTENT_BY_BOOK_TS, ["bookUrl", "ts"]);
        }
      };
      request.onsuccess = () => resolve(request.result);
      // 隐私模式/配额拒绝/被策略禁用: 静默降级为无缓存
      request.onerror = () => resolve(undefined);
      // 另一个标签页占着旧版本库: 不阻塞, 本次会话按无缓存处理
      request.onblocked = () => resolve(undefined);
    } catch {
      resolve(undefined);
    }
  });
  return dbPromise;
}

function requestResult<T>(request: IDBRequest<T>): Promise<T | undefined> {
  return new Promise((resolve) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(undefined);
  });
}

function transactionDone(tx: IDBTransaction): Promise<boolean> {
  return new Promise((resolve) => {
    tx.oncomplete = () => resolve(true);
    tx.onerror = () => resolve(false);
    tx.onabort = () => resolve(false);
  });
}

/** 目录指纹 (轻量, 不哈希全文): 章数 + 首/末章的 url 与标题, 加更与换源都会变 */
export function hashChapters(chapters: BookChapter[]): string {
  const head = chapters[0];
  const tail = chapters[chapters.length - 1];
  return [
    chapters.length,
    head?.url ?? "",
    head?.title ?? "",
    tail?.url ?? "",
    tail?.title ?? "",
  ].join("|");
}

/** 某本书在本地的正文主键区间 (键游标用, 不解码正文) */
function contentKeysOf(
  store: IDBObjectStore,
  bookUrl: string,
  onKeys: (keys: IDBValidKey[]) => void,
): void {
  const range = IDBKeyRange.bound([bookUrl, 0], [bookUrl, Number.MAX_SAFE_INTEGER]);
  const request = store.index(CONTENT_BY_BOOK_TS).getAllKeys(range);
  request.onsuccess = () => onKeys(request.result as IDBValidKey[]);
}

/** 删掉某本书的全部正文缓存 (淘汰/换源时调用) */
function deleteContentsOfBook(store: IDBObjectStore, bookUrl: string): void {
  contentKeysOf(store, bookUrl, (keys) => {
    for (const key of keys) {
      store.delete(key);
    }
  });
}

/** 每本只留最近 CONTENT_CAP 章 (按 ts 升序淘汰最老的) */
function pruneContents(tx: IDBTransaction, bookUrl: string): void {
  contentKeysOf(tx.objectStore(CONTENT_STORE), bookUrl, (keys) => {
    const excess = keys.length - CONTENT_CAP;
    if (excess <= 0) {
      return;
    }
    for (const key of keys.slice(0, excess)) {
      tx.objectStore(CONTENT_STORE).delete(key);
    }
  });
}

/** 目录只留最近 BOOK_CAP 本, 被淘汰的书连同其正文一起清掉 (不留孤儿) */
function pruneBooks(tx: IDBTransaction): void {
  const store = tx.objectStore(CHAPTER_STORE);
  const request = store.index(CHAPTER_BY_TS).getAllKeys();
  request.onsuccess = () => {
    const keys = request.result as IDBValidKey[];
    const excess = keys.length - BOOK_CAP;
    if (excess <= 0) {
      return;
    }
    for (const key of keys.slice(0, excess)) {
      store.delete(key);
      deleteContentsOfBook(tx.objectStore(CONTENT_STORE), String(key));
    }
  };
}

/** 本会话已落盘的目录指纹: 同一份目录不重复写 (节流) */
const writtenChapterHash = new Map<string, string>();
/** 本会话已落盘的正文 (键 → 正文字数): 内容没变不重复写 */
const writtenContent = new Map<string, number>();

/**
 * 读某本书的目录缓存; 无缓存或不可用时 undefined.
 * 命中后调用方可以立刻渲染目录, 网络刷新在后台并行进行.
 */
export async function getChapters(bookUrl: string): Promise<BookChapter[] | undefined> {
  if (bookUrl === "") {
    return undefined;
  }
  try {
    const db = await openDb();
    if (db === undefined) {
      return undefined;
    }
    const record = await requestResult<ChapterRecord | undefined>(
      db.transaction(CHAPTER_STORE, "readonly").objectStore(CHAPTER_STORE).get(bookUrl),
    );
    const chapters = record?.chapters;
    return Array.isArray(chapters) && chapters.length > 0 ? chapters : undefined;
  } catch {
    return undefined;
  }
}

/** 写目录缓存 (异步落盘, 调用方不必 await); 目录结构变化时顺带丢弃该书正文缓存 */
export async function putChapters(bookUrl: string, chapters: BookChapter[]): Promise<void> {
  if (bookUrl === "" || chapters.length === 0) {
    return;
  }
  const hash = hashChapters(chapters);
  if (writtenChapterHash.get(bookUrl) === hash) {
    return;
  }
  try {
    const db = await openDb();
    if (db === undefined) {
      return;
    }
    // 上一次落盘的指纹: 不同说明目录结构变了, 旧正文的 index 对应关系作废
    const previous = await requestResult<ChapterRecord | undefined>(
      db.transaction(CHAPTER_STORE, "readonly").objectStore(CHAPTER_STORE).get(bookUrl),
    );
    const structureChanged = previous !== undefined && previous.hash !== hash;
    const tx = db.transaction([CHAPTER_STORE, CONTENT_STORE], "readwrite");
    const record: ChapterRecord = { bookUrl, chapters, hash, ts: Date.now() };
    tx.objectStore(CHAPTER_STORE).put(record);
    if (structureChanged) {
      deleteContentsOfBook(tx.objectStore(CONTENT_STORE), bookUrl);
    }
    pruneBooks(tx);
    if (await transactionDone(tx)) {
      writtenChapterHash.set(bookUrl, hash);
    }
  } catch {
    // 静默降级: 写失败只丢缓存, 不影响阅读
  }
}

/**
 * 读某章正文缓存; chapterUrl 不符 (换源后同 index 指向别的章) 视为未命中.
 * 返回 undefined 表示「没有可用副本」, 调用方必须继续走网络.
 */
export async function getContent(
  bookUrl: string,
  index: number,
  chapterUrl?: string,
): Promise<CachedContent | undefined> {
  if (bookUrl === "" || index < 0) {
    return undefined;
  }
  try {
    const db = await openDb();
    if (db === undefined) {
      return undefined;
    }
    const record = await requestResult<ContentRecord | undefined>(
      db.transaction(CONTENT_STORE, "readonly").objectStore(CONTENT_STORE).get([bookUrl, index]),
    );
    if (record === undefined || typeof record.content !== "string" || record.content === "") {
      return undefined;
    }
    const sameChapter =
      chapterUrl === undefined ||
      chapterUrl === "" ||
      record.chapterUrl === "" ||
      record.chapterUrl === chapterUrl;
    if (!sameChapter) {
      return undefined;
    }
    return {
      content: record.content,
      title: record.title,
      nextContentUrl: record.nextContentUrl,
      chapterUrl: record.chapterUrl,
    };
  } catch {
    return undefined;
  }
}

/** 写某章正文缓存 (异步落盘, 调用方不必 await) */
export async function putContent(
  bookUrl: string,
  index: number,
  data: BookContent,
  chapterUrl = "",
): Promise<void> {
  if (bookUrl === "" || index < 0 || data.content === "") {
    return;
  }
  if (data.content.length > CONTENT_MAX_CHARS) {
    return;
  }
  const key = `${bookUrl}\u0000${index}\u0000${chapterUrl}`;
  if (writtenContent.get(key) === data.content.length) {
    return;
  }
  try {
    const db = await openDb();
    if (db === undefined) {
      return;
    }
    const tx = db.transaction(CONTENT_STORE, "readwrite");
    const record: ContentRecord = {
      bookUrl,
      index,
      chapterUrl,
      title: data.title,
      content: data.content,
      nextContentUrl: data.nextContentUrl,
      ts: Date.now(),
    };
    tx.objectStore(CONTENT_STORE).put(record);
    pruneContents(tx, bookUrl);
    if (await transactionDone(tx)) {
      writtenContent.set(key, data.content.length);
    }
  } catch {
    // 静默降级
  }
}

// 进书时立刻要用: 模块一被加载就预热连接 (读目录/正文的 effect 在 React 挂载后才跑,
// 那时才开库会白等一次 open 的往返). 失败无所谓 —— 之后的调用会各自拿到 undefined.
void openDb();
