/**
 * 精确对齐 Reader3 Rust 后端 HTTP API 契约的领域模型定义
 */

export interface ApiEnvelope<T = unknown> {
  isSuccess: boolean;
  errorMsg: string;
  data: T;
}

/* ---------- Auth & System ---------- */

export interface FormatUser {
  username: string;
  lastLoginAt: number;
  accessToken: string;
  enableWebdav: boolean;
  enableLocalStore: boolean;
  enableBookSource: boolean;
  enableRssSource: boolean;
  bookSourceLimit: number;
  bookLimit: number;
  isAdmin: boolean;
  createdAt: number;
}

export interface UserAdminItem {
  username: string;
  bookSourceCount: number;
  bookCount: number;
  enableWebdav: boolean;
  enableLocalStore: boolean;
  enableBookSource: boolean;
  enableRssSource: boolean;
  bookSourceLimit: number;
  bookLimit: number;
  isAdmin: boolean;
  disabled: boolean;
  lastLoginAt: number;
  createdAt: number;
}

export interface SystemFontItem {
  name: string;
  size: number;
}

export interface UserInfoPayload {
  userInfo: FormatUser | null;
  secure: boolean;
  secureKey: boolean;
  fonts?: SystemFontItem[];
}

export interface SystemInfo {
  version: string;
  timestamp: number;
  secure: boolean;
  mode: "single" | "multi";
  isAdmin: boolean;
  inviteRequired: boolean;
  userLimit: number;
  uptimeSeconds: number;
}

/* ---------- Books, Chapters & Groups ---------- */

export interface Book {
  bookUrl: string;
  tocUrl: string;
  origin: string;
  originName: string;
  name: string;
  author: string;
  kind?: string | null;
  customTag?: string | null;
  coverUrl?: string | null;
  customCoverUrl?: string | null;
  intro?: string | null;
  customIntro?: string | null;
  charset?: string | null;
  type: number;
  group: number;
  groupIds?: number[];
  latestChapterTitle?: string | null;
  latestChapterTime: number;
  lastCheckTime: number;
  lastCheckCount: number;
  totalChapterNum: number;
  durChapterTitle?: string | null;
  durChapterIndex: number;
  durChapterPos: number;
  durChapterTime: number;
  wordCount?: string | null;
  canUpdate: boolean;
  order: number;
  originOrder: number;
  useReplaceRule: boolean;
  variable?: string | null;
  readConfig?: unknown;
  isInShelf: boolean;
  lastCheckError?: string | null;
  displayCover?: string | null;
  displayIntro?: string | null;
  localEpub?: boolean;
  splitLongChapter?: boolean;
  language?: string | null;
  publisher?: string | null;
  publishedAt?: string | null;
  createdAt: number;
}

export interface RelatedBook {
  name: string;
  author: string;
  bookUrl: string;
  coverUrl?: string | null;
}

export interface BookInfo {
  name: string;
  author: string;
  kind?: string | null;
  intro?: string | null;
  coverUrl?: string | null;
  tocUrl?: string | null;
  updateTime?: string | null;
  wordCount?: string | null;
  latestChapterTitle?: string | null;
  bookUrl: string;
  origin: string;
  originName: string;
  language?: string | null;
  publisher?: string | null;
  publishedAt?: string | null;
  relatedBooks?: RelatedBook[];
  type: number;
}

export type SearchBook = BookInfo & {
  originCount?: number;
  origins?: string[];
  bookSourceUrl?: string;
};

export interface BookChapter {
  title: string;
  url: string;
  tag?: string | null;
  isVolume: boolean;
  index: number;
  chapterWordCount?: number;
}

export type BookContentPayload =
  | { content: string }
  | { audioUrl: string; contentType?: string }
  | { images: string[] }
  | { downloadUrl: string }
  | { videoUrl: string };

export interface BookGroup {
  id: number;
  name: string;
  cover?: string | null;
  show: boolean;
  order: number;
}

export interface BookGroupWithCount extends BookGroup {
  orderNum: number;
  bookCount: number;
  groupId: number;
  groupName: string;
}

export interface Bookmark {
  bookUrl: string;
  title: string;
  bookName: string;
  bookAuthor: string;
  paragraphIndex: number;
  chapterIndex: number;
  chapterName: string;
  bookText: string;
  content: string;
  createdAt: number;
}

export interface ImportBookPreview {
  name: string;
  author: string;
  format: string;
  chapterCount: number;
  preview: string[];
  book: Partial<BookInfo>;
  chapters: BookChapter[];
}

/* ---------- Book Sources & Stats ---------- */

