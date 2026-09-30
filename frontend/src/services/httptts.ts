import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { z } from "zod";

import { get, parseWith, post, type ApiRequestConfig } from "@/lib/api-client";

/**
 * HTTP TTS (神经音色) 服务层:
 * - buildTtsUrl / fetchTtsAudioUrl: 模板 → 请求 URL → 音频 objectURL (useTts 的 template 音源消费)
 * - testHttpTts: 设置「自定义模板 · 测试」的短文本连通性检查
 * - 多引擎网关 (默认 http://127.0.0.1:9912): 前端按 GET /engines 能力清单选择音源 ——
 *   fetchGatewayStatus/fetchGatewayVoices 及其 react-query 封装 (网关不可达静默降级),
 *   buildGatewayTtsUrl/fetchGatewayTtsAudioUrl 合成, testGatewayEngine 供设置区「测试」按钮;
 *   v3 运行时配置 (密钥/端点/模板不再依赖网关环境变量): fetchGatewayConfig/useGatewayConfig
 *   掩码视图, updateGatewayConfig/useUpdateGatewayConfig 增量保存 (空串=清除),
 *   verifyGatewayEngine/useTestGatewayEngine 走 POST /test 真实合成验证 (配置弹窗「验证」)
 * - getHttpTTSList / saveHttpTTS / deleteHttpTTS: warp 听书源存储路由
 *   (wire 名以 serde rename 为准: type / contentType / concurrentRate; id 与 url 同值)
 */

/** {text} 替换上限 (字): 超长段落截断, 避免拼出被服务拒绝的超长 URL */
export const HTTP_TTS_TEXT_LIMIT = 500;

/** 合成请求超时 (ms): 一段神经合成通常 1-3s, 留足余量 */
export const HTTP_TTS_TIMEOUT_MS = 15_000;

/** TtsBar 音色预设 (edge-tts 中文神经音色) */
export const HTTP_TTS_VOICE_PRESETS = [
  { value: "zh-CN-XiaoxiaoNeural", label: "晓晓 · 女 · 暖" },
  { value: "zh-CN-YunxiNeural", label: "云希 · 男 · 叙述" },
  { value: "zh-CN-YunjianNeural", label: "云健 · 男 · 沉" },
  { value: "zh-CN-YunyangNeural", label: "云扬 · 男 · 播报" },
] satisfies { value: string; label: string }[];

/**
 * 中文音色过滤(全站统一口径, 阅读器与设置面板共用):
 * - edge 的音色 id 带 BCP-47 前缀(`zh-CN-…`), 非 zh 的一律不要;
 * - kokoro 用 `zf_`/`zm_` 或中文名;
 * - 火山/阿里/腾讯等引擎 id 多为中文或拼音(无 locale 前缀) → 原样保留,
 *   避免"一刀切过滤"把它们全滤空(曾因此导致无声)。
 */
export function filterChineseVoices<T extends { id: string; name: string }>(
  engine: string,
  voices: readonly T[],
): T[] {
  if (engine === "edge") {
    return voices.filter((v) => v.id.startsWith("zh-") || v.name.startsWith("zh-"));
  }
  if (engine === "kokoro") {
    return voices.filter(
      (v) => v.id.startsWith("zf_") || v.id.startsWith("zm_") || v.name.startsWith("中文"),
    );
  }
  // 其他引擎: 仅剔除明确带非中文 locale 前缀的音色(如 af-ZA- / en-US-)
  return voices.filter((v) => {
    const locale = /^([a-z]{2})-/.exec(v.id)?.[1];
    return locale === undefined || locale === "zh";
  });
}

/** query 拼接: 已有 query 用 & 续, 没有就 ? 起头 (与 warp build_http_tts_url 同语义) */
function appendQuery(url: string, pair: string): string {
  return url.includes("?") ? `${url}&${pair}` : `${url}?${pair}`;
}

/**
 * 模板 → 请求 URL: {text} 用 encodeURIComponent(段落纯文本, 截断 500 字) 替换,
 * {voice} 用音色名替换; 模板没有对应占位时降级成 query 参数 (text= / voice=).
 */
