/**
 * 关系图谱的「整本批量生成」链路 (后端 service::graph).
 *
 * 五个接口:
 *   GET  /graphEstimate?url=   预估(已缓存章数 / 批数 / token 与花费说明)
 *   POST /graphStart           确认后启动任务 → {jobId}
 *   GET  /graphSSE?url=        进度帧(queued/running/done/failed/cancelled)
 *   POST /graphCancel          取消任务
 *   GET  /graph?url=           生成成品; graph:null 表示这本书还没生成过
 *
 * 解析策略与 demo 一致: zod 宽容(缺字段给兜底值, 未知字段忽略),
 * 后端以后加字段不会让整页崩掉; 唯一"不宽容"的是 characters 为空 —— 那等于没数据.
 */
import * as React from "react";
import { z } from "zod";

import { toApiError, get, parseWith, post, stripNullsDeep } from "@/lib/api-client";
import { connectSSE } from "@/lib/sse";
import { graphDemoSchema, graphMetaSchema } from "@/types/graphDemo";

/** 数字字段: 缺字段/类型不对都给兜底值, 取整 */
const int = (fallback: number) =>
  z
    .number()
    .catch(fallback)
    .transform((value) => Math.round(value));

/** 文本字段: 缺字段给默认值 */
const text = (fallback: string) => z.string().catch(fallback);

// ---- 预估 ----

export const graphEstimateSchema = z.object({
  /** 已缓存(可离线送模型的)章数 */
  cachedChapters: int(0),
  /** 要送模型的分批数 */
  batches: int(0),
  /** 预估 prompt token 总量 */
  estPromptTokens: int(0),
  /** 花费说明的人话(每批大概多少钱), 由后端给, 前端只展示 */
  estCostNote: text(""),
});

export type GraphEstimate = z.infer<typeof graphEstimateSchema>;

// ---- 启动 / 取消 ----

export const graphStartResultSchema = z.object({
  jobId: text(""),
});

export type GraphStartResult = z.infer<typeof graphStartResultSchema>;

export const GRAPH_STATUSES = ["queued", "running", "done", "failed", "cancelled"] as const;

export type GraphStatus = (typeof GRAPH_STATUSES)[number];

/** 未知状态按 running 处理(不会误判成终态把流关掉) */
const graphStatusSchema = z.enum(GRAPH_STATUSES).catch("running");

export const graphProgressSchema = z.object({
  status: graphStatusSchema,
  doneBatches: int(0),
  totalBatches: int(0),
  characters: int(0),
  relations: int(0),
  /** 失败原因(其余状态为 null/缺省) */
  error: z
    .string()
    .nullish()
    .transform((value) => (value == null || value === "" ? null : value)),
});

export type GraphProgress = z.infer<typeof graphProgressSchema>;

/** 任务是否已经停下(完成/失败/取消) —— 这些状态之后(含服务端未关流)不再更新 UI */
export function isGraphTerminal(status: GraphStatus): boolean {
  return status === "done" || status === "failed" || status === "cancelled";
}

// ---- 成品 ----

/**
 * 成品元信息: demo 契约 + 后端附加字段.
 * source 目前只有 "batch"; cost 形状后端未定, 只透传不解析(z.unknown 天然忽略多余字段).
 */
export const bookGraphMetaSchema = graphMetaSchema.extend({
  source: text("batch"),
  cost: z.unknown().optional(),
});

export const bookGraphSchema = graphDemoSchema.extend({
  meta: bookGraphMetaSchema,
});

export type BookGraphMeta = z.infer<typeof bookGraphMetaSchema>;
export type BookGraphData = z.infer<typeof bookGraphSchema>;

export interface BookGraphResult {
  /** 是否已有可渲染的成品(没生成过 / 生成了但一个人都没有 → false) */
  hasData: boolean;
  data: BookGraphData | null;
}

// ---- 调用 ----

/**
 * 后端兜底路由对「未注册的接口」回的错误串 (旧镜像里 /graph* 整组都没有, 见 router.rs 尾部 handler).
 * 只认这一个固定串 (trim 后全等), 不做包含匹配 —— 放宽一次就会把「接口不存在对应的数据」
 * 这类真实错误也吞成「版本旧」.
 */
const MISSING_API_MESSAGES: ReadonlySet<string> = new Set(["接口不存在"]);

/**
 * 这次失败是不是「后端没这个接口」(服务端版本较旧), 而不是真出了错.
 *
 * 两条依据覆盖后端两种可能行为:
 *   1. 兜底 handler 走标准信封 + HTTP 200 → api-client 直接抛 errorMsg 原文「接口不存在」;
 *   2. 路由层 404/501 且没有信封 → api-client 抛「请求失败 (HTTP 404)」, 原始 axios 错误挂在
 *      cause 上, 这里沿 cause 链看 status.
 * 其它 errorMsg(「系统错误」等)/ 网络失败 / 超时 / 响应格式错误一律 false, 照常当真实错误报.
 */
export function isGraphApiMissing(error: unknown): boolean {
  let current: unknown = error;
  for (let depth = 0; current instanceof Error && depth < 5; depth += 1) {
    if (MISSING_API_MESSAGES.has(current.message.trim())) {
      return true;
    }
    const status = (current as Error & { response?: { status?: unknown } }).response?.status;
    if (status === 404 || status === 501) {
      return true;
    }
    current = (current as { cause?: unknown }).cause;
  }
  return false;
}

/** 预估: 生成前给用户看「多少章 / 几批 / 大概花多少钱」 */
export async function getGraphEstimate(bookUrl: string): Promise<GraphEstimate> {
  return parseWith(graphEstimateSchema, await get<unknown>("/graphEstimate", { url: bookUrl }));
}

