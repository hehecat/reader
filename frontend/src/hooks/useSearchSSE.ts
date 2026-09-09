import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { getBookInfo } from "@/services/book";
import { stripNullsDeep } from "@/lib/api-client";
import { connectSSE } from "@/lib/sse";
import { searchSSEBatchSchema } from "@/services/search";
import { getBookSourcesLite } from "@/services/sources";
import { useSettingsStore } from "@/stores/settings-store";
import type { SearchBook } from "@/types/api";
/** 简介补全跨搜索缓存: 同一本书不重复回源 (搜索规则缺 intro 是源侧常态) */
const introCache = new Map<string, string>();

/** 搜索进度: done=已搜索的书源数(lastIndex+1), total=书源总数(未知时等于 done) */
export interface SearchProgress {
  done: number;
  total: number;
}

export interface StartSearchOptions {
  /** 并发搜索的书源数, 缺省用后端默认值 */
  concurrentCount?: number;
  /** 起始书源下标, 缺省 -1(从头开始); loadMore 内部用最后的 lastIndex 续搜 */
  lastIndex?: number;
}

export interface UseSearchSSEResult {
  /** 聚合去重后的结果, 流式追加 */
  results: SearchBook[];
  searching: boolean;
  /** 仅搜索中非 null */
  progress: SearchProgress | null;
  error: string | null;
  /** 书源未搜完(还能 loadMore); 书源总数未知时保守为 true */
  hasMore: boolean;
  /** 发起新搜索: 断开旧连接、清空结果 */
  start: (key: string, opts?: StartSearchOptions) => void;
  /** 手动停止(保留已有结果, 不算错误) */
  stop: () => void;
  /** 用最后的 lastIndex 继续搜索下一批书源 */
  loadMore: () => void;
  /** 已搜到的书源游标 (快照保存用) */
  lastIndex: number;
  /** 会话快照恢复: 以已聚合结果直接进入「已停止」态, 不重新起搜 */
  hydrate: (key: string, items: SearchBook[], fromIndex: number) => void;
}
/** 单连接搜索窗口 (与服务端 searchSize 一致): 空轮按窗推进游标, 避免死源区间重复搜 */
const SEARCH_WINDOW = 50;

/**
 * 聚合累加器.
 *
 * 后端在单个 SSE 连接内已按 `书名_作者` 过滤(BookController.searchBookMultiSSE),
 * 但 loadMore 是新连接(resultMap 重置), 且响应里不含 origins 字段(Jackson 忽略),
 * 因此前端跨批次聚合策略:
 * 1. `bookUrl|origin` 完全相同 → 重复数据, 丢弃;
 * 2. `书名_作者` 相同(与后端同一把聚合键)→ 视为同一本书的不同书源,
 *    合并进首见条目: origin 并入 origins 数组(换源列表), 并补齐首见条目缺失的字段;
 * 3. 其余 → 新条目, origins 以自身 origin 起始.
 */
interface Accumulator {
  /** 按到达顺序保存的聚合结果 */
  items: SearchBook[];
  /** 精确去重: `${bookUrl}|${origin}` */
  seen: Set<string>;
  /** 聚合索引: `${name}_${author}` → items 中的位置与当前条目 */
  entries: Map<string, { index: number; book: SearchBook }>;
}

/** 相关性分级: 0 书名完全匹配 → 1 书名包含 → 2 作者包含 → 3 简介包含 → 4 其余字段命中 */
export function searchRelevance(book: SearchBook, keyword: string): number {
  const kw = keyword.trim().toLowerCase();
  if (kw.length === 0) {
    return 4;
  }
  const name = book.name.trim().toLowerCase();
  const author = book.author.trim().toLowerCase();
  const intro = (book.intro ?? "").toLowerCase();
  if (name === kw) return 0;
  if (name.includes(kw)) return 1;
  if (author.includes(kw)) return 2;
  if (intro.includes(kw)) return 3;
  return 4;
}

/** 按相关性稳定排序: 同级的保持到达顺序 (流式体验不跳变) */
export function rankResults(items: readonly SearchBook[], keyword: string): SearchBook[] {
  return items
    .map((book, index) => ({ book, tier: searchRelevance(book, keyword), index }))
    .sort((a, b) => a.tier - b.tier || a.index - b.index)
    .map((entry) => entry.book);
}

