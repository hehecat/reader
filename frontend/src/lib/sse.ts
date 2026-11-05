import {
  API_BASE_URL,
  ApiError,
  UnauthorizedError,
  dispatchUnauthorized,
  envelopeMessage,
  isApiEnvelope,
} from "@/lib/api-client";
import { getAccessToken } from "@/lib/storage";
import { NEED_LOGIN } from "@/types/api";

export type SSEParams = Record<string, string | number | boolean | undefined>;

export interface SSEHandlers {
  /** `data:` 行 JSON.parse 后回调 (解析失败时回传原始字符串) */
  onData: (parsed: unknown) => void;
  /** `event: end` 全部完成 */
  onEnd?: (parsed: unknown) => void;
  /** `event: error` 或连接中断 */
  onError?: (err: Error) => void;
  /** 流结束/取消/出错后必定回调一次 */
  onClose?: () => void;
}

/**
 * 拼接 SSE 请求地址: encodeURIComponent 编码, 跳过 undefined,
 * 自动附加 localStorage 里的 accessToken.
 * url 可以是完整路径(`/reader3/searchBookMultiSSE`)、接口名(`searchBookMultiSSE`)
 * 或绝对地址(`https://host/reader3/xxx`); 非绝对地址统一挂到 API_BASE_URL 下, 已带前缀则不重复添加.
 *
 * 注意: 这里的 token 只能走 query —— 原生 EventSource 无法设置请求头(不支持 headers 选项).
 * 代价是 token 会出现在 access log / 浏览器历史里; 若将来换 fetch 流式实现(如 services/ai.ts
 * 那样手动解析 SSE), 应改走请求头 accessToken, 与 api-client 拦截器保持一致.
 */
export function buildSSEUrl(url: string, params: SSEParams = {}): string {
  const query: string[] = [];
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined) {
      continue;
    }
    query.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
  }
  const token = getAccessToken();
  if (token !== null && !query.some((item) => item.startsWith("accessToken="))) {
    query.push(`accessToken=${encodeURIComponent(token)}`);
  }
  let path = url;
  if (!/^https?:\/\//.test(path)) {
    if (!path.startsWith("/")) {
      path = `/${path}`;
    }
    if (path !== API_BASE_URL && !path.startsWith(`${API_BASE_URL}/`)) {
      path = `${API_BASE_URL}${path}`;
    }
  }
  return query.length > 0 ? `${path}?${query.join("&")}` : path;
}

function parseEventData(event: Event): unknown {
  const raw = (event as MessageEvent<unknown>).data;
  if (typeof raw !== "string") {
    return undefined;
  }
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return raw;
  }
}

/**
 * 建立 SSE 连接 (原生 EventSource, withCredentials).
 * @returns 取消函数: 关闭连接并触发 onClose (幂等).
 */
export function connectSSE(
  url: string,
  params: SSEParams,
  handlers: SSEHandlers,
): () => void {
  const source = new EventSource(buildSSEUrl(url, params), { withCredentials: true });
  let finished = false;

  const finish = (): void => {
    if (finished) {
      return;
    }
    finished = true;
    source.close();
    handlers.onClose?.();
  };

  source.addEventListener("message", (event) => {
    if (finished) {
      return;
    }
    handlers.onData(parseEventData(event));
  });

  source.addEventListener("end", (event) => {
    if (finished) {
      return;
    }
    handlers.onEnd?.(parseEventData(event));
    // 服务端已 response.end(), 不主动 close 会触发 EventSource 自动重连
    finish();
  });

  source.addEventListener("error", (event) => {
    if (finished) {
      return;
    }
    const raw = (event as MessageEvent<unknown>).data;
    if (typeof raw === "string") {
      // 服务端 `event: error`, data 为标准封装
      const parsed = parseEventData(event);
      if (isApiEnvelope(parsed)) {
        const message = envelopeMessage(parsed, "请求失败");
        if (parsed.data === NEED_LOGIN) {
          dispatchUnauthorized(message);
          handlers.onError?.(new UnauthorizedError(message));
        } else {
          handlers.onError?.(new ApiError(message));
        }
      } else {
        handlers.onError?.(new ApiError(raw));
      }
      finish();
      return;
    }
    // 原生连接错误: 不做自动重连, 交给调用方决定是否重新发起
    handlers.onError?.(new ApiError("SSE 连接中断"));
    finish();
  });

  return finish;
}
