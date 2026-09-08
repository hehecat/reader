import { z } from "zod";

import { plainIntro } from "@/lib/text";

/**
 * API 数据模型 (对应 warp Rust 后端的实体, serde 序列化).
 * 「Gson 省略 null」的旧假设作废: warp 对缺失的 Option 字段显式输出 null,
 * 统一由 api-client 的 stripNullsDeep 在进 zod 前归一成 undefined,
 * 因此可选字段仍一律写 `.optional()` / `.default()`, 不写 `.nullable()`.
 */

/** 通用响应封装: { isSuccess, errorMsg, data } */
export const envelopeSchema = <T extends z.ZodTypeAny>(data: T) =>
  z.object({
    isSuccess: z.boolean(),
    errorMsg: z.string().default(""),
    data,
  });

export interface ApiEnvelope<T> {
  isSuccess: boolean;
  errorMsg: string;
  data: T;
}

/** 后端约定的特殊 data 值 */
export const NEED_LOGIN = "NEED_LOGIN";
export const NEED_SECURE_KEY = "NEED_SECURE_KEY";

/** Book.ReadConfig (io.legado.app.data.entities.Book.ReadConfig) */
export const readConfigSchema = z.object({
  reverseToc: z.boolean().default(false),
  pageAnim: z.number().default(-1),
  reSegment: z.boolean().default(false),
  imageStyle: z.string().optional(),
  useReplaceRule: z.boolean().default(false),
  /** 后端实体额外字段, 保留以免回写书籍时丢失 */
  delTag: z.number().optional(),
});

/**
 * Book (io.legado.app.data.entities.Book).
 * 注意: warp 的 /getBookInfo 返回部分 BookInfo (只有 name/author/kind/intro/
 * coverUrl/tocUrl/bookUrl/origin/originName/type 等), 不带进度与计数字段,
 * 因此这些字段一律 .default(), 书架/换源等返回完整 Book 的接口不受影响.
 */
export const bookSchema = z.object({
  bookUrl: z.string(),
  tocUrl: z.string(),
  origin: z.string(),
  originName: z.string(),
  name: z.string(),
  author: z.string(),
  kind: z.string().optional(),
  customTag: z.string().optional(),
  coverUrl: z.string().optional(),
  customCoverUrl: z.string().optional(),
  /** 源站简介常带原始 HTML: 进模型前清洗为纯文本 */
  intro: z.string().transform(plainIntro).optional(),
  customIntro: z.string().optional(),
  charset: z.string().optional(),
  /** 0=文本 1=音频 */
  type: z.number(),
  group: z.number().default(0),
  latestChapterTitle: z.string().optional(),
  latestChapterTime: z.number().default(0),
  lastCheckTime: z.number().default(0),
  lastCheckCount: z.number().default(0),
  totalChapterNum: z.number().default(0),
  durChapterTitle: z.string().optional(),
  durChapterIndex: z.number().default(0),
  durChapterPos: z.number().default(0),
  durChapterTime: z.number().default(0),
  wordCount: z.string().optional(),
  canUpdate: z.boolean().default(false),
  order: z.number().default(0),
  originOrder: z.number().default(0),
  useReplaceRule: z.boolean().default(false),
  variable: z.string().optional(),
  readConfig: readConfigSchema.optional(),
});

/** BookChapter (io.legado.app.data.entities.BookChapter); warp 目录只回 title/url/tag/isVolume/index 等, baseUrl/bookUrl 缺省 */
export const bookChapterSchema = z.object({
  url: z.string(),
  title: z.string(),
  isVolume: z.boolean(),
  baseUrl: z.string().default(""),
  bookUrl: z.string().default(""),
  index: z.number(),
  resourceUrl: z.string().optional(),
  tag: z.string().optional(),
  start: z.number().optional(),
  end: z.number().optional(),
  /** EPUB 章节 fragmentId, 后端实体额外字段 */
  startFragmentId: z.string().optional(),
  endFragmentId: z.string().optional(),
  variable: z.string().optional(),
});

