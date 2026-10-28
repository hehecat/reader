import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { toast } from "@/components/ui";
import { ApiError } from "@/lib/api-client";
import { effectiveTotalChapterNum } from "@/lib/chapterCount";
import { getCoverUrl, saveBookProgress } from "@/services/book";
import {
  deleteBook,
  deleteBooks,
  getBookGroups,
  getBookshelf,
  saveBookGroupId,
} from "@/services/bookshelf";
import type { Book, BookGroup } from "@/types/api";

/** 服务端状态 query key(PRD 6.2 约定) */
export const BOOKS_QUERY_KEY = ["books"] as const;
export const BOOK_GROUPS_QUERY_KEY = ["bookGroups"] as const;

/** 后端 BookType.local: 本地书的 origin 值 */
export const LOCAL_ORIGIN = "loc_book";
/** Book.type: 0=文本 1=音频 */
export const AUDIO_BOOK_TYPE = 1;

/** 内置分组 id(与旧 Vue 前端一致, 负数为虚拟分组, 不参与位运算) */
export const GROUP_ALL = -1;
export const GROUP_LOCAL = -2;
export const GROUP_AUDIO = -3;
export const GROUP_UNGROUPED = -4;

export interface ShelfGroup extends BookGroup {
  /** 内置分组不可删除/重命名 */
  builtIn: boolean;
}

export interface ShelfGroupItem {
  group: ShelfGroup;
  /** 该分组下的书籍数, 用于隐藏空分组标签 */
  count: number;
}

const BUILT_IN_GROUPS: ShelfGroup[] = [
  { groupId: GROUP_ALL, groupName: "全部", order: -10, show: true, builtIn: true },
  { groupId: GROUP_LOCAL, groupName: "本地", order: -9, show: true, builtIn: true },
  { groupId: GROUP_AUDIO, groupName: "音频", order: -8, show: true, builtIn: true },
  { groupId: GROUP_UNGROUPED, groupName: "未分组", order: -7, show: true, builtIn: true },
];

const EMPTY_BOOKS: Book[] = [];
const EMPTY_GROUPS: BookGroup[] = [];

/** 归一化后端/网络异常里的可展示文案 */
export function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.length > 0) return error.message;
  return fallback;
}

/** 阅读器路由(全局路由契约): /reader?url={encodeURIComponent(bookUrl)}&index={chapterIndex} */
export function readerPath(bookUrl: string, index?: number, source?: string): string {
  const params = new URLSearchParams({ url: bookUrl });
  if (typeof index === "number" && index >= 0) params.set("index", String(index));
  // 搜索预览直达阅读器时携带书源: 未入架书后端需要显式 bookSource 才能取目录/正文
  if (source !== undefined && source !== "") params.set("bookSource", source);
  return `/reader?${params.toString()}`;
}

/** 封面地址: customCoverUrl 优先, 空值返回 ""(CoverImage 据此渲染占位) */
export function bookCover(book: Book): string {
  return getCoverUrl(book.customCoverUrl ?? book.coverUrl);
}

/** 最近阅读优先: durChapterTime 降序(复制后排序, 不改动缓存里的原数组) */
export function sortBooksByRecent(books: Book[]): Book[] {
  return [...books].sort((a, b) => b.durChapterTime - a.durChapterTime);
}

/** 书籍是否属于某分组: 自定义分组按 group 位运算, 内置分组按语义字段 */
export function matchesGroup(book: Book, groupId: number): boolean {
  switch (groupId) {
    case GROUP_ALL:
      return true;
    case GROUP_LOCAL:
      return book.origin === LOCAL_ORIGIN;
    case GROUP_AUDIO:
      return book.type === AUDIO_BOOK_TYPE;
    case GROUP_UNGROUPED:
      return book.group === 0;
    default:
      // groupId === 0 视为不过滤(旧前端语义)
      return groupId === 0 ? true : (book.group & groupId) !== 0;
  }
}

export function filterBooksByGroup(books: Book[], groupId: number): Book[] {
  if (groupId === GROUP_ALL) return books;
  return books.filter((book) => matchesGroup(book, groupId));
}

/** 分组标签数据源: 内置分组 + 自定义分组(getBookGroups), 按 order 升序; 空分组(除「全部」)不展示 */
export function buildShelfGroups(books: Book[], groups: BookGroup[]): ShelfGroupItem[] {
  const custom: ShelfGroup[] = groups
    .filter((group) => group.groupId > 0)
    .map((group) => ({ ...group, builtIn: false }));

  return [...BUILT_IN_GROUPS, ...custom]
    .map((group) => ({ group, count: filterBooksByGroup(books, group.groupId).length }))
    .filter((item) => item.group.groupId === GROUP_ALL || item.count > 0)
    .sort((a, b) => a.group.order - b.group.order);
}

