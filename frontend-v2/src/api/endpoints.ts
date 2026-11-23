import { apiGet, apiPost, openSseStream, type SseCallbacks } from "./client";
import type {
  Book,
  BookChapter,
  BookContentHit,
  BookContentPayload,
  BookGroup,
  BookGroupWithCount,
  BookInfo,
  Bookmark,
  BookSource,
  BookSourceLite,
  CacheInfo,
  CacheProgress,
  CustomFont,
  DebugStep,
  DictLookupResult,
  FileEntry,
  FormatUser,
  HttpTts,
  ImportBookPreview,
  InvalidBookSource,
  ReplaceRule,
  RestoreReport,
  SearchBook,
  ShelfBookCacheInfo,
  SourceCookie,
  SourceLoginResult,
  SourceStat,
  SystemInfo,
  UserAdminItem,
  UserInfoPayload,
} from "./types";

/* ---------- Auth & System ---------- */

export const authApi = {
  getSystemInfo: () => apiGet<SystemInfo>("/reader3/getSystemInfo", undefined, { silentAuth: true }),
  getUserInfo: () => apiGet<UserInfoPayload>("/reader3/getUserInfo", undefined, { silentAuth: true }),
  login: (body: { username: string; password: string; isLogin: boolean; code?: string }) =>
    apiPost<FormatUser>("/reader3/login", body, { silentAuth: true }),
  logout: () => apiPost<null>("/reader3/logout"),
};

export const usersApi = {
  list: () => apiGet<UserAdminItem[]>("/reader3/getUsers"),
  add: (body: {
    username: string;
    password: string;
    isAdmin?: boolean;
    enableWebdav?: boolean;
    enableLocalStore?: boolean;
    enableBookSource?: boolean;
    bookSourceLimit?: number;
    bookLimit?: number;
  }) => apiPost<FormatUser>("/reader3/addUser", body),
  update: (body: {
    username: string;
    isAdmin?: boolean;
    disabled?: boolean;
    enableWebdav?: boolean;
    enableLocalStore?: boolean;
    enableBookSource?: boolean;
    bookSourceLimit?: number;
    bookLimit?: number;
  }) => apiPost<null>("/reader3/updateUser", body),
  remove: (username: string) => apiPost<null>("/reader3/deleteUser", { username }),
  resetPassword: (username: string, password: string) =>
    apiPost<null>("/reader3/resetUserPassword", { username, password }),
  clearInactive: (inactiveDay: number) =>
    apiPost<{ deleted: string[]; count: number }>("/reader3/clearInactiveUsers", { inactiveDay }),
};

/* ---------- Bookshelf, Reading, Groups, Bookmarks & Local Import ---------- */

