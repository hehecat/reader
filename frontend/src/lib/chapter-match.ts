import type { BookChapter } from "@/types/api";

/**
 * 章节对齐: 各源目录结构不同 —— 有的把「上架感言 / 第一卷 / 番外」混在章节序列里,
 * 有的用「第一章」有的是「第1章」「1.」「1、」, 纯按序号取同位置会错位比对。
 *
 * 对齐优先级:
 *   1. 章号匹配 —— 从目标章标题解析出的编号, 到候选目录里找同编号章节(可跨干扰项);
 *   2. 标题匹配 —— 目标章无编号(楔子/序章/番外名)时按归一化标题匹配;
 *   3. 位置兜底 —— 以上都不中, 取同位置并标记, 由 UI 提示"可能错位".
 */

/** 章节标题的干扰项: 不计入章号, 也不该被当成"对应本章" */
const NOISE = /(上架|感言|公告|通知|请假|作品相关|作者的话|求票|推荐票|月票|打赏|完本|完结感言)/;

/** 中文数字 → 阿拉伯数字(支持 一~九百九十九, 含"两/零") */
export function cnNumberToInt(text: string): number | null {
  const digits: Record<string, number> = {
    零: 0,
    一: 1,
    二: 2,
    两: 2,
    三: 3,
    四: 4,
    五: 5,
    六: 6,
    七: 7,
    八: 8,
    九: 9,
  };
  const units: Record<string, number> = { 十: 10, 百: 100 };
  let total = 0;
  let section = 0;
  let matched = false;
  for (const ch of text) {
    if (ch in digits) {
      section = digits[ch] ?? 0;
      matched = true;
      continue;
    }
    if (ch in units) {
      const unit = units[ch] ?? 0;
      total += (section === 0 ? 1 : section) * unit;
      section = 0;
      matched = true;
      continue;
    }
    return null; // 出现非数字字符: 不视为纯中文数字
  }
  return matched ? total + section : null;
}

/**
 * 解析章节标题里的章号: 支持
 *   `第一章 机心` `第1章 机心` `第 12 话` `1. 标题` `1、标题` `001 标题` `Chapter 12`
 * 无编号(楔子/序章/正文名/卷名) → null。
 */
export function parseChapterNumber(title: string): number | null {
  const text = title
    .replace(/[\uFF01-\uFF5E]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0))
    .replace(/\u3000/g, " ")
    .trim();
  // 第<数字>[章话节節回卷] / 第<中文数字>[章话节節回卷]
  const cn = /第\s*([零一二两三四五六七八九十百]+)\s*[章话节節回]/.exec(text);
  if (cn?.[1]) {
    return cnNumberToInt(cn[1]);
  }
  const arabic = /第\s*(\d{1,5})\s*[章话节節回]/.exec(text);
  if (arabic?.[1]) {
    return Number.parseInt(arabic[1], 10);
  }
  // 裸编号开头: `12. 标题` / `12、标题` / `12 标题` / `12`
  const bare = /^(\d{1,5})\s*[.、,:：)）]?\s*\S?/.exec(text);
  if (bare?.[1]) {
    const n = Number.parseInt(bare[1], 10);
    // 形如 `2023年…` 的年份不算章号
    if (!/^\d{1,5}\s*年/.test(text)) {
      return n;
    }
  }
  const en = /chapter\s*(\d{1,5})/i.exec(text);
  if (en?.[1]) {
    return Number.parseInt(en[1], 10);
  }
  return null;
}

/** 归一化标题: 全角转半角、去空白与常见标点, 用于兜底比较 */
function normalizeTitle(title: string): string {
  return title
    .replace(/[\uFF01-\uFF5E]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0))
    .replace(/[\s·・.、,:：,，!！?？\-—_()（）[\]【】"'"'']/g, "")
    .toLowerCase();
}

export type ChapterMatchKind = "number" | "title" | "index" | "missing";

/** 判别联合: kind=missing 时没有命中章节, 其余必带 chapter(便于调用方收窄) */
export type ChapterMatch =
  | {
      kind: "number" | "title" | "index";
      /** 命中的章节 */
      chapter: BookChapter;
      /** 命中的章号(number 匹配时) */
      number?: number;
    }
  | { kind: "missing"; chapter?: undefined; number?: undefined };

/**
 * 把目标章对齐到候选源目录。
 * @param targetTitle 目标章标题(阅读器当前章)
 * @param targetIndex 目标章在当前源目录中的位置(兜底用)
 * @param chapters 候选源目录
 */
export function matchChapter(
  targetTitle: string,
  targetIndex: number,
  chapters: BookChapter[],
): ChapterMatch {
  if (chapters.length === 0) {
    return { kind: "missing" };
  }
  const wanted = parseChapterNumber(targetTitle);
  if (wanted !== null) {
    // 干扰项(上架感言等)不参与章号匹配
    const hit = chapters.find(
      (c) => !NOISE.test(c.title) && parseChapterNumber(c.title) === wanted,
    );
    if (hit) {
      return { kind: "number", chapter: hit, number: wanted };
    }
  }
  const wantedTitle = normalizeTitle(targetTitle);
  if (wantedTitle.length > 0) {
    const hit = chapters.find((c) => normalizeTitle(c.title) === wantedTitle);
    if (hit) {
      return { kind: "title", chapter: hit };
    }
  }
  const fallback = chapters[targetIndex];
  if (fallback !== undefined) {
    return { kind: "index", chapter: fallback };
  }
  return { kind: "missing" };
}
