import { get, post, type ApiRequestConfig } from "@/lib/api-client";
import { getAccessToken } from "@/lib/storage";
import { postMultipart } from "@/services/import";

/** 用户上传的自定义字体（按账号存库） */
export interface CustomFont {
  id: string;
  name: string;
  /** @font-face family 名: ReaderCustom-{id} */
  family: string;
  size: number;
  updatedAt?: number;
}

export const FONTS_QUERY_KEY = ["customFonts"] as const;

/** 当前用户已上传字体列表 */
export async function getFontList(config?: ApiRequestConfig): Promise<CustomFont[]> {
  const data = await get<CustomFont[]>("/getFontList", undefined, config);
  return data ?? [];
}

/** 上传字体文件 (ttf/otf/woff/woff2, ≤40MB) */
/** 走 fetch 直传(与书籍上传同链路): axios 实例带 30s 超时, 慢网大文件会被掐断→后端截断 multipart 报错 */
export async function uploadFont(file: File, config?: ApiRequestConfig): Promise<CustomFont> {
  const form = new FormData();
  form.append("file", file, file.name);
  return await postMultipart<CustomFont>("/uploadFont", form, config);
}

/** 删除已上传字体 */
export async function deleteFont(id: string, config?: ApiRequestConfig): Promise<void> {
  await post<unknown>("/deleteFont", { id }, config);
}

/** 字体字节地址 (@font-face src 用; 鉴权走 query accessToken) */
export function fontUrl(id: string): string {
  const tok = getAccessToken();
  const q = new URLSearchParams({ id });
  if (tok) q.set("accessToken", tok);
  return `/reader3/getFont?${q.toString()}`;
}

/** 按文件名扩展名推 @font-face format() */
export function fontFormat(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "ttf":
      return "truetype";
    case "otf":
      return "opentype";
    case "woff":
      return "woff";
    default:
      return "woff2";
  }
}