/** 合并一本书进累加器, 返回 items 是否发生变化 */
function mergeBook(acc: Accumulator, book: SearchBook): boolean {
  const dupKey = `${book.bookUrl}|${book.origin}`;
  if (acc.seen.has(dupKey)) {
    return false;
  }
  acc.seen.add(dupKey);

  const aggKey = `${book.name}_${book.author}`;
  const entry = acc.entries.get(aggKey);
  if (entry === undefined) {
    const origins =
      book.origins !== undefined && book.origins.length > 0
        ? [...new Set([book.origin, ...book.origins])]
        : [book.origin];
    const originUrls = { ...(book.originUrls ?? {}), [book.origin]: book.bookUrl };
    const item: SearchBook = { ...book, origins, originUrls };
    acc.entries.set(aggKey, { index: acc.items.length, book: item });
    acc.items.push(item);
    return true;
  }

  // 同名同作者的另一书源: 保留首见的 bookUrl 作为代表, origin 并入换源列表
  const previous = entry.book;
  const merged: SearchBook = {
    ...previous,
    origins: [
      ...new Set([...(previous.origins ?? [previous.origin]), ...(book.origins ?? []), book.origin]),
    ],
    originUrls: {
      ...(previous.originUrls ?? {}),
      ...(book.originUrls ?? {}),
      [book.origin]: book.bookUrl,
    },
    kind: previous.kind ?? book.kind,
    coverUrl: previous.coverUrl ?? book.coverUrl,
    intro: previous.intro ?? book.intro,
    wordCount: previous.wordCount ?? book.wordCount,
    latestChapterTitle: previous.latestChapterTitle ?? book.latestChapterTitle,
  };
  entry.book = merged;
  acc.items[entry.index] = merged;
  return true;
}

/** 合并一批增量结果, 返回是否有条目变化 */
/**
 * 多源搜索里每个源返回自家模糊命中(站内推荐/散字匹配都会混进来):
 * 聚合前按「书名/作者/简介包含关键词的全部空白分词」收口, 搜「大主宰」不再混入
 * 「山村小神医」这类三者都不沾边的结果; 按作者搜或换名但简介含关键词的书同样保留.
 */
function matchesSearchKey(book: SearchBook, key: string): boolean {
  const tokens = key
    .toLowerCase()
    .split(/\s+/)
    .filter((token) => token.length > 0);
  if (tokens.length === 0) {
    return true;
  }
  const haystack = `${book.name}\u0000${book.author ?? ""}\u0000${book.intro ?? ""}`.toLowerCase();
  return tokens.every((token) => haystack.includes(token));
}

function mergeBatch(acc: Accumulator, batch: readonly SearchBook[]): boolean {
  let changed = false;
  for (const book of batch) {
    if (mergeBook(acc, book)) {
      changed = true;
    }
  }
  return changed;
}

