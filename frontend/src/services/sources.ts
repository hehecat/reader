import { z } from "zod";
import {
  ApiError,
  get,
  parseWith,
  post,
  stripNullsDeep,
  type ApiParams,
  type ApiRequestConfig,
} from "@/lib/api-client";
import { connectSSE } from "@/lib/sse";
import { getSecureKey } from "@/lib/storage";
import { bookSourceListSchema, bookSourceSchema, type BookSource } from "@/types/api";

/** `/getInvalidBookSources` 返回项 */
export const invalidBookSourceSchema = z.object({
  sourceUrl: z.string(),
  /** 标记为失效的时间戳 */
  time: z.number().optional(),
  error: z.string().optional(),
});

export const invalidBookSourceListSchema = z.array(invalidBookSourceSchema);

export type InvalidBookSource = z.infer<typeof invalidBookSourceSchema>;

/** `/getBookSources?simple=1` 轻量项: 计数/命名用, 不含规则大字段 (1.4MB → ~33KB) */
export const bookSourceLiteSchema = z.object({
  bookSourceGroup: z.string().optional(),
  bookSourceName: z.string(),
  bookSourceUrl: z.string(),
  enabled: z.boolean().optional(),
  bookSourceType: z.number().optional(),
});
export const bookSourceLiteListSchema = z.array(bookSourceLiteSchema);
export type BookSourceLite = z.infer<typeof bookSourceLiteSchema>;

export async function getBookSources(config?: ApiRequestConfig): Promise<BookSource[]> {
  const data = await get<unknown>("/getBookSources", undefined, config);
  return parseWith(bookSourceListSchema, data);
}

/** 轻量书源列表: 搜索源计数/死源命名等不需要规则字段的消费方专用 */
export async function getBookSourcesLite(
  config?: ApiRequestConfig,
): Promise<BookSourceLite[]> {
  const data = await get<unknown>("/getBookSources?simple=1", undefined, config);
  return parseWith(bookSourceLiteListSchema, data);
}

/** 批量新增/更新书源, 后端 body 是 BookSource JSON 数组 */
export async function saveBookSources(
  list: BookSource[],
  config?: ApiRequestConfig,
): Promise<void> {
  await post<unknown>("/saveBookSources", list, config);
}

/** 删除单个书源, 后端 body 是单个 BookSource 对象 */
export async function deleteBookSource(url: string, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/deleteBookSource", { bookSourceUrl: url }, config);
}

/** 批量删除书源, 后端 body 是 BookSource 对象数组 */
export async function deleteBookSources(
  urls: string[],
  config?: ApiRequestConfig,
): Promise<void> {
  await post<unknown>(
    "/deleteBookSources",
    urls.map((bookSourceUrl) => ({ bookSourceUrl })),
    config,
  );
}

/**
 * 管理操作: 把某个用户的书源设为系统默认书源(对之后注册的用户生效).
 * 后端 body 是 `{username}`, 管理密码走 query 参数 secureKey(未显式传入时取 localStorage).
 */
export async function setAsDefault(
  username: string,
  secureKey?: string,
  config?: ApiRequestConfig,
): Promise<void> {
  const key = secureKey ?? getSecureKey();
  const params: ApiParams = { ...config?.params };
  if (key !== null) {
    params.secureKey = key;
  }
  await post<unknown>("/setAsDefaultBookSources", { username }, { ...config, params });
}

export async function getInvalidBookSources(
  config?: ApiRequestConfig,
): Promise<InvalidBookSource[]> {
  const data = await post<unknown>("/getInvalidBookSources", undefined, config);
  return parseWith(invalidBookSourceListSchema, data);
}

/** `/saveFromRemoteSource?preview=1` 返回: 归一化书源数组 + 已存在书源 URL 列表 */
export const remoteSourcePreviewSchema = z.object({
  sources: z.array(z.unknown()),
  existing: z.array(z.string()).optional(),
});

export interface RemoteSourcePreview {
  /** 后端解析归一化后的书源(按 bookSourceUrl 去重, 非法条目已跳过) */
  sources: BookSource[];
  /** 服务端已存在的书源 URL, 导入时会被覆盖更新 */
  existing: string[];
}

/** `/bookSourceDebugSSE` step 事件的 message (warp service::debug::DebugStep) */
export const debugStepSchema = z.object({
  /** 规则名 (如 ruleSearch.bookList / 请求 URL / 规则应用) */
  ruleName: z.string().default(""),
  /** 请求 URL; 非请求步骤为空串 */
  url: z.string().default(""),
  elapsedMs: z.number().default(0),
  /** 结果长度(字符数/结果条数) */
  resultLen: z.number().default(0),
  /** 错误信息, 无则缺省 */
  error: z.string().optional(),
  /** 解析明细(规则类型/JS 错误片段等), 仅存档不渲染 */
  detail: z.unknown().optional(),
});

/** `/bookSourceDebugSSE` start 事件的 message: {action, bookSource(源名)} */
export const debugStartSchema = z.object({
  action: z.string().default(""),
  bookSource: z.string().default(""),
});