/** SearchBook (io.legado.app.data.entities.SearchBook) */
export const searchBookSchema = z.object({
  bookUrl: z.string(),
  origin: z.string(),
  originName: z.string(),
  type: z.number(),
  name: z.string(),
  author: z.string(),
  kind: z.string().optional(),
  coverUrl: z.string().optional(),
  intro: z.string().transform(plainIntro).optional(),
  wordCount: z.string().optional(),
  latestChapterTitle: z.string().optional(),
  tocUrl: z.string(),
  time: z.number(),
  variable: z.string().optional(),
  originOrder: z.number(),
  /** 后端聚合的书源列表 (LinkedHashSet) */
  origins: z.array(z.string()).optional(),
});

/** BookGroup (io.legado.app.data.entities.BookGroup) */
export const bookGroupSchema = z.object({
  groupId: z.number(),
  groupName: z.string(),
  order: z.number(),
  show: z.boolean(),
});

/** 登录接口返回的用户数据 (BaseController.formatUser) */
export const loginResultSchema = z.object({
  username: z.string(),
  /** 格式 `username:token` */
  accessToken: z.string(),
  lastLoginAt: z.number(),
  enableWebdav: z.boolean(),
  enableLocalStore: z.boolean(),
  createdAt: z.number(),
});

/**
 * 书源规则对象 (SearchRule / ExploreRule / BookInfoRule / TocRule / ContentRule).
 * 后端所有规则字段均为 String?, 这里统一用宽松的 record 处理.
 */
export const bookSourceRuleSchema = z.record(z.string(), z.string().optional());

/** BookSource (io.legado.app.data.entities.BookSource) */
export const bookSourceSchema = z.object({
  bookSourceUrl: z.string(),
  bookSourceName: z.string(),
  bookSourceGroup: z.string().default(""),
  /** 0=文本 1=音频; 源仓库常以字符串下发数字字段 → coerce 容忍 */
  bookSourceType: z.coerce.number().default(0),
  bookSourceComment: z.string().default(""),
  enabled: z.boolean().default(true),
  enabledExplore: z.boolean().default(true),
  customOrder: z.coerce.number().default(0),
  lastUpdateTime: z.coerce.number().default(0),
  respondTime: z.coerce.number().default(180000),
  weight: z.coerce.number().default(0),
  header: z.string().optional(),
  loginUrl: z.string().optional(),
  /** legado 登录表单定义 JSON 串: [{"name","type","text":{"label","isPassword"}}] */
  loginUi: z.string().optional(),
  loginCheckJs: z.string().optional(),
  bookUrlPattern: z.string().optional(),
  concurrentRate: z.string().optional(),
  searchUrl: z.string().optional(),
  exploreUrl: z.string().optional(),
  ruleSearch: bookSourceRuleSchema.optional(),
  ruleExplore: bookSourceRuleSchema.optional(),
  ruleBookInfo: bookSourceRuleSchema.optional(),
  ruleToc: bookSourceRuleSchema.optional(),
  ruleContent: bookSourceRuleSchema.optional(),
});

export type ReadConfig = z.infer<typeof readConfigSchema>;
export type Book = z.infer<typeof bookSchema>;
export type BookChapter = z.infer<typeof bookChapterSchema>;
export type SearchBook = z.infer<typeof searchBookSchema>;
export type BookGroup = z.infer<typeof bookGroupSchema>;
export type LoginResult = z.infer<typeof loginResultSchema>;
export type BookSourceRule = z.infer<typeof bookSourceRuleSchema>;
export type BookSource = z.infer<typeof bookSourceSchema>;

export const bookListSchema = z.array(bookSchema);
export const bookChapterListSchema = z.array(bookChapterSchema);
export const searchBookListSchema = z.array(searchBookSchema);
export const bookGroupListSchema = z.array(bookGroupSchema);
export const bookSourceListSchema = z.array(bookSourceSchema);
