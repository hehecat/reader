import { z } from "zod";

import { get, parseWith, type ApiRequestConfig } from "@/lib/api-client";

/** 词典条目: kind = en(英汉) | char(汉字) | word(词语) | idiom(成语) */
export const dictEntrySchema = z.object({
  kind: z.string(),
  word: z.string(),
  phonetic: z.string().optional(),
  pinyin: z.string().optional(),
  body: z.string().optional(),
  source: z.string(),
});

/** status: ok 可查 | building 首次导入中(轮询) | unavailable 未安装词典数据 */
export const dictResultSchema = z.object({
  status: z.enum(["ok", "building", "unavailable"]),
  entries: z.array(dictEntrySchema),
});

export type DictEntry = z.infer<typeof dictEntrySchema>;
export type DictResult = z.infer<typeof dictResultSchema>;

/** 离线词典查询(英汉/汉语); 首次启动导入期返回 building, 由调用方轮询 */
export async function dictLookup(
  word: string,
  config?: ApiRequestConfig,
): Promise<DictResult> {
  const data = await get<unknown>("/dictLookup", { word }, config);
  return parseWith(dictResultSchema, data);
}

/** 条目 kind → 中文标签 */
export const DICT_KIND_LABEL: Record<string, string> = {
  en: "英汉",
  char: "汉字",
  word: "词语",
  idiom: "成语",
};