/** 确认生成: confirm 恒为 true(确认框就是这道闸), 后端据此启动任务 */
export async function startGraphGeneration(input: {
  url: string;
  bookName?: string;
  /** 调试用: 只跑前 N 批 */
  maxBatches?: number;
}): Promise<string> {
  const result = parseWith(
    graphStartResultSchema,
    await post<unknown>("/graphStart", {
      url: input.url,
      bookName: input.bookName ?? "",
      confirm: true,
      maxBatches: input.maxBatches,
    }),
  );
  if (result.jobId === "") {
    throw new Error("启动失败: 后端没有返回任务号");
  }
  return result.jobId;
}

/** 取消: 已结束的任务后端按幂等处理 */
export async function cancelGraphGeneration(bookUrl: string): Promise<void> {
  await post<unknown>("/graphCancel", { url: bookUrl });
}

/**
 * 取成品. 兼容两种返回形状: `{graph: {...} | null, meta?}` 与直接把成品摊平返回.
 * 未生成过(或成品为空)一律落到 hasData: false, 由页面显示空态.
 */
export async function getBookGraph(bookUrl: string): Promise<BookGraphResult> {
  const raw = await get<unknown>("/graph", { url: bookUrl });
  const payload = unwrapGraphPayload(raw);
  if (payload === undefined || payload === null) {
    return { hasData: false, data: null };
  }
  const data = parseWith(bookGraphSchema, payload);
  if (data.characters.length === 0) {
    return { hasData: false, data: null };
  }
  return { hasData: true, data };
}

/** `{graph: null}` / `{graph: {...}}` 取内层; 已经是成品对象则原样返回 */
function unwrapGraphPayload(raw: unknown): unknown {
  if (typeof raw !== "object" || raw === null) {
    return undefined;
  }
  const record = raw as Record<string, unknown>;
  if ("graph" in record) {
    return stripNullsDeep(record.graph);
  }
  return raw;
}

// ---- 进度流 ----

export interface GraphStreamHandlers {
  onProgress: (progress: GraphProgress) => void;
  onError: (error: Error) => void;
}

/**
 * 附着进度流 (与 openCacheStream 同款): SSE 负载不经 api-client 解包, 先进
 * stripNullsDeep 把显式 null 归一, 再进 zod; 终态后主动关流并吞掉关流错误
 * (服务端可能还挂着连接, EventSource 关流不该冒泡成用户可见错误).
 * @returns 断开本地流的函数(幂等; 服务端任务不受影响)
 */
export function openGraphStream(bookUrl: string, handlers: GraphStreamHandlers): () => void {
  let settled = false;
  const close = connectSSE(
    "/reader3/graphSSE",
    { url: bookUrl },
    {
      onData: (payload) => {
        if (settled) {
          return;
        }
        const parsed = graphProgressSchema.safeParse(stripNullsDeep(payload));
        if (!parsed.success) {
          return;
        }
        handlers.onProgress(parsed.data);
        if (isGraphTerminal(parsed.data.status)) {
          settled = true;
          close();
        }
      },
      onError: (error) => {
        if (settled) {
          return;
        }
        settled = true;
        handlers.onError(error);
      },
    },
  );
  return () => {
    settled = true;
    close();
  };
}

export interface GraphGeneration {
  /** 当前进度(null = 还没开始) */
  progress: GraphProgress | null;
  /** 启动请求进行中 */
  starting: boolean;
  /** 启动失败原文 */
  error: string | null;
  /** 确认后启动任务并附着进度流 */
  start: (options?: { bookName?: string; maxBatches?: number }) => Promise<void>;
  /** 取消服务端任务(流本身留在原位等 cancelled 帧) */
  cancel: () => Promise<void>;
  /** 收尾: 断开流 + 清空状态(关对话框时调) */
  reset: () => void;
}

/**
 * 生成流程的页内状态: 启动 → 进度 → 终态.
 * 断开流不会取消服务端任务(切走再回来重开一次即重新附着), 所以只在关闭对话框时 reset.
 */
export function useGraphGeneration(bookUrl: string): GraphGeneration {
  const [progress, setProgress] = React.useState<GraphProgress | null>(null);
  const [starting, setStarting] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const detachRef = React.useRef<(() => void) | null>(null);

  const detach = React.useCallback(() => {
    detachRef.current?.();
    detachRef.current = null;
  }, []);

  // 卸载/换书: 断开本地流(服务端任务继续跑)
  React.useEffect(() => detach, [detach]);

  const start = React.useCallback(
    async (options: { bookName?: string; maxBatches?: number } = {}) => {
      if (bookUrl === "") {
        return;
      }
      setError(null);
      setStarting(true);
      // 先给一帧 queued: 对话框立刻从「确认」切到「进度」, 不必等 SSE 首帧
      setProgress({
        status: "queued",
        doneBatches: 0,
        totalBatches: 0,
        characters: 0,
        relations: 0,
        error: null,
      });
      try {
        await startGraphGeneration({ url: bookUrl, ...options });
        detach();
        detachRef.current = openGraphStream(bookUrl, {
          onProgress: setProgress,
          onError: (err) => {
            setError(err.message);
          },
        });
      } catch (err) {
        // 启动就失败: 退回确认态并展示原因(而不是停在一条永远不动的进度条上)
        setProgress(null);
        setError(toApiError(err).message);
      } finally {
        setStarting(false);
      }
    },
    [bookUrl, detach],
  );

  const cancel = React.useCallback(async () => {
    try {
      await cancelGraphGeneration(bookUrl);
    } catch (err) {
      setError(toApiError(err).message);
    }
  }, [bookUrl]);

  const reset = React.useCallback(() => {
    detach();
    setProgress(null);
    setError(null);
  }, [detach]);

  return { progress, starting, error, start, cancel, reset };
}