/** 多源流式搜索: SSE 增量结果聚合去重、进度、停止与自动续搜. 组件卸载时自动断开连接. */
export function useSearchSSE(): UseSearchSSEResult {
  const [results, setResults] = useState<SearchBook[]>([]);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastIndex, setLastIndex] = useState(-1);
  const [activeKey, setActiveKey] = useState<string | null>(null);

  const accRef = useRef<Accumulator>({ items: [], seen: new Set(), entries: new Map() });
  const cancelRef = useRef<(() => void) | null>(null);
  const keyRef = useRef<string | null>(null);
  const lastIndexRef = useRef(-1);
  const concurrentRef = useRef<number | undefined>(undefined);
  const searchingRef = useRef(false);
  searchingRef.current = searching;
  /** 自动续搜: 每连接只搜 searchSize 个源, 流结束后自动接下一批直到搜完/手动停止 */
  const autoRef = useRef(true);
  /** 服务端「没有更多了」= 搜完收口信号 (error 事件载荷); state 镜像供 hasMore 渲染 */
  const noMoreRef = useRef(false);
  const [noMore, setNoMore] = useState(false);
  /** 连续无进展重连计数: 空轮防死循环 */
  const stallRef = useRef(0);
  /** 本轮连接起始游标: 收口时对比判断空轮 */
  const roundFromRef = useRef(-1);

  // 书源总数: 进度 total 与 hasMore 判断依据; 与书源页共享 ["sources"] 缓存
  const { data: sources } = useQuery({
    queryKey: ["sourcesLite"],
    queryFn: () => getBookSourcesLite(),
    enabled: searching || activeKey !== null,
    staleTime: 5 * 60_000,
  });
  // 进度/hasMore 口径 = 启用源数 (搜索只跑启用源; 全量含已禁用会显示 3/303 误导)
  const sourceCount = useMemo(
    () => (sources ?? []).filter((source) => source.enabled).length,
    [sources],
  );

  /** 简介补全: 搜索规则常不给 intro, 详情规则给 — 按当前排序序回源回填, 并发上限 3 */
  const enrichedRef = useRef<Set<string>>(new Set());
  const enrichQueueRef = useRef<string[]>([]);
  const inflightRef = useRef(0);
  const pumpEnrichRef = useRef<() => void>(() => {});
  const pumpEnrich = useCallback(() => {
    const MAX_INFLIGHT = 6;
    while (inflightRef.current < MAX_INFLIGHT && enrichQueueRef.current.length > 0) {
      const bookUrl = enrichQueueRef.current.shift();
      if (bookUrl === undefined) {
        break;
      }
      const item = accRef.current.items.find((entry) => entry.bookUrl === bookUrl);
      if (item === undefined) {
        continue;
      }
      inflightRef.current += 1;
      void getBookInfo({ url: item.bookUrl, bookSourceUrl: item.origin }, { timeout: 8_000 })
        .then((info) => (info.intro ?? "").trim())
        .catch(() => "")
        .then((intro) => {
          inflightRef.current -= 1;
          if (intro.length > 0) {
            introCache.set(bookUrl, intro);
            for (const agg of accRef.current.entries.values()) {
              if (agg.book.bookUrl === bookUrl) {
                agg.book = { ...agg.book, intro };
                accRef.current.items[agg.index] = agg.book;
              }
            }
            setResults(rankResults(accRef.current.items, keyRef.current ?? ""));
          }
          pumpEnrichRef.current();
        });
    }
  }, []);
  pumpEnrichRef.current = pumpEnrich;

  /** 把缺简介的结果按当前相关性序入队 (顶部卡片优先补) */
  const enqueueEnrich = useCallback(() => {
    const ranked = rankResults(accRef.current.items, keyRef.current ?? "");
    for (const item of ranked) {
      if ((item.intro ?? "").trim().length > 0) {
        continue;
      }
      const cached = introCache.get(item.bookUrl);
      if (cached !== undefined) {
        for (const agg of accRef.current.entries.values()) {
          if (agg.book.bookUrl === item.bookUrl) {
            agg.book = { ...agg.book, intro: cached };
            accRef.current.items[agg.index] = agg.book;
          }
        }
        continue;
      }
      if (enrichedRef.current.has(item.bookUrl)) {
        continue;
      }
      enrichedRef.current.add(item.bookUrl);
      enrichQueueRef.current.push(item.bookUrl);
    }
    pumpEnrich();
  }, [pumpEnrich]);

  const connectRef = useRef<(key: string, fromIndex: number, concurrentCount?: number) => void>(
    () => {},
  );

  const connect = useCallback((key: string, fromIndex: number, concurrentCount?: number) => {
    // 切换关键词/续搜前必须断开旧连接
    cancelRef.current?.();
    cancelRef.current = null;

    keyRef.current = key;
    lastIndexRef.current = fromIndex;
    roundFromRef.current = fromIndex;
    setActiveKey(key);
    setLastIndex(fromIndex);
    setSearching(true);
    setError(null);

    cancelRef.current = connectSSE(
      "/reader3/searchBookMultiSSE",
      {
        key,
        lastIndex: fromIndex,
        concurrentCount,
        searchSize: SEARCH_WINDOW,
        // 运行时超时(设置页即时保存): 后端 clamp 3..60
        timeout: useSettingsStore.getState().searchTimeout,
      },
      {
        onData: (payload) => {
          // warp serde 对 SearchBook 的 Option 字段显式输出 null, 先归一再进 zod
          const parsed = searchSSEBatchSchema.safeParse(stripNullsDeep(payload));
          if (!parsed.success) {
            return; // 无法解析的批次直接忽略
          }
          // 批次按并发完成顺序到达, lastIndex 可能乱序: 游标取历史最大值,
          // 避免 loadMore 从偏小的游标重搜已覆盖的书源
          const nextIndex = Math.max(lastIndexRef.current, parsed.data.lastIndex);
          lastIndexRef.current = nextIndex;
          setLastIndex(nextIndex);
          const relevant = parsed.data.data.filter((item) =>
            matchesSearchKey(item, keyRef.current ?? ""),
          );
          if (mergeBatch(accRef.current, relevant)) {
            setResults(rankResults(accRef.current.items, keyRef.current ?? ""));
            enqueueEnrich();
          }
        },
        onEnd: () => {
          // 收口与续搜统一在 onClose 处理 (end/error/中断都会走到)
        },
        onError: (err) => {
          cancelRef.current = null;
          if (err.message.includes("没有更多")) {
            noMoreRef.current = true;
            setNoMore(true);
            return;
          }
          // 原生中断不弹错误: onClose 会自动续接; 其余真错误才展示
          if (err.message !== "SSE 连接中断") {
            setSearching(false);
            setError(err.message.length > 0 ? err.message : "搜索失败");
          }
        },
        onClose: () => {
          if (!autoRef.current || keyRef.current === null || noMoreRef.current) {
            setSearching(false);
            return;
          }
          window.setTimeout(() => {
            if (!autoRef.current || keyRef.current === null || noMoreRef.current) {
              setSearching(false);
              return;
            }
            const advanced = lastIndexRef.current > roundFromRef.current;
            // 空轮(死源区间)无数据批次: 游标按窗推进, 不重复搜同一区间
            const nextFrom = advanced
              ? lastIndexRef.current
              : roundFromRef.current + SEARCH_WINDOW;
            if (!advanced) {
              stallRef.current += 1;
            } else {
              stallRef.current = 0;
            }
            if (stallRef.current >= 10) {
              setSearching(false);
              return;
            }
            lastIndexRef.current = nextFrom;
            setLastIndex(nextFrom);
            connectRef.current(keyRef.current, nextFrom, concurrentRef.current);
          }, 300);
        },
      },
    );
  }, [enqueueEnrich]);

  connectRef.current = connect;

  const start = useCallback(
    (key: string, opts?: StartSearchOptions) => {
      const trimmed = key.trim();
      if (trimmed.length === 0) {
        setError("请输入搜索关键词");
        return;
      }
      accRef.current = { items: [], seen: new Set(), entries: new Map() };
      enrichedRef.current = new Set();
      enrichQueueRef.current = [];
      autoRef.current = true;
      noMoreRef.current = false;
      setNoMore(false);
      stallRef.current = 0;
      setResults([]);
      concurrentRef.current = opts?.concurrentCount;
      connect(trimmed, opts?.lastIndex ?? -1, opts?.concurrentCount);
    },
    [connect],
  );

  const stop = useCallback(() => {
    autoRef.current = false;
    cancelRef.current?.();
    cancelRef.current = null;
    setSearching(false);
  }, []);

  const loadMore = useCallback(() => {
    const key = keyRef.current;
    if (key === null || searchingRef.current) {
      return;
    }
    autoRef.current = true;
    noMoreRef.current = false;
    stallRef.current = 0;
    connect(key, lastIndexRef.current, concurrentRef.current);
  }, [connect]);

  /** 会话快照恢复: 阅读器预览返回后保留原搜索列表, 不重新起搜 */
  const hydrate = useCallback(
    (key: string, items: SearchBook[], fromIndex: number) => {
      cancelRef.current?.();
      cancelRef.current = null;
      // 旧快照可能存于过滤规则上线前: 恢复时同样收口
      const clean = items.filter((item) => matchesSearchKey(item, key));
      const seen = new Set<string>();
      const entries = new Map<string, { index: number; book: SearchBook }>();
      clean.forEach((item, index) => {
        seen.add(`${item.bookUrl}|${item.origin}`);
        entries.set(`${item.name}_${item.author}`, { index, book: item });
      });
      accRef.current = { items: clean, seen, entries };
      enrichedRef.current = new Set();
      enrichQueueRef.current = [];
      keyRef.current = key;
      lastIndexRef.current = fromIndex;
      setActiveKey(key);
      setLastIndex(fromIndex);
      setSearching(false);
      setError(null);
      setResults(clean);
    },
    [],
  );

  // 离开页面自动断开
  useEffect(
    () => () => {
      autoRef.current = false;
      cancelRef.current?.();
      cancelRef.current = null;
    },
    [],
  );

  const progress = useMemo<SearchProgress | null>(() => {
    if (!searching) {
      return null;
    }
    const done = Math.max(lastIndex + 1, 0);
    return { done, total: sourceCount > 0 ? Math.max(sourceCount, done) : done };
  }, [searching, lastIndex, sourceCount]);

  const hasMore = useMemo(() => {
    if (activeKey === null || noMore) {
      return false;
    }
    if (sourceCount === 0) {
      return true; // 书源总数未知时不阻塞用户续搜
    }
    return lastIndex < sourceCount - 1;
  }, [activeKey, noMore, lastIndex, sourceCount]);

  return { results, searching, progress, error, hasMore, start, stop, loadMore, hydrate, lastIndex };
}