export const booksApi = {
  getShelf: (refresh = false) =>
    apiGet<Book[]>("/reader3/getBookshelf", refresh ? { refresh: 1 } : undefined),
  saveBook: (book: Partial<Book> & { bookUrl: string }) =>
    apiPost<null>("/reader3/saveBook", book),
  deleteBook: (bookUrl: string) => apiPost<string>("/reader3/deleteBook", { bookUrl }),
  deleteBooks: (bookUrls: string[]) =>
    apiPost<{ count: number }>("/reader3/deleteBooks", { bookUrls }),
  saveProgress: (body: {
    bookUrl: string;
    durChapterIndex: number;
    durChapterPos?: number;
    durChapterTime?: number;
    durChapterTitle?: string;
  }) =>
    apiPost<null>("/reader3/saveBookProgress", {
      ...body,
      durChapterPos: body.durChapterPos ?? 0,
      durChapterTime: body.durChapterTime ?? Date.now(),
    }),
  getBookInfo: (url: string, bookSource?: string, signal?: AbortSignal) =>
    apiPost<BookInfo>("/reader3/getBookInfo", { url, bookSource }, { signal }),
  getChapters: (
    params: { url: string; tocUrl?: string; bookSource?: string; refresh?: number },
    signal?: AbortSignal,
  ) => apiPost<BookChapter[]>("/reader3/getChapterList", params, { signal }),
  getContent: (
    params: {
      chapterUrl: string;
      bookUrl?: string;
      bookSource?: string;
      index?: number;
      title?: string;
      cache?: number;
      refresh?: number;
      type?: number;
    },
    signal?: AbortSignal,
  ) => apiPost<BookContentPayload>("/reader3/getBookContent", params, { signal }),
  switchSource: (body: {
    bookUrl: string;
    newUrl: string;
    bookSourceUrl: string;
    name?: string;
    author?: string;
  }) => apiPost<Book>("/reader3/setBookSource", body),
  getChaptersByRule: (body: Record<string, unknown>, signal?: AbortSignal) =>
    apiPost<BookChapter[] | { book: Book; chapters: BookChapter[] }>(
      "/reader3/getChapterListByRule",
      body,
      { signal },
    ),

  // Groups
  getGroups: () => apiGet<BookGroupWithCount[]>("/reader3/getBookGroups"),
  saveGroup: (group: Partial<BookGroup> & { name: string }) =>
    apiPost<BookGroup>("/reader3/saveBookGroup", group),
  deleteGroup: (id: number) => apiPost<string>("/reader3/deleteBookGroup", { id }),
  setBookGroup: (bookUrl: string, groupId: number) =>
    apiPost<null>("/reader3/saveBookGroupId", { bookUrl, groupId }),

  // Bookmarks
  getBookmarks: (bookUrl: string) =>
    apiPost<Bookmark[]>("/reader3/getBookmarks", { bookUrl }),
  saveBookmark: (bm: Bookmark) => apiPost<null>("/reader3/saveBookmark", bm),
  deleteBookmark: (bookUrl: string, title: string) =>
    apiPost<null>("/reader3/deleteBookmark", { bookUrl, title }),
  deleteBookmarks: (bookUrl: string, ids: string[]) =>
    apiPost<{ count: number }>("/reader3/deleteBookmarks", { bookUrl, ids }),

  // Local TXT / EPUB Import
  previewLocalBook: (file: File) => {
    const form = new FormData();
    form.append("file", file);
    return apiPost<ImportBookPreview>("/reader3/importBookPreview", form, { timeoutMs: 120_000 });
  },
  uploadLocalBook: (file: File) => {
    const form = new FormData();
    form.append("file", file);
    return apiPost<BookInfo>("/reader3/uploadLocalBook", form, { timeoutMs: 180_000 });
  },
};

/* ---------- Sources & Debug ---------- */

