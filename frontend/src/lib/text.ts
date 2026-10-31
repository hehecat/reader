/**
 * 源站简介常带原始 HTML (内联 style/&nbsp;/<br>): 去标签、解码实体、压缩空白,
 * 保留换行 (展示层 whitespace-pre-line 依赖). 无标记时原样 trim 返回.
 */
export function plainIntro(raw: string): string {
  if (/^[.…・·\s]+$/.test(raw)) {
    return ""; // 源站占位简介 (纯省略号/点号) 视为无简介
  }
  if (!raw.includes("<") && !raw.includes("&")) {
    return raw.trim();
  }
  const withBreaks = raw
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|li|tr)>/gi, "\n");
  const noTags = withBreaks.replace(/<[^>]*>/g, "");
  const decoded = noTags
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#x([0-9a-f]+);/gi, (_m, hex: string) => String.fromCharCode(Number.parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (_m, dec: string) => String.fromCharCode(Number(dec)));
  const cleaned = decoded
    .split("\n")
    .map((line) => line.replace(/[ \t　]+/g, " ").trim())
    .filter((line) => line.length > 0)
    .join("\n")
    .trim();
  // 源站占位简介 (纯省略号/点号) 视为无简介, 让卡片走兜底而非显示「...」
  if (/^[.…・·\s]+$/.test(cleaned)) {
    return "";
  }
  return cleaned;
}

/**
 * 书源常把「已完结 / 连载中」这类**状态词**塞进 latestChapterTitle
 * （实测：松鹤庭沐·言璃版搜「一世之尊」→ latestChapterTitle='已完结'）。
 * 那是源站列表里的状态列，不是章节名；当章节名显示会让人以为「已完结却没有目录」。
 * 返回可显示的章节名，状态词/空白一律回空串（调用方按「没有最新章节」处理）。
 */
const STATUS_WORD =
  /^(?:已?完结|已?完本|完本|全本|连载中?|连载|太监|断更|停更|休更|上架|新书|推荐|置顶)$/;

export function chapterTitleText(raw?: string | null): string {
  // 源站也常写「[已完结]」/「状态：已完结」
  const trimmed = (raw ?? "")
    .trim()
    .replace(/^[\[【（(]\s*/, "")
    .replace(/\s*[\]】）)]$/, "")
    .replace(/^(?:状态|进度|连载状态)\s*[:：]\s*/, "")
    .trim();
  return trimmed.length === 0 || STATUS_WORD.test(trimmed) ? "" : trimmed;
}

/**
 * 源站搜索规则给的「最新章」偶尔是别的东西: 实测 QQ 浏览器源返回 `17|0|OK`
 * (源规则里拼出来的状态串)。中文章节名必然带汉字, 不带汉字的当没有。
 */
export function looksLikeChapterTitle(raw?: string | null): boolean {
  const text = chapterTitleText(raw);
  return text.length > 0 && /\p{Script=Han}/u.test(text);
}

/**
 * 书源搜索命中的 bookUrl 是不是「关键词搜索页」而不是书籍详情页。
 * 实测反例：松鹤庭沐·言璃版把 `.../wechat?keyword=一世之尊&start=0&end=19` 当 bookUrl，
 * 三本不同的书共用一个链接 —— 目录规则喂到这种页只能解出 0 章。
 */
export function looksLikeSearchUrl(bookUrl: string | undefined, name: string): boolean {
  if (!bookUrl) return false;
  const key = name.trim();
  if (key.length === 0) return false;
  const encoded = encodeURIComponent(key);
  const url = bookUrl.toLowerCase();
  return url.includes(encoded.toLowerCase()) || url.includes(key.toLowerCase());
}
