import { z } from "zod";
import { parseWith, post, type ApiRequestConfig } from "@/lib/api-client";
import { searchBookListSchema, type SearchBook } from "@/types/api";

/**
 * SSE(`/searchBookMultiSSE`, `/searchBookSourceSSE`)每批增量的 data 负载:
 * `{ lastIndex, data: SearchBook[] }`; `event: end` 的负载只有 `{ lastIndex }`.
 */
export const searchSSEBatchSchema = z.object({
  lastIndex: z.number(),
  data: searchBookListSchema.default([]),
});

export type SearchSSEBatch = z.infer<typeof searchSSEBatchSchema>;

/** 单书源搜索 */
export async function searchBook(
  key: string,
  sourceUrl: string,
  page = 1,
  config?: ApiRequestConfig,
): Promise<SearchBook[]> {
  const data = await post<unknown>(
    "/searchBook",
    // warp 单源搜索参数名是 bookSource (URL 或完整源 JSON); bookSourceUrl 是 SSE 端点的参数名
    { key, bookSource: sourceUrl, page },
    config,
  );
  return parseWith(searchBookListSchema, data);
}