export function buildTtsUrl(template: string, text: string, voice: string): string {
  const clipped = text.length > HTTP_TTS_TEXT_LIMIT ? text.slice(0, HTTP_TTS_TEXT_LIMIT) : text;
  let url = template.includes("{text}")
    ? template.replaceAll("{text}", encodeURIComponent(clipped))
    : appendQuery(template, `text=${encodeURIComponent(clipped)}`);
  if (url.includes("{voice}")) {
    url = url.replaceAll("{voice}", encodeURIComponent(voice));
  } else if (voice !== "") {
    url = appendQuery(url, `voice=${encodeURIComponent(voice)}`);
  }
  return url;
}

/** 拉取一段合成音频 → objectURL (调用方负责 revokeObjectURL); 非 2xx / 空响应抛错 */
export async function fetchTtsAudioUrl(
  template: string,
  text: string,
  voice: string,
): Promise<string> {
  const response = await fetch(buildTtsUrl(template, text, voice), {
    signal: AbortSignal.timeout(HTTP_TTS_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new Error(`音源返回 HTTP ${String(response.status)}`);
  }
  const blob = await response.blob();
  if (blob.size === 0) {
    throw new Error("音源返回空音频");
  }
  return URL.createObjectURL(blob);
}

/** 音源连通性测试: 短文本 GET, 200 即视为可用; 失败抛可读文案 (调用方转 toast) */
export async function testHttpTts(template: string, voice: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(buildTtsUrl(template, "你好", voice), {
      signal: AbortSignal.timeout(HTTP_TTS_TIMEOUT_MS),
    });
  } catch {
    throw new Error("无法连接音源: 请检查模板地址与合成服务是否启动");
  }
  if (!response.ok) {
    throw new Error(`音源返回 HTTP ${String(response.status)}`);
  }
  // 测试只关心可达性, 音频本体直接丢弃
  await response.body?.cancel();
}

/* ---------------------------------- 多引擎网关 ---------------------------------- */

/** 网关元数据请求超时 (ms): 冷启动后首个 /engines 要实测自建引擎实时率 (rtf 高时可达数分钟),
 *  网关真宕机时连接拒绝会立刻失败, 长超时只作用于「活着但在基准测试」的网关 */
export const GATEWAY_STATUS_TIMEOUT_MS = 600_000;

/** 网关合成请求超时 (ms): 自建 CPU 引擎实测实时率可能远大于 1 (rtf>1), 长段合成要留足余量 */
export const GATEWAY_TTS_TIMEOUT_MS = 180_000;

/** 能力清单/音色清单的缓存时长 (ms) */
const GATEWAY_QUERY_STALE_MS = 5 * 60_000;

/** 能力清单查询 key (固定 key; 网关地址变化时整体作废重拉) */
export const TTS_GATEWAY_STATUS_QUERY_KEY = ["ttsGatewayStatus"] as const;

/** 网关配置 (掩码视图) 查询 key: 固定 key, 保存成功与网关地址变化时整体作废 */
export const TTS_GATEWAY_CONFIG_QUERY_KEY = ["ttsGatewayConfig"] as const;

/** 音色清单查询 key: 按引擎分别缓存 */
export function ttsGatewayVoicesQueryKey(engine: string): readonly ["ttsGatewayVoices", string] {
  return ["ttsGatewayVoices", engine];
}

/** 各引擎默认音色 (与网关 DEFAULT_VOICE 同口径): ttsHttpVoice 为空串时使用 */
export const DEFAULT_GATEWAY_VOICE: Record<string, string> = {
  kokoro: "zm_yunxi",
  cosyvoice: "中文男",
  edge: "zh-CN-YunxiNeural",
  tencent: "101050",
};

/** 查询未就绪时消费方的稳定空列表兜底 */
export const EMPTY_GATEWAY_VOICES: GatewayVoice[] = [];

/** /engines 返回的引擎类别: 自建 / 免费云 / 密钥云 */
export const gatewayEngineKindSchema = z.enum(["self-hosted", "cloud-free", "cloud-keyed"]);

/** /engines 随引擎下发的前端可配置字段 (v3): key 形如 'tencent.secret_id' / 'endpoints.kokoro_url' /
 *  'generic_template'; secret=true 时 value 为掩码串 (如 AKID****abcd), 明文字段 value 即当前值, 空串=未配置 */
export const gatewayConfigFieldSchema = z.object({
  key: z.string(),
  label: z.string(),
  secret: z.boolean(),
  value: z.string(),
});

export const gatewayEngineSchema = z.object({
  id: z.string(),
  name: z.string(),
  kind: gatewayEngineKindSchema,
  available: z.boolean(),
  /** 不可用原因 (可用时为空串) */
  reason: z.string().default(""),
  gpu: z.boolean().optional(),
  /** 部署/配置说明 (不可用行的提示与配置弹窗的描述文案) */
  setup: z.string().default(""),
  /** 实测实时率 = 合成耗时/音频时长, >1 比实时慢 (仅 kokoro 自建 ONNX 就绪时有) */
  rtf: z.number().optional(),
  /** 前端可配置字段 (v3): 空数组 = 该引擎无需配置 (如 edge) */
  config_fields: z.array(gatewayConfigFieldSchema).default([]),
});

export const gatewayStatusSchema = z.object({
  /** 网关主机是否有 GPU */
  gpu: z.boolean(),
  /** auto 语义解析出的引擎 id: 自建可用且实测够快 → 自建, 否则 edge → 密钥云 */
  auto: z.string(),
  engines: z.array(gatewayEngineSchema),
});

export const gatewayVoiceSchema = z.object({
  id: z.string(),
  name: z.string(),
  gender: z.string().optional(),
});

/** GET /config 掩码视图: sections 为三个密钥云 (tencent/aliyun/volc, secret 字段掩码), endpoints/generic_template 明文 */
export const gatewayConfigSchema = z.object({
  sections: z.record(z.string(), z.record(z.string(), z.string())),
  endpoints: z.record(z.string(), z.string()),
  generic_template: z.string(),
});

/** POST /config 增量 patch: 顶层 key → 字符串 (generic_template) 或 分节 → { field: value }; 空串=清除, 未提及字段不变 */
export type GatewayConfigPatch = Record<string, Record<string, string> | string>;

/** POST /test 结果: ok=true 带真实合成耗时 (ms); ok=false 带引擎错误原文 (网关失败时也返回该 JSON) */
export const gatewayTestResultSchema = z.discriminatedUnion("ok", [
  z.object({ ok: z.literal(true), ms: z.number() }),
  z.object({ ok: z.literal(false), error: z.string() }),
]);

export type GatewayEngineKind = z.infer<typeof gatewayEngineKindSchema>;
export type GatewayEngine = z.infer<typeof gatewayEngineSchema>;
export type GatewayStatus = z.infer<typeof gatewayStatusSchema>;
export type GatewayVoice = z.infer<typeof gatewayVoiceSchema>;
export type GatewayConfigField = z.infer<typeof gatewayConfigFieldSchema>;
export type GatewayConfig = z.infer<typeof gatewayConfigSchema>;
export type GatewayTestResult = z.infer<typeof gatewayTestResultSchema>;

/** 网关地址去掉尾斜杠; 空值 = 同源代理路径 (caddy /tts-gateway/* → 网关 9912),
 * 跨设备与 https 混合内容都不踩坑 */
function normalizeGatewayBase(base: string): string {
  const trimmed = base.trim().replace(/\/+$/, "");
  return trimmed === "" ? `${window.location.origin}/tts-gateway` : trimmed;
}

/** 请求信号: 超时兜底与调用方 (react-query 卸载/取消) 信号合并 */
function gatewaySignal(timeoutMs: number, signal?: AbortSignal): AbortSignal {
  const timeout = AbortSignal.timeout(timeoutMs);
  return signal === undefined ? timeout : AbortSignal.any([timeout, signal]);
}

/**
 * 网关合成 URL: /tts?text=&voice=&engine= (engine 可为 auto, 网关服务端自行解析).
 * {text} 截断 500 字后由 searchParams 编码; voice 空串不携带, 网关会用引擎默认音色.
 */
export function buildGatewayTtsUrl(base: string, text: string, voice: string, engine: string): string {
  const clipped = text.length > HTTP_TTS_TEXT_LIMIT ? text.slice(0, HTTP_TTS_TEXT_LIMIT) : text;
  const url = new URL(`${normalizeGatewayBase(base)}/tts`);
  url.searchParams.set("text", clipped);
  if (voice !== "") {
    url.searchParams.set("voice", voice);
  }
  url.searchParams.set("engine", engine);
  return url.toString();
}

/** 拉取网关能力清单; 不可达/非 2xx/格式错误都抛错 (查询层 retry:false 静默降级) */
export async function fetchGatewayStatus(base: string, signal?: AbortSignal): Promise<GatewayStatus> {
  const response = await fetch(`${normalizeGatewayBase(base)}/engines`, {
    signal: gatewaySignal(GATEWAY_STATUS_TIMEOUT_MS, signal),
  });
  if (!response.ok) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  return parseWith(gatewayStatusSchema, await response.json());
}
export async function fetchGatewayVoices(
  base: string,
  engine: string,
  signal?: AbortSignal,
): Promise<GatewayVoice[]> {
  const url = new URL(`${normalizeGatewayBase(base)}/voices`);
  url.searchParams.set("engine", engine);
  const response = await fetch(url, { signal: gatewaySignal(GATEWAY_STATUS_TIMEOUT_MS, signal) });
  if (!response.ok) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  return parseWith(z.array(gatewayVoiceSchema), await response.json());
}

/** 网关合成一段音频 → objectURL (调用方负责 revokeObjectURL); 非 2xx/空响应抛错 */
export async function fetchGatewayTtsAudioUrl(
  base: string,
  text: string,
  voice: string,
  engine: string,
): Promise<string> {
  const response = await fetch(buildGatewayTtsUrl(base, text, voice, engine), {
    signal: AbortSignal.timeout(GATEWAY_TTS_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  const blob = await response.blob();
  if (blob.size === 0) {
    throw new Error("网关返回空音频");
  }
  return URL.createObjectURL(blob);
}

/** 引擎连通性测试: 短文本合成, 200 即可用; 失败抛可读文案 (调用方转 toast) */
export async function testGatewayEngine(base: string, engine: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(buildGatewayTtsUrl(base, "你好", DEFAULT_GATEWAY_VOICE[engine] ?? "", engine), {
      signal: AbortSignal.timeout(GATEWAY_TTS_TIMEOUT_MS),
    });
  } catch {
    throw new Error("无法连接网关: 请检查网关地址与网关是否已启动");
  }
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(
      `引擎返回 HTTP ${String(response.status)}${detail === "" ? "" : ` (${detail.slice(0, 120)})`}`,
    );
  }
  // 测试只关心可达性, 音频本体直接丢弃
  await response.body?.cancel();
}

/**
 * 网关能力清单查询 (key ['ttsGatewayStatus']): 网关地址变化时把清单、配置与全部音色缓存一起作废重拉;
 * 网关不可达静默失败 (retry:false, 不弹 toast), UI 退化为只显示 自动/模板/系统音.
 */
export function useTtsGatewayStatus(base: string) {
  const queryClient = useQueryClient();
  const previousBase = useRef(base);
  useEffect(() => {
    if (previousBase.current === base) {
      return;
    }
    previousBase.current = base;
    void queryClient.invalidateQueries({ queryKey: TTS_GATEWAY_STATUS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ["ttsGatewayVoices"] });
    void queryClient.invalidateQueries({ queryKey: TTS_GATEWAY_CONFIG_QUERY_KEY });
  }, [base, queryClient]);
  return useQuery({
    queryKey: TTS_GATEWAY_STATUS_QUERY_KEY,
    queryFn: ({ signal }) => fetchGatewayStatus(base, signal),
    staleTime: GATEWAY_QUERY_STALE_MS,
    retry: false,
  });
}

/** 引擎音色清单查询 (key ['ttsGatewayVoices', engine]): engine 空串 (auto 未解析/非网关音源) 不发请求 */
export function useTtsGatewayVoices(base: string, engine: string) {
  return useQuery({
    queryKey: ttsGatewayVoicesQueryKey(engine),
    queryFn: ({ signal }) => fetchGatewayVoices(base, engine, signal),
    staleTime: GATEWAY_QUERY_STALE_MS,
    retry: false,
    enabled: engine !== "",
  });
}

/* -------------------------- 网关运行时配置 (v3: /config · /test) -------------------------- */

/** 拉取网关配置掩码视图; 不可达/非 2xx/格式错误都抛错 (查询层 retry:false 静默降级) */
export async function fetchGatewayConfig(
  base: string,
  signal?: AbortSignal,
): Promise<GatewayConfig> {
  const response = await fetch(`${normalizeGatewayBase(base)}/config`, {
    signal: gatewaySignal(GATEWAY_STATUS_TIMEOUT_MS, signal),
  });
  if (!response.ok) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  return parseWith(gatewayConfigSchema, await response.json());
}

/** config_fields key → POST /config patch: 'section.field' 归入分节, 无点号 (generic_template) 放顶层 */
export function toGatewayConfigPatch(changes: Record<string, string>): GatewayConfigPatch {
  const patch: GatewayConfigPatch = {};
  for (const [key, value] of Object.entries(changes)) {
    const dot = key.indexOf(".");
    if (dot === -1) {
      patch[key] = value;
      continue;
    }
    const section = key.slice(0, dot);
    const fields = patch[section];
    patch[section] = {
      ...(typeof fields === "object" ? fields : {}),
      [key.slice(dot + 1)]: value,
    };
  }
  return patch;
}

/** 增量保存配置 (只带改动字段, 空串=清除); 网关返回最新掩码视图 */
export async function updateGatewayConfig(
  base: string,
  patch: GatewayConfigPatch,
): Promise<GatewayConfig> {
  const response = await fetch(`${normalizeGatewayBase(base)}/config`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
    signal: AbortSignal.timeout(GATEWAY_STATUS_TIMEOUT_MS),
  });
  if (!response.ok) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  return parseWith(gatewayConfigSchema, await response.json());
}

/** 网关侧真实合成验证 (POST /test, voice 缺省用引擎默认音色): 网络失败抛错;
 *  引擎失败不是异常 —— 返回 {ok:false, error} 原文, 由调用方决定展示 */
export async function verifyGatewayEngine(
  base: string,
  engine: string,
  voice?: string,
): Promise<GatewayTestResult> {
  let response: Response;
  try {
    response = await fetch(`${normalizeGatewayBase(base)}/test`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(voice === undefined || voice === "" ? { engine } : { engine, voice }),
      signal: AbortSignal.timeout(GATEWAY_TTS_TIMEOUT_MS),
    });
  } catch {
    throw new Error("无法连接网关: 请检查网关地址与网关是否已启动");
  }
  const result = gatewayTestResultSchema.safeParse(await response.json().catch(() => null));
  if (!result.success) {
    throw new Error(`网关返回 HTTP ${String(response.status)}`);
  }
  return result.data;
}

