import { useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";

import { getBookInfoCache, rememberBookInfo } from "@/lib/bookInfoCache";
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
  /** 章节列表; 加载中为空数组 */
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
  chaptersQuery: UseQueryResult<BookChapter[], Error>;
  bookmarksQuery: UseQueryResult<Bookmark[], Error>;
  /** 手动刷新目录: bypass warp 目录缓存 (refresh=1) 并写回查询缓存 */
  refreshChapters: () => Promise<BookChapter[]>;
}

/**
 * 阅读器基础数据: 书籍信息 + 章节列表 + 书签.
 * bookUrl 为空时查询都不发起 (页面层负责呈现「缺少书籍链接」态).
 */
export function useBookData(bookUrl: string, sourceHint?: string): UseBookDataResult {
  const enabled = bookUrl !== "";

  // warp 的 getBookInfo/getChapterList/getBookContent 都靠 bookSource 参数选源,
  // 缺省时落到服务端默认源(可能是不可达的内网源); 书架书的 origin 是唯一可靠提示,
  // 故先等书架列表就绪再发起书籍查询 (本地书 origin=loc_book 不需要提示).
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
  const bookSourceUrl = useMemo(() => {
    const origin = shelfBook?.origin ?? (sourceHint || undefined);
    return origin === undefined || origin === LOCAL_ORIGIN ? undefined : origin;
  }, [shelfBook, sourceHint]);
  const ready = enabled && !shelfQuery.isPending;

  const bookQuery = useQuery({
    queryKey: ["bookInfo", bookUrl],
    queryFn: () => {
      const promise =
        bookSourceUrl === undefined ? getBookInfo(bookUrl) : getBookInfo({ url: bookUrl, bookSourceUrl });
      void promise.then(rememberBookInfo).catch(() => {});
      return promise;
    },
    enabled: ready,
    // 书架书: 书架记录即完整 Book; 其余书: 24h 客户端详情缓存. 两者都作初始数据秒渲染,
    // 随后必然向服务端取一次(含跨设备的最新阅读进度) —— 多端场景: 本端 5 分钟缓存内进书
    // 会拿到旧进度, 继续读会把新进度覆盖, 故这里不做 staleTime 复用。
    initialData: shelfBook ?? getBookInfoCache(bookUrl),
    staleTime: 0,
    refetchOnMount: "always",
  });

  const chaptersQuery = useQuery({
    queryKey: ["chapters", bookUrl],
    queryFn: () => getChapterList(bookUrl, undefined, { bookSourceUrl }),
    enabled: ready,
    staleTime: 5 * 60_000,
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
  const refreshChapters = useCallback(async (): Promise<BookChapter[]> => {
    const list = await getChapterList(bookUrl, true, { bookSourceUrl });
    queryClient.setQueryData<BookChapter[]>(["chapters", bookUrl], list);
    return list;
  }, [bookUrl, bookSourceUrl, queryClient]);

  const book = bookQuery.data;
  const allBookmarks = bookmarksQuery.data;
  const bookmarks = useMemo(
    () => (book === undefined ? [] : bookmarksOfBook(allBookmarks ?? [], book)),
    [allBookmarks, book],
  );

  return {
    book,
    shelfBook,
    shelfQuery,
    chapters: chaptersQuery.data ?? [],
    bookmarks,
    bookSourceUrl,
    inShelf: shelfBook !== undefined,
    bookQuery,
    chaptersQuery,
    bookmarksQuery,
    refreshChapters,
  };
}
