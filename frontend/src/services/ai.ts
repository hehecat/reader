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

export interface AiChatRequest {
  url: string;
  question: string;
  bookName?: string;
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
  onDone?: () => void;
  onError?: (message: string) => void;
}

interface AiEvent {
  type?: string;
  model?: string;
  snippets?: AiSnippet[];
  chars?: number;
  text?: string;
  message?: string;
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
        case "delta":
          handlers.onDelta?.(String(event.text ?? ""));
          break;
        case "done":
          finished = true;
          handlers.onDone?.();
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
  const data = await post<unknown>("/aiGraph", input);
  return aiGraphSchema.parse(data).graph;
}