export const sourcesApi = {
  list: () => apiGet<BookSource[]>("/reader3/getBookSources"),
  listLite: () => apiGet<BookSourceLite[]>("/reader3/getBookSources", { simple: 1 }),
  get: (bookSourceUrl: string) =>
    apiPost<BookSource>("/reader3/getBookSource", { bookSourceUrl }),
  save: (source: Partial<BookSource> & { bookSourceUrl: string }) =>
    apiPost<null>("/reader3/saveBookSource", source),
  saveMany: (sources: BookSource[]) =>
    apiPost<{ count: number }>("/reader3/saveBookSources", sources),
  remove: (bookSourceUrl: string) =>
    apiPost<null>("/reader3/deleteBookSource", { bookSourceUrl }),
  removeMany: (urls: string[]) =>
    apiPost<{ deleted: number }>("/reader3/deleteBookSources", urls),
  publishDefault: (bookSources: string[], mode?: "publish" | "remove") =>
    apiPost<{ published?: number; missing?: string[]; removed?: number }>(
      "/reader3/setAsDefaultBookSources",
      { bookSources, mode: mode === "remove" ? "remove" : undefined },
    ),
  stats: () => apiGet<SourceStat[]>("/reader3/getSourceStats"),
  invalids: () => apiGet<InvalidBookSource[]>("/reader3/getInvalidBookSources"),
  availableForBook: (url: string, refresh = 0) =>
    apiPost<SearchBook[]>("/reader3/getAvailableBookSource", { url, refresh }),
  previewRemote: (url: string) =>
    apiPost<{ sources: BookSource[]; existing: string[] }>(
      "/reader3/saveFromRemoteSource",
      { url },
      { params: { preview: 1 }, timeoutMs: 90_000 },
    ),
  importRemote: (url: string) =>
    apiPost<{ count: number }>("/reader3/saveFromRemoteSource", { url }, { timeoutMs: 90_000 }),
  login: (body: {
    bookSource: string;
    username?: string;
    password?: string;
    captcha?: string;
    mode?: "http" | "browser";
  }) => apiPost<SourceLoginResult>("/reader3/loginBookSource", body, { timeoutMs: 90_000 }),
  getCookies: () => apiGet<SourceCookie[]>("/reader3/getBookSourceCookie"),
  setCookie: (bookSource: string, cookie: string) =>
    apiPost<{ success: boolean; cleared?: boolean }>("/reader3/setBookSourceCookie", {
      bookSource,
      cookie,
    }),
  debugStream: (
    body: {
      action: "search" | "explore" | "toc" | "content";
      bookSource: string;
      key?: string;
      chapterUrl?: string;
    },
    callbacks: SseCallbacks<
      | { type: "start"; message: { action: string; bookSource: string } }
      | { type: "step"; message: DebugStep }
      | { type: "result"; data: unknown }
      | { type: "error"; message: string }
    >,
  ) => openSseStream("/reader3/bookSourceDebugSSE", body, callbacks),
};

/* ---------- Search & Explore ---------- */

export const discoverApi = {
  searchSingle: (
    body: { key: string; bookSource: string; page?: number; timeout?: number },
    signal?: AbortSignal,
  ) => apiPost<SearchBook[]>("/reader3/searchBook", body, { signal }),
  explore: (
    body: { url: string; bookSource: string; page?: number; refresh?: number; timeout?: number },
    signal?: AbortSignal,
  ) =>
    apiPost<SearchBook[]>(
      "/reader3/exploreBook",
      { ...body, page: body.page ?? 1 },
      { signal },
    ),
  searchMultiStream: (
    body: {
      key: string;
      bookSourceGroup?: string;
      bookSourceUrl?: string;
      exact?: boolean;
      lastIndex?: number;
      searchSize?: number;
      concurrentCount?: number;
      timeout?: number;
      all?: boolean;
    },
    callbacks: SseCallbacks<
      { lastIndex: number; data: SearchBook[] },
      { lastIndex: number; isEnd: boolean }
    >,
  ) =>
    openSseStream(
      "/reader3/searchBookMultiSSE",
      {
        key: body.key,
        bookSourceGroup: body.bookSourceGroup || undefined,
        bookSourceUrl: body.bookSourceUrl || undefined,
        exact: body.exact ? 1 : undefined,
        lastIndex: body.lastIndex ?? -1,
        searchSize: body.searchSize ?? 50,
        concurrentCount: body.concurrentCount ?? 24,
        timeout: body.timeout ?? 15,
      },
      callbacks,
      body.all ? { all: 1 } : undefined,
    ),
  searchAlternativesStream: (
    body: { url: string; bookSource: string },
    callbacks: SseCallbacks<
      { lastIndex: number; data: SearchBook[] },
      { lastIndex: number; isEnd: boolean }
    >,
  ) => openSseStream("/reader3/searchBookSourceSSE", body, callbacks),
};

/* ---------- Purify, Cache, Dict, Fonts, TTS & WebDAV ---------- */