/**
 * `/bookSourceDebugSSE` 的 `data:` 事件负载 (warp):
 * {type:start|step|result|error, message|data}; 全部走匿名 `data:` 行, 没有 `event: end`.
 */
export const sourceDebugEventSchema = z.object({
  type: z.string().default(""),
  message: z.unknown().optional(),
  data: z.unknown().optional(),
});

export type DebugStep = z.infer<typeof debugStepSchema>;

/** 调试动作: search 需 key, explore 走发现规则, toc/content 需 chapterUrl */
export type BookSourceDebugAction = "search" | "explore" | "toc" | "content";

/** step 事件 → 一行日志: "[规则名] 请求URL (结果长度 字 / 错误: 原因)" */
export function formatDebugStep(step: DebugStep): string {
  const head = step.url.length > 0 ? `[${step.ruleName}] ${step.url}` : `[${step.ruleName}]`;
  const error = step.error ?? "";
  return error.length > 0
    ? `${head} (${step.resultLen} 字 / 错误: ${error})`
    : `${head} (${step.resultLen} 字)`;
}

export interface ParsedBookSources {
  /** 通过 schema 校验的书源(按 bookSourceUrl 去重, 同批后出现的覆盖先出现的) */
  sources: BookSource[];
  /** JSON 合法但不是可识别书源而被跳过的条目数 */
  skipped: number;
  /** 文本不是合法书源 JSON 时的错误信息 */
  error: string | null;
}

/**
 * 解析粘贴/远程拉取的书源 JSON 文本: 支持数组或单个对象两种形态.
 * 每个条目过 bookSourceSchema(补齐默认值), 空 bookSourceUrl 或校验失败的条目计入 skipped.
 */
export function parseBookSourcesText(text: string): ParsedBookSources {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return { sources: [], skipped: 0, error: null };
  }
  let raw: unknown;
  try {
    raw = JSON.parse(trimmed) as unknown;
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return { sources: [], skipped: 0, error: `JSON 解析失败: ${reason}` };
  }
  if (typeof raw !== "object" || raw === null) {
    return { sources: [], skipped: 0, error: "内容不是书源 JSON" };
  }
  const items: unknown[] = Array.isArray(raw) ? raw : [raw];
  const merged = new Map<string, BookSource>();
  let skipped = 0;
  for (const item of items) {
    const parsed = bookSourceSchema.safeParse(item);
    if (parsed.success && parsed.data.bookSourceUrl.trim().length > 0) {
      merged.set(parsed.data.bookSourceUrl, parsed.data);
    } else {
      skipped += 1;
    }
  }
  return { sources: [...merged.values()], skipped, error: null };
}

/**
 * 读取远程书源链接并解析为书源列表(导入前预览).
 * warp 移除了 readRemoteSourceFile, 改走 saveFromRemoteSource 的 preview=1 语义:
 * 后端抓取 URL、校验并归一化书源数组后返回 {sources, existing}, 不写库; 正式导入
 * 仍由前端把预览到的列表经 saveBookSources 提交(所见即所存, 书源数上限在保存时校验).
 * 一个书源都没有时抛错.
 */
export async function previewRemoteSources(
  url: string,
  config?: ApiRequestConfig,
): Promise<RemoteSourcePreview> {
  const data = await post<unknown>(
    "/saveFromRemoteSource",
    { url },
    { ...config, params: { ...config?.params, preview: 1 } },
  );
  const parsed = parseWith(remoteSourcePreviewSchema, data);
  const merged = new Map<string, BookSource>();
  for (const item of parsed.sources) {
    const source = bookSourceSchema.safeParse(stripNullsDeep(item));
    if (source.success && source.data.bookSourceUrl.trim().length > 0) {
      merged.set(source.data.bookSourceUrl, source.data);
    }
  }
  if (merged.size === 0) {
    throw new ApiError("远程书源文件里没有可识别的书源");
  }
  // warp 对远程 JSON 缺 enabled 键归一化为 false (legacy 前端语义为 true):
  // 非 existing 的新源客户端兜底 enabled=true, 与 legacy 导入体验一致;
  // existing 源保留后端真值, 避免覆盖更新时把用户停用的源重新启用.
  const existingSet = new Set(parsed.existing ?? []);
  for (const [url, source] of merged) {
    if (!existingSet.has(url)) {
      source.enabled = true;
    }
  }
  return { sources: [...merged.values()], existing: parsed.existing ?? [] };
}

export interface BookSourceDebugHandlers {
  /** 每个调试事件渲染成的一行日志 (start/step/result 已格式化为展示文案) */
  onLog: (line: string) => void;
  /** 调试完成 (收到 result 事件) */
  onEnd?: () => void;
  /** `event: error`、流内 error 事件或连接中断 */
  onError?: (error: Error) => void;
}

/**
 * 书源调试 (GET SSE `/bookSourceDebugSSE`, warp 契约):
 * 参数 action=search|explore|toc|content + key (search 必填) + bookSource (完整书源
 * 对象 JSON, 后端 resolve_book_source 直接反序列化) + chapterUrl (toc/content 用).
 * 流负载 data:{type,message}: start → step* → result | error; 校验失败时后端发
 * `event: error` + 标准封装 (connectSSE 的 onError 分支). warp 不发 `event: end`,
 * 收到 result/error 事件后由本函数主动关流, 免得 EventSource 自动重连重跑调试.
 * @returns 取消函数: 关闭连接(幂等).
 */
