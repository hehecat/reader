import { z } from "zod";

import {
  API_BASE_URL,
  ApiError,
  UnauthorizedError,
  dispatchUnauthorized,
  envelopeMessage,
  isApiEnvelope,
  parseWith,
  type ApiParams,
  type ApiRequestConfig,
} from "@/lib/api-client";
import { getAccessToken } from "@/lib/storage";
import { NEED_LOGIN } from "@/types/api";

/**
 * 本地书导入 (warp Rust 后端两步流):
 * - `importBookPreview`: multipart 上传书籍文件, 解析并返回预览元数据(书名/作者/
 *   格式/章节数/前 10 章标题). 不入库——后端解析完即删临时文件, 取消无需清理.
 * - `uploadLocalBook`: 同一文件正式上传入库(入架+章节+封面), 返回书籍详情字段.
 *   书名/作者由后端按文件名/内容元数据推导, 用户改名走 `saveBook` 增量补存.
 *
 * legacy 的两步流(preview 落盘 /assets → saveBook 搬移正式目录)与本地书仓接口
 * (importFromLocalPathPreview/getLocalStoreFileList)在 warp 中已移除, 相应函数删除.
 */

/** importBookPreview 返回的预览(后端还附带 book/chapters 兼容字段, 前端不消费) */
export const importPreviewSchema = z.object({
  name: z.string(),
  author: z.string(),
  /** 书籍文件格式(txt/epub/umd/cbz…) */
  format: z.string().optional(),
  /** 解析出的章节总数; 0 表示未解析出目录(uploadLocalBook 会拒绝入库) */
  chapterCount: z.number().optional(),
  /** 前 10 章标题 */
  preview: z.array(z.string()).optional(),
});
export type ImportPreview = z.infer<typeof importPreviewSchema>;

/**
 * uploadLocalBook 入库成功返回的书籍(BookInfo 详情形状; 书架 Book 的其余字段由
 * 后端补默认值, 前端以 invalidate 重拉的书架列表为准, 这里只消费定位/对账字段).
 */
export const uploadedBookSchema = z.object({
  bookUrl: z.string(),
  name: z.string(),
  author: z.string(),
  origin: z.string(),
});
export type UploadedBook = z.infer<typeof uploadedBookSchema>;

/**
 * multipart 直传: api-client 的 axios 实例只发 JSON, 文件上传在这里用 fetch 自封装.
 * accessToken 与 api-client 拦截器一致地放在 query; 响应按同样的 envelope 规则解包/抛错.
 * 不手动设置 Content-Type, 由浏览器带上 multipart boundary.
 */
export async function postMultipart<T>(
  url: string,
  form: FormData,
  config?: ApiRequestConfig,
): Promise<T> {
  const query = new URLSearchParams();
  const params: ApiParams = config?.params ?? {};
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) query.set(key, String(value));
  }
  const token = getAccessToken();
  if (token !== null && !query.has("accessToken")) {
    query.set("accessToken", token);
  }

  let response: Response;
  try {
    response = await fetch(`${API_BASE_URL}${url}?${query.toString()}`, {
      method: "POST",
      body: form,
      // 与 axios withCredentials 对齐, 兼容 session cookie 鉴权
      credentials: "include",
      signal: config?.signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new ApiError("请求已取消", { cause: error });
    }
    throw new ApiError("网络连接失败", { cause: error });
  }

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
    return body.data as T;
  }
  const message = envelopeMessage(body, "请求失败");
  if (body.data === NEED_LOGIN || response.status === 401) {
    dispatchUnauthorized(message);
    throw new UnauthorizedError(message);
  }
  throw new ApiError(message);
}

/**
 * 上传书籍文件并返回导入预览(单个文件, multipart 字段名 `file`).
 * 后端校验扩展名(SUPPORTED_EXTENSIONS: txt/epub/umd/cbz/pdf 等), 解析目录后即删
 * 临时文件——预览不产生任何服务端残留.
 */
export async function importBookPreview(
  file: File,
  config?: ApiRequestConfig,
): Promise<ImportPreview> {
  const form = new FormData();
  form.append("file", file, file.name);
  const data = await postMultipart<unknown>("/importBookPreview", form, config);
  return parseWith(importPreviewSchema, data);
}

/**
 * 上传本地书并正式入库(uploadLocalBook): 解析章节、写入书架(origin=local,
 * bookUrl=local://uuid)、提取封面. 章节解析为空时后端报 "未解析到章节内容".
 */
export async function uploadLocalBook(
  file: File,
  config?: ApiRequestConfig,
): Promise<UploadedBook> {
  const form = new FormData();
  form.append("file", file, file.name);
  const data = await postMultipart<unknown>("/uploadLocalBook", form, config);
  return parseWith(uploadedBookSchema, data);
}