export const featuresApi = {
  // Replace rules
  getReplaceRules: () => apiGet<ReplaceRule[]>("/reader3/getReplaceRules"),
  saveReplaceRule: (rule: Partial<ReplaceRule> & { name: string; find: string }) =>
    apiPost<{ id: string }>("/reader3/saveReplaceRule", rule),
  saveReplaceRules: (rules: ReplaceRule[]) =>
    apiPost<{ count: number }>("/reader3/saveReplaceRules", rules),
  deleteReplaceRule: (id: string) => apiPost<null>("/reader3/deleteReplaceRule", { id }),
  deleteReplaceRules: (payload: { ids: string[] } | { all: true }) =>
    apiPost<{ count: number }>("/reader3/deleteReplaceRules", payload),

  // Cache & In-book search
  getCacheInfo: () => apiGet<CacheInfo>("/reader3/getCacheInfo"),
  clearCache: (type: "toc" | "chapters" | "all") =>
    apiPost<{ deletedToc: number; deletedChapters: number }>("/reader3/clearCache", { type }),
  deleteBookCache: (url: string) => apiPost<string>("/reader3/deleteBookCache", { url }),
  getBookCacheInfo: (url: string) =>
    apiPost<ShelfBookCacheInfo>("/reader3/getShelfBookWithCacheInfo", { url }),
  cacheBookStream: (
    params: { url?: string; taskId?: string },
    callbacks: SseCallbacks<CacheProgress>,
  ) => openSseStream("/reader3/cacheBookSSE", params, callbacks),
  cancelCacheBook: (params: { url?: string; taskId?: string }) =>
    apiPost<{ cancelled: boolean }>("/reader3/cancelCacheBook", params),
  searchBookContent: (bookUrl: string, key: string, signal?: AbortSignal) =>
    apiPost<BookContentHit[]>("/reader3/searchBookContent", { bookUrl, key }, { signal }),

  // Offline / online dict
  dictLookup: (word: string, signal?: AbortSignal) =>
    apiPost<DictLookupResult>("/reader3/dictLookup", { word }, { signal }),

  // Custom fonts
  listFonts: () => apiGet<CustomFont[]>("/reader3/getFontList"),
  uploadFont: (file: File) => {
    const form = new FormData();
    form.append("file", file);
    return apiPost<CustomFont>("/reader3/uploadFont", form, { timeoutMs: 120_000 });
  },
  deleteFont: (id: string) => apiPost<{ deleted: boolean }>("/reader3/deleteFont", { id }),

  // HttpTTS
  listHttpTts: () => apiGet<HttpTts[]>("/reader3/getHttpTTSList"),
  saveHttpTts: (item: Partial<HttpTts> & { url: string; name: string }) =>
    apiPost<null>("/reader3/saveHttpTTS", item),
  deleteHttpTts: (url: string) => apiPost<null>("/reader3/deleteHttpTTS", { url }),

  // WebDAV backup & files
  backupToWebdav: () => apiPost<{ path: string }>("/reader3/backupToWebdav", undefined, { timeoutMs: 120_000 }),
  restoreFromWebdav: (path: string, overwrite = false) =>
    apiPost<RestoreReport>("/reader3/restoreFromWebdav", { path, overwrite }, { timeoutMs: 120_000 }),
  listFiles: (home = "__WEBDAV__", path = "/") =>
    apiGet<FileEntry[]>("/reader3/file/list", { home, path }),
  uploadFiles: (files: File[], home = "__WEBDAV__", path = "/") => {
    const form = new FormData();
    form.append("home", home);
    form.append("path", path);
    for (const f of files) form.append("file", f);
    return apiPost<FileEntry[]>("/reader3/file/upload", form, { timeoutMs: 180_000 });
  },
  deleteFile: (path: string, home = "__WEBDAV__") =>
    apiPost<string>("/reader3/file/delete", { home, path }),
  deleteFiles: (paths: string[], home = "__WEBDAV__") =>
    apiPost<{ deleted: number; failed: number }>("/reader3/file/deleteMulti", { home, paths }),
};