/** 网关配置查询 (key ['ttsGatewayConfig']): 配置弹窗据此回显当前值/掩码占位 */
export function useGatewayConfig(base: string) {
  return useQuery({
    queryKey: TTS_GATEWAY_CONFIG_QUERY_KEY,
    queryFn: ({ signal }) => fetchGatewayConfig(base, signal),
    staleTime: GATEWAY_QUERY_STALE_MS,
    retry: false,
  });
}

/** 保存配置 mutation: 成功后作废能力清单与配置缓存 (可用圆点与掩码回显即时刷新) */
export function useUpdateGatewayConfig(base: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (patch: GatewayConfigPatch) => updateGatewayConfig(base, patch),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: TTS_GATEWAY_STATUS_QUERY_KEY });
      void queryClient.invalidateQueries({ queryKey: TTS_GATEWAY_CONFIG_QUERY_KEY });
    },
  });
}

/** 真实合成验证 mutation (配置弹窗「验证」按钮): {ok:false} 也走 onSuccess, 错误原文由调用方转 toast */
export function useTestGatewayEngine(base: string) {
  return useMutation({
    mutationFn: (engine: string) => verifyGatewayEngine(base, engine),
  });
}

/** warp /getHttpTTSList 返回项: url 为主键 (id 同值); Option 字段经 stripNullsDeep 归一 */
export const httpTtsSchema = z.object({
  id: z.string().optional(),
  url: z.string(),
  name: z.string(),
  /** 0=在线合成 / 1=本地引擎 */
  type: z.number().optional(),
  contentType: z.string().optional(),
  concurrentRate: z.string().optional(),
});

export const httpTtsListSchema = z.array(httpTtsSchema);

export type HttpTts = z.infer<typeof httpTtsSchema>;

/** /saveHttpTTS body (url 主键, name 必填, 其余可选) */
export interface HttpTtsInput {
  url: string;
  name: string;
  type?: number;
  contentType?: string;
  concurrentRate?: string;
}

/** 已存听书源列表 (用户命名空间, 无数据回退 default) */
export async function getHttpTTSList(config?: ApiRequestConfig): Promise<HttpTts[]> {
  const data = await get<unknown>("/getHttpTTSList", undefined, config);
  return parseWith(httpTtsListSchema, data);
}

/** 保存听书源 (按 url 主键覆盖) */
export async function saveHttpTTS(tts: HttpTtsInput, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/saveHttpTTS", tts, config);
}

/** 删除听书源 (body: {url}; 后端也认 id, url 即 id) */
export async function deleteHttpTTS(url: string, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/deleteHttpTTS", { url }, config);
}
