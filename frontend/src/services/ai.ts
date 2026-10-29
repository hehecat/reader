import { z } from "zod";

import { API_BASE_URL, get, post } from "@/lib/api-client";
import { getAccessToken } from "@/lib/storage";

/** AI 助手可用性：未配置端点时 enabled=false（前端隐藏入口） */
export const aiStatusSchema = z.object({
  enabled: z.boolean(),
  model: z.string().nullable().optional(),
});
export type AiStatus = z.infer<typeof aiStatusSchema>;

export async function getAiStatus(): Promise<AiStatus> {
  const data = await get<unknown>("/aiStatus");
  return aiStatusSchema.parse(data);
}

export interface AiSnippet {
  /** 章节缓存键（书源书为 URL 哈希，前端只用 title 反查） */
  index: number;
  title: string;
}

/** 检索范围: 只看当前章及前后各一章(默认) / 全书放宽覆盖 */
export type AiScope = "current" | "book";

export interface AiChatRequest {
  url: string;
  question: string;
  bookName?: string;
  /** 检索范围; 缺省由后端按 current 处理 */
  scope?: AiScope;
  /** 章节标题序（后端按它排序，无命中时取"开头两章 + 当前章附近"） */
  titles?: string[];
  currentTitle?: string;
  history?: { role: "user" | "assistant"; content: string }[];
}

export interface AiChatHandlers {
  onMeta?: (meta: { model?: string; snippets: AiSnippet[] }) => void;
  /** 推理模型思考进度（字符数），用于"思考中"提示 */
  onThinking?: (chars: number) => void;
  onDelta?: (text: string) => void;
  /** 过程提示（如"输出达到上限, 自动续写中"） */
  onNotice?: (message: string) => void;
  /** finish = "length" 表示达到 max_tokens 被截断 */
  onDone?: (finish?: string) => void;
  onError?: (message: string) => void;
}

interface AiEvent {
  type?: string;
  model?: string;
  snippets?: AiSnippet[];
  chars?: number;
  text?: string;
  message?: string;
  finish?: string;
}

/**
 * 基于已缓存章节的问答（POST + fetch 流式）。
 * EventSource 只支持 GET，而历史对话要放在 body 里，所以这里手写 SSE 解析。
 * @returns 取消函数：中止流并触发 onDone
 */
