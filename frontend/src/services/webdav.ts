import { z } from "zod";

import {
  API_BASE_URL,
  ApiError,
  UnauthorizedError,
  dispatchUnauthorized,
  envelopeMessage,
  get,
  isApiEnvelope,
  parseWith,
  post,
  type ApiParams,
  type ApiRequestConfig,
} from "@/lib/api-client";
import { getAccessToken } from "@/lib/storage";
import { NEED_LOGIN } from "@/types/api";

/**
 * WebDAV 备份服务封装.
 *
 * 后端的 "WebDAV" 不是外部云存储: 就是服务端本地 `storage/data/<用户名>/webdav` 目录,
 * 没有任何 url/账号/密码配置项, 唯一开关是用户的 enable_webdav (login / getUserInfo
 * 返回). 未开启时各接口统一报 "未开启webdav功能".
 *
 * Rust 后端(warp)移除了 legacy WebdavController 的 JSON 文件接口
 * (getWebdavFileList/getWebdavFile/uploadFileToWebdav/deleteWebdavFile*), 文件操作
 * 改走通用文件管理 API `/reader3/file/*`, 用 `home=__WEBDAV__` 定位 webdav 根目录;
 * 鉴权与其他 JSON 接口一致(accessToken), secure 模式由后端校验 enable_webdav.
 * (`/reader3/webdav` 的 WebDAV 协议端点只认 Basic 用户名+登录密码, SPA 不存明文
 * 密码, 故不使用.)
 *
 * 备份文件约定 (backupToWebdav / restoreFromWebdav 保持原 JSON 路由):
 * `backupToWebdav` 以目录里最新一份 `backup<日期>.zip` 为底合并当前数据后写出当日
 * `backup<yyyy-MM-dd>.zip`; 目录里没有任何 backup*.zip 时后端支持空底首备 (直接由
 * 当前数据生成). `restoreFromWebdav` 只接受 .zip, 覆盖书源/书架/分组/RSS订阅/替换规则/书签.
 */

/** `/reader3/file/*` 的 home 参数: 定位 `storage/data/<ns>/webdav` */
const WEBDAV_HOME = "__WEBDAV__";

/** webdav 目录/文件条目 (file/list 与 file/upload 同构返回) */
export const webdavFileSchema = z.object({
  name: z.string(),
  /** 文件字节数; 目录是文件系统块大小, 无业务含义 */
  size: z.number(),
  /** 相对 webdav 根目录的路径(如 "/backup2026-09-04.zip"), 恢复/下载/删除都用它 */
  path: z.string(),
  /** 最后修改时间 (epoch ms) */
  lastModified: z.number(),
  isDirectory: z.boolean(),
});
export const webdavFileListSchema = z.array(webdavFileSchema);
export type WebdavFile = z.infer<typeof webdavFileSchema>;

/** file/deleteMulti 返回的删除计数; 不存在/非法路径被后端静默跳过, 不计入 failed */
const deleteMultiResultSchema = z.object({
  deleted: z.number(),
  failed: z.number(),
});

/** 列出目录内容; 路径不存在/不是目录时后端返回 errorMsg, 由 api-client 抛 ApiError */
export async function getWebdavFileList(
  path = "/",
  config?: ApiRequestConfig,
): Promise<WebdavFile[]> {
  const data = await get<unknown>("/file/list", { home: WEBDAV_HOME, path }, config);
  return parseWith(webdavFileListSchema, data);
}

/** 一键备份: 以目录里已有的最新 backup*.zip 为底, 合并当前数据写出当日备份 */
export async function backupToWebdav(config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/backupToWebdav", undefined, config);
}

/** 从 zip 备份恢复书源/书架/分组/RSS订阅/替换规则/书签(整体覆盖) */
export async function restoreFromWebdav(path: string, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/restoreFromWebdav", { path }, config);
}

/** 删除单个文件/目录(目录递归删除) */
export async function deleteWebdavFile(path: string, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/file/delete", { home: WEBDAV_HOME, path }, config);
}

/**
 * 批量删除(file/deleteMulti): 后端逐个删除并静默跳过不存在的路径, 返回
 * {deleted, failed} 计数; 一个都没删成且有失败项时抛错, 部分成功按成功处理
 * (与 legacy 静默跳过语义一致, 由调用方重拉列表对账).
 */