export interface BookSourceLite {
  bookSourceGroup?: string | null;
  bookSourceName: string;
  bookSourceUrl: string;
  enabled: boolean;
  bookSourceType: number;
  published?: boolean;
}

export interface BookSource extends BookSourceLite {
  bookUrlPattern?: string | null;
  customOrder: number;
  enabledExplore: boolean;
  enabledCookieJar?: boolean | null;
  concurrentRate?: string | null;
  jsLib?: string | null;
  header?: string | null;
  proxyUrl?: string | null;
  loginUrl?: string | null;
  loginUi?: string | null;
  loginCheckJs?: string | null;
  loginJs?: string | null;
  bookSourceComment?: string | null;
  variableComment?: string | null;
  lastUpdateTime: number;
  respondTime: number;
  weight: number;
  exploreUrl?: string | null;
  searchUrl?: string | null;
  ruleExplore?: Record<string, unknown> | null;
  ruleSearch?: Record<string, unknown> | null;
  ruleBookInfo?: Record<string, unknown> | null;
  ruleToc?: Record<string, unknown> | null;
  ruleContent?: Record<string, unknown> | null;
  ruleRelated?: Record<string, unknown> | null;
  [key: string]: unknown;
}

export interface SourceStat {
  sourceUrl: string;
  confidence: number | null;
  confidenceLo: number | null;
  attempts: number;
  effectiveSamples: number;
  successRate: number;
  avgLatencyMs: number;
  consecFailures: number;
  lastFailureAt: number;
}

export interface InvalidBookSource {
  sourceUrl: string;
  time: number;
  error: string;
  errorMsg?: string;
  bookSourceUrl?: string;
}

export interface SourceCookie {
  sourceUrl: string;
  cookie: string;
  userAgent?: string;
  loginHeader?: string;
  updatedAt: number;
}

export type SourceLoginResult =
  | { success: true; needCaptcha: false; cookie: string }
  | { success: false; needCaptcha: true; captchaUrl: string; captchaId: string; message: string }
  | { success: false; needManualCaptcha: true; message: string }
  | { success: false; needCaptcha?: false; needManualCaptcha?: false; message: string };

export interface DebugStep {
  ruleName?: string;
  rule_name?: string;
  url?: string;
  elapsedMs?: number;
  elapsed_ms?: number;
  resultLen?: number;
  result_len?: number;
  error?: string | null;
  detail?: unknown;
}

/* ---------- Replace Rules, Cache, Dict, Fonts, TTS, Files ---------- */

export interface ReplaceRule {
  id: string;
  name: string;
  group?: string | null;
  find: string;
  replace: string;
  scope?: string | null;
  scopeTitle: boolean;
  scopeContent: boolean;
  isRegex: boolean;
  timeoutMillisecond: number;
  enabled: boolean;
  order: number;
}

export interface CacheInfo {
  tocCacheCount: number;
  tocCacheSize: number;
  chapterCount: number;
  chapterSize: number;
  totalSize: number;
}

export interface ShelfBookCacheInfo extends Partial<Book> {
  bookUrl: string;
  cacheChapterCount: number;
  cacheSize: number;
  onShelf?: boolean;
}

export interface CacheProgress {
  cached: number;
  total: number;
  title: string;
  finished: boolean;
  cancelled: boolean;
  error: string | null;
  cachedCount?: number;
  successCount?: number;
  failedCount?: number;
}

export interface BookContentHit {
  chapterIndex: number;
  title: string;
  snippet: string;
}

export interface DictEntry {
  kind: string;
  word: string;
  phonetic?: string;
  pinyin?: string;
  body?: string;
  source: string;
}

export interface DictLookupResult {
  status: "ok" | "building" | "unavailable" | string;
  entries: DictEntry[];
}

export interface CustomFont {
  id: string;
  name: string;
  family: string;
  size: number;
  updatedAt?: number;
}

export interface HttpTts {
  id: string;
  url: string;
  name: string;
  type: number;
  contentType?: string | null;
  concurrentRate?: string | null;
  loginUrl?: string | null;
  loginUi?: string | null;
  header?: string | null;
  jsLib?: string | null;
  enabledCookieJar?: boolean | null;
  loginCheckJs?: string | null;
  lastUpdateTime: number;
}

export interface FileEntry {
  name: string;
  size: number;
  path: string;
  lastModified: number;
  isDirectory: boolean;
}

export interface RestoreCounts {
  books: number;
  sources: number;
  bookmarks: number;
  groups: number;
  rules: number;
  httpTts?: number;
  txtTocRules?: number;
}

export interface RestoreReport {
  restored: RestoreCounts;
  skipped: RestoreCounts;
}
