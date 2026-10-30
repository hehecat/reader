import { useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useCallback, useMemo, useEffect, useRef, useState } from "react";

import {
  bookInfoQueryKey,
  bookInfoQueryPrefix,
  chaptersQueryKey,
  chaptersQueryPrefix,
  getBookInfoCache,
  getBookInfoCacheEntry,
  rememberBookInfo,
} from "@/lib/bookInfoCache";
import { getChapters, putChapters } from "@/lib/chapterCache";
import { getBookInfo, getChapterList } from "@/services/book";
import {
  BOOKMARK_CACHE_TTL_MS,
  bookmarksOfBook,
  bookmarksQueryKey,
  getBookmarks,
  type Bookmark,
} from "@/services/bookmarks";
import { getBookshelf } from "@/services/bookshelf";
import { BOOKS_QUERY_KEY, LOCAL_ORIGIN } from "@/hooks/useBookshelf";
import type { Book, BookChapter } from "@/types/api";

export interface UseBookDataResult {
  /** 书籍信息 (含 durChapterIndex/durChapterPos, 用于进度恢复); 加载中为 undefined */
  book: Book | undefined;
  /** 章节列表: 网络/内存缓存优先, 其次 IndexedDB 持久缓存; 都没有时为空数组 */
  chapters: BookChapter[];
  /** 本书书签 (warp getBookmarks 按 bookUrl 返回, 已按阅读顺序排序) */
  bookmarks: Bookmark[];
  /** 书架书记录的书源地址 (warp 各抓取接口必传 bookSource 的取值); 不在架/本地书为 undefined */
  bookSourceUrl: string | undefined;
  /** 书是否在书架 (换源等书架专属操作的门槛) */
  inShelf: boolean;
  bookQuery: UseQueryResult<Book, Error>;
  /** 书架记录: 阅读进度的权威来源(getBookInfo 不返回 durChapter*) */
  shelfBook: Book | undefined;
  /** 书架查询: 需要重取权威进度(如多端同步检查)时 refetch 它 */
  shelfQuery: UseQueryResult<Book[], Error>;
  /** 目录查询: isLoading 只反映网络请求, 不代表「没目录」—— 判断空目录请用 chapters.length */
  chaptersQuery: UseQueryResult<BookChapter[], Error>;
  bookmarksQuery: UseQueryResult<Bookmark[], Error>;
  /** 手动刷新目录: bypass warp 目录缓存 (refresh=1) 并写回查询缓存 + 本地持久缓存 */
  refreshChapters: () => Promise<BookChapter[]>;
}

/** 目录为空时的稳定引用 (避免每次渲染都新建数组) */
const EMPTY_CHAPTERS: BookChapter[] = [];

/**
 * 等书架列表补 origin 的窗口 (ms).
 * 书架记录是 warp 选源参数最可靠的来源, 但它只是「补参数」—— 断网时 getBookshelf 会挂到
 * 请求超时, 绝不能让它把阅读页钉在「正在加载书籍信息…」上, 故到点就走, 交给服务端默认源。
 */
const ORIGIN_WAIT_MS = 600;

/**
 * 阅读器基础数据: 书籍信息 + 章节列表 + 书签.
 * bookUrl 为空时查询都不发起 (页面层负责呈现「缺少书籍链接」态).
 */
