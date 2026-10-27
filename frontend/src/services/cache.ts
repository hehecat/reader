import { z } from "zod";

import { get, parseWith, post, stripNullsDeep, type ApiRequestConfig } from "@/lib/api-client";
import { connectSSE, type SSEParams } from "@/lib/sse";
import { bookSchema } from "@/types/api";
import { toast } from "@/components/ui";

/** 自动预热去重 (bookUrl 维度, 模块级): 加架/导入后后台整书缓存 */
const autoCaching = new Set<string>();

/**
 * 后台整书缓存 (静默启动): 加架/导入成功后调用, 把首开几十秒的目录+正文抓取
 * 前移到加架时刻; 完成轻 toast, 失败静默 (自动预热不打扰).
 * 返回是否真正启动 (重复调用同书返回 false).
 */
export function ensureCacheStream(url: string): boolean {
  if (url.length === 0 || url.startsWith("local://") || autoCaching.has(url)) {
    return false;
  }
  autoCaching.add(url);
  startCacheBookStream(url, {
    onProgress: () => {},
    onDone: (progress) => {
      autoCaching.delete(url);
      if (!progress.cancelled) {
        toast.success(`后台缓存完成 · ${progress.cached} 章`);
      }
    },
    onError: () => {
      autoCaching.delete(url);
    },
  });
  return true;
}

/**
 * `/getShelfBookWithCacheInfo` (带 url) 返回: 书架 Book + 服务端缓存统计.
 * warp 实际字段名为 cacheChapterCount/cacheSize (字节数), 其余实体字段与 bookSchema 同源.
 */
export const bookCacheInfoSchema = bookSchema.extend({
  /** 已缓存章节数 (服务端 book_chapters 缓存表计数) */
  cacheChapterCount: z.number().default(0),
  /** 已缓存正文总字节数 */
  cacheSize: z.number().default(0),
});

export type BookCacheInfo = z.infer<typeof bookCacheInfoSchema>;

/**
 * `cacheBookSSE` message 事件的 data 负载 (无 event: end, 终态后服务端直接关流).
 * legacy 别名字段 (cachedCount/successCount/failedCount) UI 不消费, zod 默认剥离.
 */
export const cacheProgressSchema = z.object({
  /** 已缓存章节数 */
  cached: z.number().default(0),
  /** 总章节数 (目录解析完成前为 0) */
  total: z.number().default(0),
  /** 书名 (任务刚启动时为 url, 目录就绪后为书名) */
  title: z.string().default(""),
  /** 任务结束 (成功或失败) */
  finished: z.boolean().default(false),
  /** 任务被取消 */
  cancelled: z.boolean().default(false),
  /** 失败原因 (finished 且失败时非空) */
  error: z.string().optional(),
});

export type CacheProgress = z.infer<typeof cacheProgressSchema>;

/** `/cancelCacheBook` 返回: 是否命中运行中的任务 */
const cancelResultSchema = z.object({
  cancelled: z.boolean().default(false),
});

/** 单本书的缓存统计 (书必须在架, 否则后端报「书籍不存在」) */
export async function getBookCacheInfo(
  url: string,
  config?: ApiRequestConfig,
): Promise<BookCacheInfo> {
  const data = await post<unknown>("/getShelfBookWithCacheInfo", { url }, config);
  return parseWith(bookCacheInfoSchema, data);
}

/** 取消服务端缓存任务 (taskId 缺省即 url); 返回是否命中任务 */
export async function cancelCacheBook(url: string, config?: ApiRequestConfig): Promise<boolean> {
  const data = await post<unknown>("/cancelCacheBook", { url }, config);
  return parseWith(cancelResultSchema, data).cancelled;
}

/** `/getCacheInfo` 返回: 当前用户的目录 / 正文缓存统计 (大小为近似字节数) */
export const cacheInfoSchema = z.object({
  tocCacheCount: z.number().default(0),
  tocCacheSize: z.number().default(0),
  chapterCount: z.number().default(0),
  chapterSize: z.number().default(0),
  totalSize: z.number().default(0),
});

export type CacheInfo = z.infer<typeof cacheInfoSchema>;

export type ClearCacheType = "toc" | "chapters" | "all";

/** 缓存统计 (设置页「缓存」卡片) */
export async function getCacheInfo(config?: ApiRequestConfig): Promise<CacheInfo> {
  const data = await get<unknown>("/getCacheInfo", undefined, config);
  return parseWith(cacheInfoSchema, data);
}

/** 清空缓存: toc = 目录缓存, chapters = 正文缓存, all = 全部 */
export async function clearCache(
  type: ClearCacheType,
  config?: ApiRequestConfig,
): Promise<void> {
  await post<unknown>("/clearCache", { type }, config);
}

export interface CacheStreamHandlers {
  /** 每帧进度 (服务端 300ms 轮询推送) */
  onProgress: (progress: CacheProgress) => void;
  /** 终态负载 (finished 或 cancelled=true), 随后本地流关闭 */
  onDone: (progress: CacheProgress) => void;
  /** 业务错误 (event: error) 或连接中断; 终态后的正常关流不触发 */
  onError: (err: Error) => void;
}

/**
 * 打开缓存进度流的公共实现.
 * 服务端发完终态负载才关流, 原生 EventSource 会把关流报成连接错误:
 * 这里先记下终态 (settled), 之后的 onError 一律吞掉; 收到 cancelled=true 时
 * 服务端可能还在等当前章完成, 客户端视为终态直接收流.
 */
function openCacheStream(params: SSEParams, handlers: CacheStreamHandlers): () => void {
  let settled = false;
  const close = connectSSE("/reader3/cacheBookSSE", params, {
    onData: (payload) => {
      if (settled) {
        return;
      }
      // SSE 负载不经 api-client 解包, Option 字段的显式 null 先归一再进 zod
      const parsed = cacheProgressSchema.safeParse(stripNullsDeep(payload));
      if (!parsed.success) {
        return;
      }
      const progress = parsed.data;
      handlers.onProgress(progress);
      if (progress.finished || progress.cancelled) {
        settled = true;
        handlers.onDone(progress);
        close();
      }
    },
    onError: (err) => {
      if (settled) {
        return;
      }
      settled = true;
      handlers.onError(err);
    },
  });
  return () => {
    settled = true;
    close();
  };
}

/**
 * 启动整书缓存并流式推进度: warp 对同 url 无运行中任务时就地自启动,
 * 已有任务则附着其进度 (一次调用即完成「启动+监听」).
 * @returns 断开本地流的函数 (服务端任务继续运行, 可再次调用重新附着)
 */
export function startCacheBookStream(url: string, handlers: CacheStreamHandlers): () => void {
  return openCacheStream({ url }, handlers);
}

/**
 * 只读附着探测: 仅当同 url 已有任务时接上进度流 (传 taskId, 服务端不自启动);
 * 无任务时 onError 收到「缓存任务不存在」, 调用方应静默处理.
 */
export function attachCacheBookStream(url: string, handlers: CacheStreamHandlers): () => void {
  return openCacheStream({ taskId: url }, handlers);
}
