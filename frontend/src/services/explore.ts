import { z } from "zod";

import {
  get,
  parseWith,
  post,
  type ApiRequestConfig,
} from "@/lib/api-client";
import {
  bookSchema,
  searchBookListSchema,
  type Book,
  type BookSource,
  type SearchBook,
} from "@/types/api";

/**
 * 书海(发现)相关接口.
 *
 * 后端契约(BookController / YueduApi):
 * - `/exploreBook` GET|POST, 参数 ruleFindUrl + page + bookSourceUrl, 返回 SearchBook[]
 * - `/getAvailableBookSource` GET|POST, 参数 url + refresh, 返回该书可用的换源候选(SearchBook 结构)
 * - `/setBookSource` GET|POST, 参数 bookUrl + newUrl + bookSourceUrl, 返回换源后的 Book
 */

export { exploreMenuSchema, parseExploreMenus, type ExploreMenu } from "./exploreMenus";

import { parseExploreMenus } from "./exploreMenus";
/** 书源能否用于书海探索: 已启用 + 允许发现 + 至少有一个可解析的发现菜单 */
export function isExplorable(source: BookSource): boolean {
  return source.enabled && source.enabledExplore && parseExploreMenus(source).length > 0;
}

export interface ExploreBookQuery {
  /** 书源地址(BookSource.bookSourceUrl); 缺省时后端使用默认书源 */
  bookSourceUrl?: string;
  /** 发现菜单地址, 可含 `{{page}}` 占位 */
  ruleFindUrl: string;
  /** 页码, 从 1 开始 */
  page?: number;
}

/** 探索某个书源的发现页, 返回该页书籍(结构与搜索结果一致) */
export async function exploreBook(
  query: ExploreBookQuery,
  config?: ApiRequestConfig,
): Promise<SearchBook[]> {
  const data = await get<unknown>(
    "/exploreBook",
    {
      // 后端 explore 参数名为 bookSource (与单源搜索一致); 历史误发 bookSourceUrl 致书海全空
      bookSource: query.bookSourceUrl,
      ruleFindUrl: query.ruleFindUrl,
      page: query.page ?? 1,
    },
    config,
  );
  return parseWith(searchBookListSchema, data);
}

/**
 * 换源候选项.
 * 后端返回的是按「书名_作者」缓存下来的 SearchBook 列表(可能含 `loc_book` 本地书),
 * 字段比搜索结果少(tocUrl / time / originOrder 可能缺失), 因此本地定义宽松 schema 而不是复用 searchBookSchema.
 */
export const availableBookSourceSchema = z.object({
  bookUrl: z.string(),
  /** 候选书源地址, setBookSource 的 bookSourceUrl 用它 */
  origin: z.string(),
  originName: z.string(),
  name: z.string(),
  author: z.string().default(""),
  /** 0=文本 1=音频 */
  type: z.number().default(0),
  kind: z.string().optional(),
  coverUrl: z.string().optional(),
  intro: z.string().optional(),
  latestChapterTitle: z.string().optional(),
  wordCount: z.string().optional(),
});

export const availableBookSourceListSchema = z.array(availableBookSourceSchema);

export type AvailableBookSource = z.infer<typeof availableBookSourceSchema>;

/**
 * 某本书当前可用的书源列表(换源候选).
 * @param bookUrl 书架上这本书的 bookUrl(书不在书架且无缓存时后端返回「书籍信息错误」)
 * @param refresh true 时后端用全部书源重新搜索该书(慢), 缺省只读缓存
 */
export async function getAvailableBookSource(
  bookUrl: string,
  refresh = false,
  config?: ApiRequestConfig,
): Promise<AvailableBookSource[]> {
  const data = await post<unknown>(
    "/getAvailableBookSource",
    { url: bookUrl, refresh: refresh ? 1 : 0 },
    config,
  );
  return parseWith(availableBookSourceListSchema, data);
}

export interface SetBookSourceInput {
  /** 书架上现有书籍的 bookUrl(换源要求书已在书架) */
  bookUrl: string;
  /** 目标源里这本书的链接(候选项的 bookUrl) */
  newBookUrl: string;
  /** 目标书源地址(候选项的 origin) */
  bookSourceUrl: string;
}

/** 换源: 后端参数名是 `newUrl`, 返回换源后的书架书籍 */
export async function setBookSource(
  input: SetBookSourceInput,
  config?: ApiRequestConfig,
): Promise<Book> {
  const data = await post<unknown>(
    "/setBookSource",
    {
      bookUrl: input.bookUrl,
      newUrl: input.newBookUrl,
      bookSourceUrl: input.bookSourceUrl,
    },
    { timeout: 120_000, ...config },
  );
  return parseWith(bookSchema, data);
}

/**
 * Book → 换源候选项, 与后端 `Book.toSearchBook()` 同口径.
 * 加入书架时后端会把当前源写进该书的候选缓存, 但用户存储的读取有数秒缓存,
 * 列表不会立刻反映; 调用方据此在前端预填, 让换源面板即时可用.
 */
export function toAvailableBookSource(book: Book): AvailableBookSource {
  return {
    bookUrl: book.bookUrl,
    origin: book.origin,
    originName: book.originName,
    name: book.name,
    author: book.author,
    type: book.type,
    kind: book.kind,
    coverUrl: book.coverUrl,
    intro: book.intro,
    latestChapterTitle: book.latestChapterTitle,
  };
}