export function bookSourceDebugSSE(
  source: BookSource,
  action: BookSourceDebugAction,
  options: { key?: string; chapterUrl?: string },
  handlers: BookSourceDebugHandlers,
): () => void {
  // connectSSE 的返回值要在事件回调里用来主动关流, 先占位再回填(事件都是异步到达)
  let finish: (() => void) | null = null;
  const cancel = connectSSE(
    "/reader3/bookSourceDebugSSE",
    { action, key: options.key, chapterUrl: options.chapterUrl, bookSource: JSON.stringify(source) },
    {
      onData: (payload) => {
        const parsed = sourceDebugEventSchema.safeParse(stripNullsDeep(payload));
        if (!parsed.success) {
          // 兜底: 非标准负载若是非空字符串也当日志展示
          if (typeof payload === "string" && payload.trim().length > 0) {
            handlers.onLog(payload);
          }
          return;
        }
        const event = parsed.data;
        if (event.type === "step") {
          const step = debugStepSchema.safeParse(stripNullsDeep(event.message));
          if (step.success) {
            handlers.onLog(formatDebugStep(step.data));
          }
          return;
        }
        if (event.type === "start") {
          const start = debugStartSchema.safeParse(stripNullsDeep(event.message));
          const name = start.success ? start.data.bookSource : source.bookSourceName;
          handlers.onLog(`开始调试 ${name} · ${action}`);
          return;
        }
        if (event.type === "result") {
          const count = Array.isArray(event.data) ? event.data.length : null;
          handlers.onLog(count === null ? "调试结束" : `调试结束 · 得到 ${count} 条结果`);
          finish?.();
          handlers.onEnd?.();
          return;
        }
        if (event.type === "error") {
          const message =
            typeof event.message === "string" && event.message.trim().length > 0
              ? event.message
              : "调试失败: 后端没有返回原因";
          finish?.();
          handlers.onError?.(new ApiError(message));
        }
      },
      onError: handlers.onError,
    },
  );
  finish = cancel;
  return cancel;
}

/** `/getSourceStats` 返回项: 书源置信度统计 (搜索排序与清理依据) */
export const sourceStatSchema = z.object({
  sourceUrl: z.string(),
  /** 0..1 置信度: 搜索成功率.5 + 速度.2 + 目录.1 + 正文.1 + 丰富度.1 */
  confidence: z.number(),
  attempts: z.number(),
  /** 0..1 搜索成功率 */
  successRate: z.number(),
  avgLatencyMs: z.number(),
  consecFailures: z.number(),
  lastFailureAt: z.number(),
});
export type SourceStat = z.infer<typeof sourceStatSchema>;
export const sourceStatListSchema = z.array(sourceStatSchema);

export async function getSourceStats(config?: ApiRequestConfig): Promise<SourceStat[]> {
  const data = await get<unknown>("/getSourceStats", undefined, config);
  return parseWith(sourceStatListSchema, stripNullsDeep(data));
}

/** 书源登录结果 (warp /reader3/loginBookSource 软性结果联合体) */
export interface SourceLoginResult {
  success: boolean;
  /** 需要图片验证码: captchaUrl 展示, 用户输入后带 captcha 重试 */
  needCaptcha?: boolean;
  captchaUrl?: string;
  captchaId?: string;
  /** 点击类验证码等无法自动处理 → 引导手动粘贴 Cookie */
  needManualCaptcha?: boolean;
  message?: string;
  cookie?: string;
}

/** 书源登录: 表单 HTTP 登录 (mode=browser 走 camoufox 自动过验证码) */
export async function loginBookSource(
  params: {
    bookSource: string;
    username?: string;
    password?: string;
    captcha?: string;
    captchaId?: string;
    mode?: "http" | "browser";
  },
  config?: ApiRequestConfig,
): Promise<SourceLoginResult> {
  const data = await post<SourceLoginResult>("/loginBookSource", params, config);
  return data;
}

/** 当前用户全部书源登录态 (cookie 行) */
export interface SourceCookieRow {
  sourceUrl: string;
  cookie: string;
  userAgent?: string;
  loginHeader?: string;
  updatedAt?: number;
}

/** 读取当前用户书源登录态列表 (登录徽标/对话框状态用) */
export async function getBookSourceCookie(
  config?: ApiRequestConfig,
): Promise<SourceCookieRow[]> {
  const data = await get<SourceCookieRow[]>("/getBookSourceCookie", undefined, config);
  return data ?? [];
}

/** 手动设置书源 Cookie (空串 = 清除); 点击验证码等场景的兜底 */
export async function setBookSourceCookie(
  bookSource: string,
  cookie: string,
  config?: ApiRequestConfig,
): Promise<void> {
  await post<unknown>("/setBookSourceCookie", { bookSource, cookie }, config);
}