export function streamAiChat(req: AiChatRequest, handlers: AiChatHandlers): () => void {
  const controller = new AbortController();

  void (async () => {
    const token = getAccessToken();
    const query = token === null ? "" : `?accessToken=${encodeURIComponent(token)}`;
    const resp = await fetch(`${API_BASE_URL}/aiChat${query}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token === null ? {} : { accessToken: token }),
      },
      body: JSON.stringify(req),
      signal: controller.signal,
    });
    if (!resp.ok || resp.body === null) {
      handlers.onError?.(`请求失败 (HTTP ${resp.status})`);
      return;
    }
    const reader = resp.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    let finished = false;

    const handle = (event: AiEvent): void => {
      switch (event.type) {
        case "meta":
          handlers.onMeta?.({ model: event.model, snippets: event.snippets ?? [] });
          break;
        case "thinking":
          handlers.onThinking?.(Number(event.chars) || 0);
          break;
        case "notice":
          handlers.onNotice?.(String(event.message ?? ""));
          break;
        case "delta":
          handlers.onDelta?.(String(event.text ?? ""));
          break;
        case "done":
          finished = true;
          handlers.onDone?.(typeof event.finish === "string" ? event.finish : undefined);
          break;
        case "error":
          finished = true;
          handlers.onError?.(String(event.message ?? "未知错误"));
          break;
        default:
          break;
      }
    };

    for (;;) {
      const { value, done } = await reader.read();
      if (done) {
        break;
      }
      buf += decoder.decode(value, { stream: true });
      let split = buf.indexOf("\n\n");
      while (split >= 0) {
        const chunk = buf.slice(0, split);
        buf = buf.slice(split + 2);
        const data = chunk
          .split("\n")
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trim())
          .join("");
        if (data !== "") {
          try {
            handle(JSON.parse(data) as AiEvent);
          } catch {
            // 非 JSON 心跳/空行：忽略
          }
        }
        split = buf.indexOf("\n\n");
      }
    }
    if (!finished) {
      handlers.onDone?.();
    }
  })().catch((error: unknown) => {
    const name = (error as Error | null)?.name;
    if (name === "AbortError") {
      handlers.onDone?.();
    } else {
      handlers.onError?.((error as Error).message || "连接中断");
    }
  });

  return () => controller.abort();
}

/** 设置页: AI 端点配置（密钥只回是否已设置） */
export const aiSettingsSchema = z.object({
  enabled: z.boolean(),
  /** saved = 设置页保存; env = 环境变量; none = 未配置 */
  source: z.string(),
  baseUrl: z.string(),
  model: z.string(),
  maxTokens: z.number(),
  /** 思考档位: off / low / medium / high（开启会更慢更贵, 也更容易被预算截断） */
  thinking: z.string().default("off"),
  keySet: z.boolean(),
});
export type AiSettings = z.infer<typeof aiSettingsSchema>;

export async function getAiSettings(): Promise<AiSettings> {
  const data = await get<unknown>("/aiSettings");
  return aiSettingsSchema.parse(data);
}

export interface AiSettingsInput {
  baseUrl: string;
  /** 空串 = 保留已保存的密钥 */
  apiKey: string;
  model: string;
  maxTokens: number;
  /** off / low / medium / high */
  thinking: string;
}

/** 保存端点/密钥/模型：服务端立即生效，无需重启 */
export async function saveAiSettings(input: AiSettingsInput): Promise<void> {
  await post<unknown>("/aiSettings", input);
}

export interface AiTestResult {
  ok: boolean;
  status?: number;
  latencyMs?: number;
  models?: number;
}

/** 探测端点连通性（GET /models），可用未保存的输入 */
export async function testAiEndpoint(input: AiSettingsInput): Promise<AiTestResult> {
  const data = await post<unknown>("/aiTest", input);
  return data as AiTestResult;
}

/**
 * 列出端点支持的模型 id（下拉用），同样接受未保存的输入。
 * 端点不支持 /models 时后端返回空数组，前端继续允许手填。
 */
export async function listAiModels(input: AiSettingsInput): Promise<string[]> {
  const data = await post<unknown>("/aiModels", input);
  return aiModelsSchema.parse(data).models;
}

const aiModelsSchema = z.object({ models: z.array(z.string()).default([]) });

/** 关系图: 人物 + 关系（后端让模型输出 JSON 后透传） */
export const graphCharacterSchema = z.object({
  name: z.string(),
  alias: z.string().optional(),
  firstChapter: z.string().optional(),
  role: z.string().optional(),
});
export const graphRelationSchema = z.object({
  from: z.string(),
  to: z.string(),
  label: z.string().optional(),
  chapter: z.string().optional(),
});
export const aiGraphSchema = z.object({
  graph: z.object({
    characters: z.array(graphCharacterSchema).default([]),
    relations: z.array(graphRelationSchema).default([]),
  }),
  cachedChapters: z.number().optional(),
});
export type AiGraphData = z.infer<typeof aiGraphSchema>["graph"];

export interface AiGraphInput {
  url: string;
  bookName?: string;
  titles?: string[];
  currentTitle?: string;
  /** 可选: 重点关注的人物/关系 */
  question?: string;
}

export async function generateAiGraph(input: AiGraphInput): Promise<AiGraphData> {
  // 推理模型生成结构化图较慢(实测 20-30s, 偶发更久): 单独放宽到 180s, 避免默认超时误杀
  const data = await post<unknown>("/aiGraph", input, { timeout: 180_000 });
  return aiGraphSchema.parse(data).graph;
}

/** 设置页: 判定模型（Jev / EMRank）配置——端点与 chat 端点不同, 单独存 */
export const jevSettingsSchema = z.object({
  enabled: z.boolean(),
  baseUrl: z.string(),
  model: z.string(),
  keySet: z.boolean(),
});
export type JevSettings = z.infer<typeof jevSettingsSchema>;

export async function getJevSettings(): Promise<JevSettings> {
  const data = await get<unknown>("/jevSettings");
  return jevSettingsSchema.parse(data);
}

export interface JevSettingsInput {
  baseUrl: string;
  /** 空串 = 保留已保存的密钥 */
  apiKey: string;
  model: string;
}

export async function saveJevSettings(input: JevSettingsInput): Promise<void> {
  await post<unknown>("/jevSettings", input);
}

/** 测试连接: 后端真发一次封闭式判定（state + questions），回显答案 */
export interface JevTestResult {
  ok: boolean;
  latencyMs?: number;
  /** 每个问题的答案, 如 { dead: { noul: 0.02 }, who: { choice: "zhaojiu" } } */
  answers?: unknown;
}

export async function testJevEndpoint(input: JevSettingsInput): Promise<JevTestResult> {
  const data = await post<unknown>("/jevTest", input);
  return data as JevTestResult;
}
