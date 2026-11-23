import type { ApiEnvelope } from "./types";

export const API_PREFIX = "/reader3";
export const AUTH_TOKEN_KEY = "mojuan.token.v2";
export const UNAUTHORIZED_EVENT = "mojuan:unauthorized";

export class ApiError extends Error {
  readonly status: number;
  readonly isNeedLogin: boolean;

  constructor(message: string, status = 200, isNeedLogin = false) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.isNeedLogin = isNeedLogin;
  }
}

export function getToken(): string {
  try {
    return localStorage.getItem(AUTH_TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setToken(token: string | null): void {
  try {
    if (!token) {
      localStorage.removeItem(AUTH_TOKEN_KEY);
    } else {
      localStorage.setItem(AUTH_TOKEN_KEY, token);
    }
  } catch {
    // ignore storage quota / privacy mode
  }
}

export function emitUnauthorized(): void {
  window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
}

export function buildUrl(
  path: string,
  params?: Record<string, string | number | boolean | null | undefined>,
  withAuthQuery = false,
): string {
  const fullPath = path.startsWith("/") ? path : `${API_PREFIX}/${path}`;
  const url = new URL(fullPath, window.location.origin);
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== null && v !== "") {
        url.searchParams.set(k, String(v));
      }
    }
  }
  if (withAuthQuery) {
    const token = getToken();
    if (token && !url.searchParams.has("accessToken")) {
      url.searchParams.set("accessToken", token);
    }
  }
  return `${url.pathname}${url.search}`;
}

export interface RequestOptions {
  params?: Record<string, string | number | boolean | null | undefined>;
  signal?: AbortSignal;
  timeoutMs?: number;
  /** 跳过自动派发未登录事件（例如启动探测 getUserInfo 时） */
  silentAuth?: boolean;
}

async function rawRequest<T>(
  method: "GET" | "POST",
  path: string,
  body?: unknown,
  options?: RequestOptions,
): Promise<T> {
  const url = buildUrl(path, options?.params);
  const headers: Record<string, string> = {};
  const token = getToken();
  if (token) {
    headers["accessToken"] = token;
  }

  let fetchBody: BodyInit | undefined;
  if (body instanceof FormData) {
    fetchBody = body;
  } else if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    fetchBody = JSON.stringify(body);
  }

  const controller = new AbortController();
  const timeoutMs = options?.timeoutMs ?? 60_000;
  const timer = timeoutMs > 0 ? window.setTimeout(() => controller.abort(), timeoutMs) : null;

  const onExternalAbort = () => controller.abort();
  if (options?.signal) {
    if (options.signal.aborted) {
      controller.abort();
    } else {
      options.signal.addEventListener("abort", onExternalAbort, { once: true });
    }
  }

  try {
    const resp = await fetch(url, {
      method,
      headers,
      body: fetchBody,
      credentials: "include",
      signal: controller.signal,
    });

    if (resp.status === 401) {
      if (!options?.silentAuth) emitUnauthorized();
      throw new ApiError("请登录后使用", 401, true);
    }

    const contentType = resp.headers.get("content-type") ?? "";
    if (!contentType.includes("application/json")) {
      if (!resp.ok) {
        throw new ApiError(`请求失败 (${resp.status})`, resp.status);
      }
      return (await resp.text()) as unknown as T;
    }

    const envelope = (await resp.json()) as ApiEnvelope<T>;
    if (!envelope.isSuccess) {
      const needLogin = envelope.data === "NEED_LOGIN";
      if (needLogin && !options?.silentAuth) {
        emitUnauthorized();
      }
      throw new ApiError(envelope.errorMsg || "操作失败", resp.status, needLogin);
    }
    return envelope.data;
  } catch (err) {
    if (err instanceof ApiError) throw err;
    if (err instanceof DOMException && err.name === "AbortError") {
      if (options?.signal?.aborted) {
        throw new ApiError("请求已取消", 0);
      }
      throw new ApiError("请求超时，请检查网络连接或稍后重试", 408);
    }
    throw new ApiError(err instanceof Error ? err.message : "网络连接失败", 0);
  } finally {
    if (timer !== null) window.clearTimeout(timer);
    options?.signal?.removeEventListener("abort", onExternalAbort);
  }
}

export function apiGet<T>(
  path: string,
  params?: Record<string, string | number | boolean | null | undefined>,
  options?: Omit<RequestOptions, "params">,
): Promise<T> {
  return rawRequest<T>("GET", path, undefined, { ...options, params });
}

export function apiPost<T>(
  path: string,
  body?: unknown,
  options?: RequestOptions,
): Promise<T> {
  return rawRequest<T>("POST", path, body, options);
}

/**
 * 通用 SSE 流式订阅（支持 POST JSON Body + Header 鉴权，不把 token 暴露在 URL，
 * 同时兼容无名 data 帧与具名 event: end / event: error 帧）
 */
export interface SseCallbacks<TData = unknown, TEnd = unknown> {
  onData?: (payload: TData) => void;
  onEnd?: (payload: TEnd) => void;
  onError?: (err: ApiError) => void;
  onClose?: () => void;
}

