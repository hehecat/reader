import type { Book } from "@/types/api";

/**
 * 书籍详情客户端持久缓存 (localStorage, TTL 24h, LRU 60 条).
 *
 * warp 服务端不缓存书籍详情 (getBookInfo 每次实时抓源站 ~1-2s), 冷刷新后
 * 首次点击会等这一次抓取. 本缓存让冷开秒渲染: 命中即作 react-query
 * initialData 立即出画面, 后台静默刷新写回; 多设备首开仍需一次实时抓取.
 *
 * 本模块同时收纳「缓存身份 / 首屏可渲染判定」两组**纯函数**(查询键工厂 + 阅读页门控判定).
 * 放这里而不是 useBookData.ts: 本文件只有 type import、没有任何运行时依赖,
 * `node --test` 能直接 import 它做纯逻辑单测; hooks/ 下的模块带 react / @tanstack / `@/` 别名,
 * node 直接跑不起来.
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

// ---------------------------------------------------------------------------
// 查询键工厂: 详情弹窗 (BookDetailDialog) 与阅读页 (useBookData) 共用
//
// 缺陷背景: 两处各自拼 key, 弹窗带 origin、阅读页不带, 同一本书在 react-query
// 缓存里是两条记录 —— 从详情点「开始阅读」进阅读页, bookInfo/chapters 全部 miss,
// 冷目录路径要重抓 15.9-32.9s, 期间整页白屏.
//
// key 形状 = [namespace, bookUrl, source]:
// - 第 2 段 bookUrl: 不同书源的同名书 URL 不同, 天然区分;
// - 第 3 段 source: 同一 bookUrl 在不同书源下抓到的详情/目录可能不同(书源规则不同,
//   后端正是用 bookSource 参数选源), 只靠 URL 会在「换源后」命中旧源数据.
//   源未知时归一化成 "" (而非省略该元素), 让段数恒定、可作前缀匹配.
// ---------------------------------------------------------------------------

/** 源未知时的占位段 (本地书 / 书架还没解析出 origin) */
export const UNKNOWN_SOURCE = "";

/** origin → key 里的源段; undefined / 空串 / 纯空白 一律视为未知 */
export function normalizeSource(origin: string | undefined | null): string {
  return (origin ?? "").trim();
}

/**
 * 书籍详情 key. 注意本地书 (LOCAL_ORIGIN) 要由调用方映射成 undefined 再传进来:
 * 阅读页的 bookSourceUrl 在本地书下就是 undefined, 两边必须落到同一段
 * (本模块不 import LOCAL_ORIGIN, 否则就带上了 hooks/ 的运行时依赖).
 */
export function bookInfoQueryKey(bookUrl: string, origin?: string): [string, string, string] {
  return ["bookInfo", bookUrl, normalizeSource(origin)];
}

/** 目录 key, 规则同上 */
export function chaptersQueryKey(bookUrl: string, origin?: string): [string, string, string] {
  return ["chapters", bookUrl, normalizeSource(origin)];
}

/**
 * 前缀键: 一本书**所有源**的详情/目录.
 * 给 invalidateQueries 的局部匹配用 (换源后旧源条目也该作废),
 * 同时也是历史遗留的 2 段键 —— useChapterContent 取上一章时按 2 段精确读取,
 * 故那边要镜像写一份 (见 useBookData 里 chapters 镜像副作用).
 */
export function bookInfoQueryPrefix(bookUrl: string): [string, string] {
  return ["bookInfo", bookUrl];
}

/** 见 bookInfoQueryPrefix */
export function chaptersQueryPrefix(bookUrl: string): [string, string] {
  return ["chapters", bookUrl];
}

// ---------------------------------------------------------------------------
// 阅读页首屏门控 (纯函数)
//
// 缺陷背景: 以前整页门控只认「书籍元信息到了没」(book === undefined → 整页骨架),
// 于是 IDB 里正文/目录都已就绪的二次进书, 仍要等一次 getBookInfo 实时抓源站
// (~1-2s, 冷路径更久) 才有第一帧 —— 已就绪的正文陪等, 用户看到白屏.
//
// 现在门控只问「有没有东西可画」: 正文 / 目录 / 元信息 任一到位就进阅读框架,
// 正文由 ContentView 自己呈现 pending/错误, 目录由框架内的状态卡呈现,
// 元信息缺失只做降级 (标题占位 + 依赖元信息的动作自行降级), 不再整页挡.
// 正文不会因此脱离目录: 章节仍由 chapters 驱动, 目录缺失时上一章/下一章按
// 「没有相邻章节」降级, 正文定位照旧.
// ---------------------------------------------------------------------------

export type ReaderBootPhase =
  /** 缺 bookUrl 参数: 整页「缺少书籍链接」 */
  | "missing-url"
  /** 正文/目录/元信息三样都没到: 真的没东西可画, 整页 loading (或失败重试) */
  | "blocked"
  /** 进入阅读框架: 有东西可画, 剩下的交给框架内各区域的 pending/降级 */
  | "reading";

export interface ReaderBootInput {
  /** url 里 bookUrl 参数是否有值 */
  hasBookUrl: boolean;
  /** 书籍元信息 (书名/作者/目录配置) 已到 */
  bookReady: boolean;
  /** 已知章节数 (>0 = 目录定位有了) */
  chapterCount: number;
  /** 正文可渲染 (至少一段) 或已明确失败 (isError 也要进框架, 由 ContentView 报错) */
  bodyReady: boolean;
}

export interface ReaderBootDecision {
  phase: ReaderBootPhase;
  /** 元信息缺失: 标题改占位、依赖元信息的动作自行降级 */
  metaDegraded: boolean;
}

export function resolveReaderBoot(input: ReaderBootInput): ReaderBootDecision {
  if (!input.hasBookUrl) {
    return { phase: "missing-url", metaDegraded: false };
  }
  // 实质上门控只看 chapterCount: bodyReady 必然意味着章节已经定位到了.
  // bodyReady 仍单独入参, 是为了让「正文已就绪」这条需求在判定里显式可见.
  if (input.bodyReady || input.chapterCount > 0 || input.bookReady) {
    return { phase: "reading", metaDegraded: !input.bookReady };
  }
  return { phase: "blocked", metaDegraded: false };
}