/** 书籍所属的自定义分组名(简介弹窗展示用) */
export function bookGroupNames(book: Book, groups: BookGroup[]): string[] {
  if (book.group === 0) return [];
  return groups
    .filter((group) => group.groupId > 0 && (book.group & group.groupId) !== 0)
    .map((group) => group.groupName);
}

export interface UseBookshelfResult {
  /** 已按最近阅读排序的书架 */
  books: Book[];
  isLoading: boolean;
  isFetching: boolean;
  /** refresh=1 的强制刷新进行中 */
  isRefreshing: boolean;
  error: Error | null;
  /** 普通刷新(走缓存策略) */
  refetch: () => void;
  /** 强制后端更新每本书的最新章节(较慢), 完成后写回 ["books"] 缓存 */
  refreshShelf: () => Promise<void>;
}

/** 书架列表: queryKey ["books"], 从阅读器返回时重新拉取以同步进度 */
export function useBookshelf(): UseBookshelfResult {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: BOOKS_QUERY_KEY,
    queryFn: () => getBookshelf(),
    select: sortBooksByRecent,
    refetchOnMount: "always",
  });

  const [isRefreshing, setIsRefreshing] = React.useState(false);

  const refreshShelf = React.useCallback(async () => {
    setIsRefreshing(true);
    try {
      const books = await getBookshelf(true);
      queryClient.setQueryData(BOOKS_QUERY_KEY, books);
      toast.success("书架已更新");
    } catch (error) {
      toast.error(errorMessage(error, "刷新书架失败"));
    } finally {
      setIsRefreshing(false);
    }
  }, [queryClient]);

  return {
    books: query.data ?? EMPTY_BOOKS,
    isLoading: query.isLoading,
    isFetching: query.isFetching,
    isRefreshing,
    error: query.error,
    refetch: query.refetch,
    refreshShelf,
  };
}

export interface UseBookGroupsResult {
  /** 后端保存的分组(含内置分组的持久化副本), 按 order 升序 */
  groups: BookGroup[];
  isLoading: boolean;
  refetch: () => void;
}

/** 分组按 order 升序(模块级函数: select 需要稳定标识) */
export function sortGroupsByOrder(groups: BookGroup[]): BookGroup[] {
  return [...groups].sort((a, b) => a.order - b.order);
}

/** 自定义分组: queryKey ["bookGroups"] */
export function useBookGroups(): UseBookGroupsResult {
  const query = useQuery({
    queryKey: BOOK_GROUPS_QUERY_KEY,
    queryFn: () => getBookGroups(),
    select: sortGroupsByOrder,
  });

  return {
    groups: query.data ?? EMPTY_GROUPS,
    isLoading: query.isLoading,
    refetch: query.refetch,
  };
}

/** 删除书籍: 单本走 deleteBook, 多本走 deleteBooks; 成功后失效 ["books"] 并提示 */
export function useDeleteBook() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (urls: string[]): Promise<number> => {
      if (urls.length === 0) return 0;
      const only = urls[0];
      if (urls.length === 1 && only !== undefined) {
        await deleteBook(only);
        return 1;
      }
      await deleteBooks(urls);
      return urls.length;
    },
    onSuccess: (count) => {
      if (count === 0) return;
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      toast.success(count > 1 ? `已删除 ${count} 本书` : "已从书架删除");
    },
    onError: (error) => {
      toast.error(errorMessage(error, "删除书籍失败"));
    },
  });
}

/** 移动书籍到分组: 后端 saveBookGroupId 会整体覆盖 book.group, 且只接受 groupId > 0 */
export function useSaveBookGroupId() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ url, groupId }: { url: string; groupId: number }) =>
      saveBookGroupId(url, groupId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
    },
    onError: (error) => {
      toast.error(errorMessage(error, "移动分组失败"));
    },
  });
}

/**
 * 设为读完: 把进度推进到最后一章.
 * 不能用 saveBook —— 后端在书籍已存在时会用书架里的旧值覆盖 durChapterIndex/Title/Time.
 */
export function useMarkBookRead() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (book: Book) => {
      // 有效总章数而非书架记录的裸值: 总数被残缺目录回写成小值时(记录 60 章 / 实际 400 章),
      // 直接写 total-1 会让读者进度倒退
      const lastIndex = effectiveTotalChapterNum(book) - 1;
      if (lastIndex < 0) {
        return Promise.reject(new ApiError("这本书还没有章节信息"));
      }
      return saveBookProgress(book.bookUrl, lastIndex);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      toast.success("已标记为读完");
    },
    onError: (error) => {
      toast.error(errorMessage(error, "标记为读完失败"));
    },
  });
}
