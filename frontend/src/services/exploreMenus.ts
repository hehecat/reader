/**
 * `BookSource.exploreUrl` 的两套写法解析(纯函数, 无网络/无 DOM, 单测直接跑).
 *
 * 1. JSON 数组: legado 原生格式, 项里的 `style` 只影响原生端布局, 这里忽略;
 * 2. 文本: 每行 `分类名::发现页地址`.
 *
 * 从 services/explore.ts 抽出来只为可测: 那个文件带 `@/` 别名和网络层, node --test 起不来.
 */
import { z } from "zod";

/** 归一化后的发现菜单项: 分类名 + 发现页地址(可含 `{{page}}` 占位, 由后端替换) */
export const exploreMenuSchema = z.object({
  name: z.string(),
  url: z.string(),
});

export type ExploreMenu = z.infer<typeof exploreMenuSchema>;

/**
 * `BookSource.exploreUrl` 为 JSON 数组时的原始项.
 * legado 书源用 `title`, 少数书源写成 `name`, 两者都接受; `style` 只影响原生端布局, 这里忽略.
 */
const exploreUrlItemSchema = z.object({
  title: z.string().optional(),
  name: z.string().optional(),
  url: z.string(),
});

/** 动态发现地址只能在服务端求值, 客户端不解析(与旧版前端一致) */
const DYNAMIC_EXPLORE_PREFIXES = ["<js>", "@js:"];

/** 去掉对象/数组末尾多余的逗号(`{"a":1,}` 这种最普遍的写坏法), 不做完整 JSON5 */
function stripTrailingCommas(text: string): string {
  return text.replace(/,(\s*[}\]])/g, "$1");
}

/** 解析 JSON 数组形式的 exploreUrl; 不是数组返回 null(交给文本形式处理) */
function parseJsonMenus(raw: string): ExploreMenu[] | null {
  if (!raw.startsWith("[")) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    try {
      parsed = JSON.parse(stripTrailingCommas(raw));
    } catch {
      // 看着是 JSON 却根本解析不了: 当这个源没有分类. 绝不能退回文本解析 ——
      // exploreUrl 的每一行都会变成一个分类按钮, 实测「书客吧」(尾逗号)把 113 行 JSON 铺满书海分类栏.
      return [];
    }
  }
  if (!Array.isArray(parsed)) {
    return null;
  }
  const menus: ExploreMenu[] = [];
  for (const item of parsed) {
    const result = exploreUrlItemSchema.safeParse(item);
    if (!result.success || result.data.url.length === 0) {
      continue;
    }
    const name = result.data.title ?? result.data.name ?? "";
    menus.push({ name: name.length > 0 ? name : result.data.url, url: result.data.url });
  }
  return menus;
}

/** 解析文本形式的 exploreUrl: 每行 `分类名::发现页地址`, 空行只是原生端的分块分隔, 这里拉平 */
function parseTextMenus(raw: string): ExploreMenu[] {
  const menus: ExploreMenu[] = [];
  for (const line of raw.replace(/\r\n/g, "\n").split("\n")) {
    const text = line.trim();
    if (text.length === 0) {
      continue;
    }
    const separator = text.indexOf("::");
    const name = separator >= 0 ? text.slice(0, separator).trim() : "";
    const url = (separator >= 0 ? text.slice(separator + 2) : text).trim();
    if (url.length === 0) {
      continue;
    }
    menus.push({ name: name.length > 0 ? name : url, url });
  }
  return menus;
}

/**
 * 书源的发现菜单列表: 优先按 JSON 数组解析 `exploreUrl`, 否则按 `名称::地址` 文本解析.
 * 无 exploreUrl 或为动态脚本时返回空数组(该书源不进书海).
 */
export function parseExploreMenus(source: { exploreUrl?: string | null }): ExploreMenu[] {
  const raw = source.exploreUrl?.trim() ?? "";
  if (raw.length === 0) {
    return [];
  }
  if (DYNAMIC_EXPLORE_PREFIXES.some((prefix) => raw.startsWith(prefix))) {
    return [];
  }
  return parseJsonMenus(raw) ?? parseTextMenus(raw);
}