export function useBookData(bookUrl: string, sourceHint?: string): UseBookDataResult {
  const enabled = bookUrl !== "";

  // 书架列表: 只用来补 warp 选源参数 bookSource (书架书的 origin 最可靠),
  // 不再挡在书籍详情/目录前面 (见 ready)。
  const shelfQuery = useQuery({
    queryKey: BOOKS_QUERY_KEY,
    queryFn: () => getBookshelf(),
    enabled,
    staleTime: 30_000,
  });
  const shelfBook = useMemo(
    () => shelfQuery.data?.find((item) => item.bookUrl === bookUrl),
    [shelfQuery.data, bookUrl],
  );
  // 源解析优先级: 书架记录 origin > 阅读器 URL 携带的 bookSource(搜索预览直达, 未入架)
  // > 24h 书籍详情缓存里的 origin (回访用户冷启动也有源可用, 不必等书架接口)
  const bookSourceUrl = useMemo(() => {
    const origin = shelfBook?.origin ?? (sourceHint || undefined) ?? getBookInfoCache(bookUrl)?.origin;
    return origin === undefined || origin === LOCAL_ORIGIN ? undefined : origin;
  }, [shelfBook, sourceHint, bookUrl]);

  // 书架数据只补 origin: 拿到就用, 拿不到 (断网/接口挂住) 等 ORIGIN_WAIT_MS 也照常起跑 ——
  // 书籍详情/目录的起跑条件不再依赖「整个书架列表就绪」。
  // 这里的 setState 在定时器里触发 (bookUrl 变化即天然复位), 不在 effect 体内同步改状态。
  const [originWaitBookUrl, setOriginWaitBookUrl] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setOriginWaitBookUrl(bookUrl), ORIGIN_WAIT_MS);
    return () => window.clearTimeout(timer);
  }, [bookUrl]);
  const originWaitOver = originWaitBookUrl === bookUrl;
  const ready = enabled && (bookSourceUrl !== undefined || !shelfQuery.isPending || originWaitOver);

  // 24h 客户端详情缓存: 只当「占位」用, 且必须书源对得上 —— 换源后的封面/书名/简介
  // 与当前源无关, 拿来占位等于先闪一份错内容, 再把正确数据换上去.
  const cachedInfo = useMemo(() => {
    const entry = getBookInfoCacheEntry(bookUrl);
    if (entry === undefined) {
      return undefined;
    }
    const cachedOrigin = entry.book.origin;
    if (
      bookSourceUrl !== undefined &&
      cachedOrigin !== undefined &&
      cachedOrigin !== "" &&
      cachedOrigin !== bookSourceUrl
    ) {
      return undefined;
    }
    return entry;
  }, [bookUrl, bookSourceUrl]);

  /** initialData 与它的「真实年龄」: 书架记录取书架列表的取回时间, 其余取缓存写入时间 */
  const initialBook = shelfBook ?? cachedInfo?.book;
  const initialBookUpdatedAt =
    shelfBook !== undefined ? shelfQuery.dataUpdatedAt : cachedInfo?.ts;

  const bookQuery = useQuery({
    // 键工厂与详情弹窗共用 (lib/bookInfoCache): 同一本书同一书源在弹窗里取过的详情,
    // 进阅读页直接命中, 不再重抓 1~2s。
    queryKey: bookInfoQueryKey(bookUrl, bookSourceUrl),
    queryFn: () => {
      const promise =
        bookSourceUrl === undefined ? getBookInfo(bookUrl) : getBookInfo({ url: bookUrl, bookSourceUrl });
      void promise.then(rememberBookInfo).catch(() => {});
      return promise;
    },
    enabled: ready,
    // 书架书: 书架记录即完整 Book; 其余书: 24h 客户端详情缓存(回访秒渲染)。
    // 缓存新鲜期内直接复用, 不再「每次挂载必回源」—— 详情要 1~2s 且会和目录抢带宽;
    // 只有过期 (staleTime) 后才在后台刷新, initialData 兜着, 刷新不阻塞渲染。
    // 跨设备的最新进度不靠它: 进度只认书架记录 (见 shelfQuery / ReaderPage 的补取)。
    initialData: initialBook,
    // 必须一起给: 只给 initialData 时 react-query 把这份数据当成「刚刚取回的」,
    // 于是 24h 前的缓存也白拿 5 分钟新鲜期, 期间网络新数据回不来 (改了封面/书名也不刷新).
    // 报上真实年龄后, 旧数据照样立刻出画面, 但立刻是 stale → 挂载即后台静默刷新.
    initialDataUpdatedAt: initialBookUpdatedAt,
    staleTime: 5 * 60_000,
  });

  // 目录持久缓存 (IndexedDB): 挂载时异步读一次, 命中则先渲染目录, 网络刷新照常在后台跑。
  // initialData 用不上 —— IndexedDB 读是异步的, 而 initialData 只能同步提供, 故走
  // 「查询数据 ?? 持久缓存」的合并: 语义等同「缓存优先 + 后台静默刷新」,
  // 网络失败时也不会把已缓存的目录清空 (断网仍可读)。
  const [cachedChapters, setCachedChapters] = useState<
    { bookUrl: string; chapters: BookChapter[] } | undefined
  >(undefined);
  useEffect(() => {
    if (bookUrl === "") {
      return;
    }
    let cancelled = false;
    void getChapters(bookUrl).then((chapters) => {
      if (!cancelled && chapters !== undefined) {
        setCachedChapters({ bookUrl, chapters });
      }
    });
    return () => {
      cancelled = true;
    };
  }, [bookUrl]);
  const cachedChaptersOfBook =
    cachedChapters !== undefined && cachedChapters.bookUrl === bookUrl
      ? cachedChapters.chapters
      : undefined;

  const chaptersQuery = useQuery({
    // 键带书源 (见 lib/bookInfoCache.chaptersQueryKey): 详情弹窗用的是同一个工厂,
    // 「详情 → 开始阅读」不再把整份目录重拉一遍 (冷目录路径 15.9-32.9s)。
    queryKey: chaptersQueryKey(bookUrl, bookSourceUrl),
    queryFn: async () => {
      // 普通路径就是 refresh=0 (服务端优先用目录缓存); 只有当用户显式点「刷新目录」才
      // 走 refreshChapters(refresh=1), 两者互不影响。
      const list = await getChapterList(bookUrl, undefined, { bookSourceUrl });
      // 落盘只在「选源提示已确定」时做: origin 还没到就取到的目录可能是服务端默认源的,
      // 写进 IndexedDB 会被下次进书当成这本书的目录渲染 (且 staleTime 内不会自愈)。
      // `local://` 的书没有 origin, 但目录就是本地解析结果, 照常持久化。
      if (bookSourceUrl !== undefined || bookUrl.startsWith("local://")) {
        // 落盘是异步的 (IndexedDB 本身异步), 不阻塞本次渲染
        void putChapters(bookUrl, list);
      }
      return list;
    },
    enabled: ready,
    staleTime: 5 * 60_000,
    // 目录是「离线可读」的核心数据: 别让默认 5 分钟 gc 在关页面时清掉
    gcTime: 7 * 24 * 60 * 60_000,
  });

  // warp 书签按书维度获取 (getBookmarks 必传 bookUrl), 缓存键随书分;
  // 增删只本地写回缓存 (见 useBookmarkActions), staleTime 内不自动 refetch
  const bookmarksQuery = useQuery({
    queryKey: bookmarksQueryKey(bookUrl),
    queryFn: () => getBookmarks(bookUrl),
    enabled,
    staleTime: BOOKMARK_CACHE_TTL_MS,
  });
  const queryClient = useQueryClient();

  // 旧 2 段目录键的镜像写: useChapterContent 取「上一章」时按 2 段键**精确**读取
  // (src/hooks/useChapterContent.ts:266, 不在本次改动范围内)。新键把书源编进去之后,
  // 2 段条目不会自然存在, 不镜像就等于静默丢掉上一章预取 (翻回上一章要现抓)。
  // 2 段键正好是新键的前缀, 故下面失效/换源时的 prefix 匹配会同时命中两者。
  const chaptersData = chaptersQuery.data;
  useEffect(() => {
    if (chaptersData === undefined) {
      return;
    }
    queryClient.setQueryData<BookChapter[]>(chaptersQueryPrefix(bookUrl), chaptersData);
  }, [chaptersData, bookUrl, queryClient]);

  // origin 迟到/换源自愈: 起跑时如果还没拿到选源提示 (书架接口慢 / 冷启动深链 / 断网),
  // 那次目录可能是服务端默认源的; origin 到了就失效重取, 避免「错源目录 + staleTime
  // 内不再回源」的僵死状态 (配合上面"origin 未知不落盘")。
  // origin 从 A 直接换成 B 也要重取: 换了书源后目录/详情都是另一个源的, 旧缓存不能留着。
  const originStateRef = useRef<{ bookUrl: string; origin: string | undefined }>({
    bookUrl,
    origin: bookSourceUrl,
  });
  useEffect(() => {
    const prev = originStateRef.current;
    originStateRef.current = { bookUrl, origin: bookSourceUrl };
    const sameBook = prev.bookUrl === bookUrl;
    const originChanged = prev.origin !== bookSourceUrl;
    if (!sameBook || bookSourceUrl === undefined || !originChanged) {
      return;
    }
    void queryClient.invalidateQueries({ queryKey: chaptersQueryPrefix(bookUrl) });
    void queryClient.invalidateQueries({ queryKey: bookInfoQueryPrefix(bookUrl) });
  }, [bookSourceUrl, bookUrl, queryClient]);
  const refreshChapters = useCallback(async (): Promise<BookChapter[]> => {
    const list = await getChapterList(bookUrl, true, { bookSourceUrl });
    queryClient.setQueryData<BookChapter[]>(chaptersQueryKey(bookUrl, bookSourceUrl), list);
    void putChapters(bookUrl, list);
    return list;
  }, [bookUrl, bookSourceUrl, queryClient]);

  const book = bookQuery.data;
  // 详情持久化: 书架记录 (getBookshelf) 与接口返回的详情都写进 24h 客户端缓存。
  // 只在 bookQuery.queryFn 里写不够 —— 书架书走 initialData 就不会发 getBookInfo,
  // 缓存永远是空的, 于是每次进书都得等 getBookshelf 才有书名/origin。写一次之后
  // 二次进书 (含断网) 首帧就有详情可用。
  useEffect(() => {
    rememberBookInfo(book);
  }, [book]);
  const allBookmarks = bookmarksQuery.data;
  const bookmarks = useMemo(
    () => (book === undefined ? [] : bookmarksOfBook(allBookmarks ?? [], book)),
    [allBookmarks, book],
  );

  return {
    book,
    shelfBook,
    shelfQuery,
    // 目录数据源: 网络(或内存查询缓存)优先, 其次本地持久缓存 —— 有缓存就不等接口
    chapters: chaptersQuery.data ?? cachedChaptersOfBook ?? EMPTY_CHAPTERS,
    bookmarks,
    bookSourceUrl,
    inShelf: shelfBook !== undefined,
    bookQuery,
    chaptersQuery,
    bookmarksQuery,
    refreshChapters,
  };
}
