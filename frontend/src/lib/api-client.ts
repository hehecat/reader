import axios, { AxiosError, type AxiosInstance, type AxiosResponse } from "axios";
import { z } from "zod";
import { clearAccessToken, getAccessToken } from "@/lib/storage";
import { NEED_LOGIN } from "@/types/api";

/** 后端接口根路径 (dev 走 vite proxy, 生产同源) */
export const API_BASE_URL = "/reader3";
/** 登录态失效时广播的事件名, 由路由层监听后跳转 /login */
export const UNAUTHORIZED_EVENT = "reader:unauthorized";
export const DEFAULT_TIMEOUT = 30_000;

export class ApiError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "ApiError";
  }
}

/** 登录态缺失/失效 (data === "NEED_LOGIN" 或 HTTP 401) */
export class UnauthorizedError extends ApiError {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "UnauthorizedError";
  }
}

/** 后端标准响应封装 (warp serde 序列化: 缺失的 Option 字段会显式输出 null) */
export interface RawEnvelope {
  isSuccess: boolean;
  errorMsg?: unknown;
  data?: unknown;
}

export function isApiEnvelope(value: unknown): value is RawEnvelope {
  if (typeof value !== "object" || value === null || !("isSuccess" in value)) {
    return false;
  }
  return typeof (value as { isSuccess: unknown }).isSuccess === "boolean";
}

/** 清除登录态并广播 unauthorized 事件 */
export function dispatchUnauthorized(reason: string): void {
  clearAccessToken();
  window.dispatchEvent(
    new CustomEvent<{ reason: string }>(UNAUTHORIZED_EVENT, { detail: { reason } }),
  );
}

/** 提取封装里的 errorMsg, 空值时回退到 fallback */
export function envelopeMessage(body: RawEnvelope, fallback: string): string {
  return typeof body.errorMsg === "string" && body.errorMsg.length > 0
    ? body.errorMsg
    : fallback;
}

/** 把任意异常归一化成 ApiError / UnauthorizedError */
export function toApiError(error: unknown): ApiError {
  if (error instanceof ApiError) {
    return error;
  }
  if (!axios.isAxiosError(error)) {
    return new ApiError(error instanceof Error ? error.message : "未知错误", { cause: error });
  }
  const status = error.response?.status;
  const payload: unknown = error.response?.data;
  if (isApiEnvelope(payload)) {
    const message = envelopeMessage(payload, "请求失败");
    if (payload.data === NEED_LOGIN || status === 401) {
      dispatchUnauthorized(message);
      return new UnauthorizedError(message, { cause: error });
    }
    return new ApiError(message, { cause: error });
  }
  if (status === 401) {
    const message = "请登录后使用";
    dispatchUnauthorized(message);
    return new UnauthorizedError(message, { cause: error });
  }
  if (error.code === AxiosError.ERR_CANCELED) {
    return new ApiError("请求已取消", { cause: error });
  }
  if (error.code === AxiosError.ECONNABORTED || error.code === AxiosError.ETIMEDOUT) {
    return new ApiError("请求超时", { cause: error });
  }
  if (error.response === undefined) {
    return new ApiError("网络连接失败", { cause: error });
  }
  return new ApiError(`请求失败 (HTTP ${String(status)})`, { cause: error });
}

export const apiClient: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  timeout: DEFAULT_TIMEOUT,
  // 后端同时支持 session cookie(reader.session), 跨端口调试时需要携带
  withCredentials: true,
});

apiClient.interceptors.request.use((config) => {
  const token = getAccessToken();
  const params = (config.params ?? {}) as Record<string, unknown>;
  // token 走请求头而非 query: query 会进 access log / Referer / 浏览器历史 / 反代日志.
  // 后端 access_token_of 依次取 query accessToken → 头 accessToken → Authorization: Bearer.
  // 调用方显式传入的 params.accessToken 保持原样, 不覆盖 (query 优先级高于头).
  // 用 headers.set 而非整体赋值, 避免冲掉实例/单次请求的默认 headers.
  if (token !== null && params.accessToken === undefined) {
    config.headers.set("accessToken", token);
  }
  return config;
});

apiClient.interceptors.response.use(
  (response: AxiosResponse): AxiosResponse => {
    const body: unknown = response.data;
    if (!isApiEnvelope(body)) {
      // 非标准封装(纯文本/二进制)原样返回
      return response;
    }
    if (body.isSuccess) {
      // 解包: 调用方拿到的就是 data
      return { ...response, data: body.data };
    }
    const message = envelopeMessage(body, "请求失败");
    if (body.data === NEED_LOGIN) {
      dispatchUnauthorized(message);
      throw new UnauthorizedError(message);
    }
    throw new ApiError(message);
  },
  (error: unknown): never => {
    throw toApiError(error);
  },
);

export interface ApiParams {
  [key: string]: string | number | boolean | undefined;
}

export interface ApiRequestConfig {
  timeout?: number;
  signal?: AbortSignal;
  /** 额外 query 参数(如管理接口的 secureKey); accessToken 由拦截器走请求头, 不在这里 */
  params?: ApiParams;
}

/** GET, 返回解包后的 data (类型断言, 由调用方决定是否过 zod) */
export async function get<T>(
  url: string,
  params?: ApiParams,
  config?: ApiRequestConfig,
): Promise<T> {
  const response = await apiClient.request<unknown>({
    ...config,
    url,
    method: "GET",
    params: { ...params, ...config?.params },
  });
  return response.data as T;
}

/** POST, 返回解包后的 data (类型断言, 由调用方决定是否过 zod) */
export async function post<T>(
  url: string,
  data?: unknown,
  config?: ApiRequestConfig,
): Promise<T> {
  const response = await apiClient.request<unknown>({
    ...config,
    url,
    method: "POST",
    data,
    params: config?.params,
  });
  return response.data as T;
}

/**
 * 递归把显式 null 归一成 undefined (对象里直接丢键, 数组里原位替换):
 * warp 后端 serde 对 Option 字段输出 null, 而 zod 的 .optional()/.default() 只接受
 * undefined, 显式 null 会被拒; 其余原语透传. 所有后端负载进 zod 前都先过这一层.
 */
export function stripNullsDeep(value: unknown): unknown {
  if (value === null) {
    return undefined;
  }
  if (Array.isArray(value)) {
    return value.map(stripNullsDeep);
  }
  if (typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      if (item !== null) {
        out[key] = stripNullsDeep(item);
      }
    }
    return out;
  }
  return value;
}

/** zod 校验 (先经 stripNullsDeep 归一 warp 的显式 null), 失败抛 ApiError("响应格式错误") */
export function parseWith<S extends z.ZodTypeAny>(schema: S, data: unknown): z.output<S> {
  const result = schema.safeParse(stripNullsDeep(data));
  if (!result.success) {
    throw new ApiError("响应格式错误", { cause: result.error });
  }
  return result.data;
}