export function openSseStream<TData = unknown, TEnd = unknown>(
  path: string,
  body: Record<string, unknown> | undefined,
  callbacks: SseCallbacks<TData, TEnd>,
  query?: Record<string, string | number | boolean | null | undefined>,
): () => void {
  const controller = new AbortController();
  const url = buildUrl(path, query);
  const headers: Record<string, string> = {
    Accept: "text/event-stream",
  };
  const token = getToken();
  if (token) headers["accessToken"] = token;
  if (body !== undefined) headers["Content-Type"] = "application/json";

  let closed = false;
  const finish = () => {
    if (!closed) {
      closed = true;
      callbacks.onClose?.();
    }
  };

  void (async () => {
    try {
      const resp = await fetch(url, {
        method: body !== undefined ? "POST" : "GET",
        headers,
        body: body !== undefined ? JSON.stringify(body) : undefined,
        credentials: "include",
        signal: controller.signal,
      });

      if (resp.status === 401) {
        emitUnauthorized();
        callbacks.onError?.(new ApiError("请登录后使用", 401, true));
        finish();
        return;
      }
      if (!resp.ok || !resp.body) {
        callbacks.onError?.(new ApiError(`流式连接失败 (${resp.status})`, resp.status));
        finish();
        return;
      }

      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";

      const dispatchFrame = (rawFrame: string) => {
        if (!rawFrame.trim()) return;
        let eventName = "message";
        const dataLines: string[] = [];
        for (const rawLine of rawFrame.split(/\r?\n/)) {
          if (rawLine.startsWith(":")) continue; // heartbeat keep-alive
          if (rawLine.startsWith("event:")) {
            eventName = rawLine.slice(6).trim();
          } else if (rawLine.startsWith("data:")) {
            dataLines.push(rawLine.slice(5).trimStart());
          }
        }
        if (dataLines.length === 0) return;
        const dataStr = dataLines.join("\n");
        let parsed: unknown = dataStr;
        try {
          parsed = JSON.parse(dataStr);
        } catch {
          // keep raw string
        }

        if (eventName === "error") {
          const obj = parsed as Partial<ApiEnvelope<unknown>>;
          const needLogin = obj?.data === "NEED_LOGIN";
          if (needLogin) emitUnauthorized();
          callbacks.onError?.(new ApiError(obj?.errorMsg || "流式请求失败", 200, needLogin));
        } else if (eventName === "end") {
          callbacks.onEnd?.(parsed as TEnd);
        } else {
          callbacks.onData?.(parsed as TData);
        }
      };

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let idx = buffer.search(/\r?\n\r?\n/);
        while (idx !== -1) {
          const frame = buffer.slice(0, idx);
          const sepLen = buffer.slice(idx).startsWith("\r\n\r\n") ? 4 : 2;
          buffer = buffer.slice(idx + sepLen);
          dispatchFrame(frame);
          idx = buffer.search(/\r?\n\r?\n/);
        }
      }
      if (buffer.trim()) dispatchFrame(buffer);
      finish();
    } catch (err) {
      if (controller.signal.aborted) {
        finish();
        return;
      }
      callbacks.onError?.(
        err instanceof ApiError ? err : new ApiError(err instanceof Error ? err.message : "流式连接中断", 0),
      );
      finish();
    }
  })();

  return () => {
    controller.abort();
  };
}

/** 构造封面/正文图片代理 URL（本地 /assets 路径直接返回，相对外链结合 referer 补全后经防盗链代理） */
export function resolveImageUrl(raw?: string | null, referer?: string): string | undefined {
  if (!raw) return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  if (
    trimmed.startsWith("/assets/") ||
    trimmed.startsWith("/epub/") ||
    trimmed.startsWith("data:") ||
    trimmed.startsWith("blob:")
  ) {
    return trimmed;
  }
  let resolved = trimmed;
  if (trimmed.startsWith("//")) {
    const proto = referer?.startsWith("http:") ? "http:" : "https:";
    resolved = `${proto}${trimmed}`;
  } else if (!/^https?:\/\//i.test(trimmed)) {
    if (referer && /^https?:\/\//i.test(referer)) {
      try {
        resolved = new URL(trimmed, referer).href;
      } catch {
        return undefined;
      }
    } else {
      return undefined;
    }
  }
  if (/^https?:\/\//i.test(resolved)) {
    return buildUrl(
      "/assets/proxy",
      { url: resolved, referer, fmt: "webp", q: 82 },
      true,
    );
  }
  return undefined;
}

export function fontFileUrl(id: string): string {
  return buildUrl("/reader3/getFont", { id }, true);
}

export function fileDownloadUrl(home: string, path: string, stream = 0): string {
  return buildUrl("/reader3/file/download", { home, path, stream: stream || undefined }, true);
}

export function ttsAudioUrl(params: {
  text: string;
  type?: string;
  voice?: string;
  rate?: string;
  pitch?: string;
  volume?: string;
}): string {
  return buildUrl("/reader3/tts", params, true);
}