export async function deleteWebdavFileList(
  paths: string[],
  config?: ApiRequestConfig,
): Promise<void> {
  const data = await post<unknown>("/file/deleteMulti", { home: WEBDAV_HOME, paths }, config);
  const result = parseWith(deleteMultiResultSchema, data);
  if (result.deleted === 0 && result.failed > 0) {
    throw new ApiError("删除文件失败");
  }
}

/**
 * 直连 fetch (multipart 上传 / 文件流下载用): api-client 的 axios 实例只发 JSON.
 * accessToken 与拦截器一致放 query, cookie 一并携带; 与 services/import.ts 同法
 * (该 helper 是模块私有, 此处按需自带).
 */
async function requestRaw(
  url: string,
  init: RequestInit,
  config?: ApiRequestConfig,
): Promise<Response> {
  const query = new URLSearchParams();
  const params: ApiParams = config?.params ?? {};
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) query.set(key, String(value));
  }
  const token = getAccessToken();
  // token 走请求头：URL 上的 token 会漏进 access log / Referer（init 自带 headers 要保留）
  const headers = new Headers(init.headers);
  if (token !== null && !headers.has("accessToken")) {
    headers.set("accessToken", token);
  }

  try {
    return await fetch(`${API_BASE_URL}${url}?${query.toString()}`, {
      credentials: "include",
      ...init,
      headers,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new ApiError("请求已取消", { cause: error });
    }
    throw new ApiError("网络连接失败", { cause: error });
  }
}

/** 按标准 envelope 解包 JSON 响应; NEED_LOGIN/401 走全局登录失效处理 */
async function unwrapEnvelope(response: Response): Promise<unknown> {
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  if (!isApiEnvelope(body)) {
    throw new ApiError(
      response.ok ? "响应格式错误" : `请求失败 (HTTP ${String(response.status)})`,
    );
  }
  if (body.isSuccess) {
    return body.data;
  }
  const message = envelopeMessage(body, "请求失败");
  if (body.data === NEED_LOGIN || response.status === 401) {
    dispatchUnauthorized(message);
    throw new UnauthorizedError(message);
  }
  throw new ApiError(message);
}

/**
 * 上传文件到 webdav 指定目录(默认根目录), 不手动设 Content-Type 由浏览器带 boundary.
 * file/upload 收多个同名 `file` 字段, home/path 作为普通表单字段一并发送; 写入失败
 * 的文件被后端静默跳过, 返回值是成功写入的条目列表, 由调用方按文件名对账.
 */
export async function uploadFilesToWebdav(
  files: File[],
  path = "/",
  config?: ApiRequestConfig,
): Promise<WebdavFile[]> {
  const form = new FormData();
  form.append("home", WEBDAV_HOME);
  form.append("path", path);
  files.forEach((file) => {
    form.append("file", file, file.name);
  });
  const response = await requestRaw(
    "/file/upload",
    { method: "POST", body: form, signal: config?.signal },
    config,
  );
  const data = await unwrapEnvelope(response);
  return parseWith(webdavFileListSchema, data);
}

/**
 * 下载文件(file/download): 成功返回文件流(stream 缺省 0 → Content-Disposition
 * attachment), 鉴权失败/未开启时返回 JSON envelope. 拿到 blob 后用临时 <a> 触发
 * 浏览器下载.
 */
export async function downloadWebdavFile(
  file: WebdavFile,
  config?: ApiRequestConfig,
): Promise<void> {
  const response = await requestRaw(
    "/file/download",
    { method: "GET", signal: config?.signal },
    { ...config, params: { ...config?.params, home: WEBDAV_HOME, path: file.path } },
  );
  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    // 失败分支才会返回 JSON; unwrapEnvelope 负责抛错
    await unwrapEnvelope(response);
    throw new ApiError("下载失败");
  }
  if (!response.ok) {
    throw new ApiError(`下载失败 (HTTP ${String(response.status)})`);
  }
  const blob = await response.blob();
  const objectUrl = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = objectUrl;
  anchor.download = file.name;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(objectUrl);
}
