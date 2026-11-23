export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let idx = 0;
  let value = bytes;
  while (value >= 1024 && idx < units.length - 1) {
    value /= 1024;
    idx += 1;
  }
  return `${value >= 100 || idx === 0 ? Math.round(value) : value.toFixed(1)} ${units[idx]}`;
}

export function formatRelativeTime(ms?: number | null): string {
  if (!ms || ms <= 0) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  if (diff < 7 * 86_400_000) return `${Math.floor(diff / 86_400_000)} 天前`;
  const d = new Date(ms);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return y === new Date().getFullYear() ? `${m}-${day}` : `${y}-${m}-${day}`;
}

export function formatUptime(totalSeconds?: number): string {
  if (!totalSeconds || totalSeconds < 0) return "—";
  const d = Math.floor(totalSeconds / 86_400);
  const h = Math.floor((totalSeconds % 86_400) / 3_600);
  const m = Math.floor((totalSeconds % 3_600) / 60);
  if (d > 0) return `${d} 天 ${h} 小时`;
  if (h > 0) return `${h} 小时 ${m} 分`;
  return `${Math.max(1, m)} 分钟`;
}

/** 清洗简介 HTML 实体与标签 */
export function cleanIntro(raw?: string | null): string {
  if (!raw) return "";
  if (/^[.…・·\s]+$/.test(raw)) return "";
  const text = raw
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|li)>/gi, "\n")
    .replace(/<[^>]*>/g, "")
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'")
    .split("\n")
    .map((line) => line.replace(/[ \t\u3000]+/g, " ").trim())
    .filter(Boolean)
    .join("\n");
  return /^[.…・·\s]+$/.test(text) ? "" : text;
}

const STATUS_WORDS = /^(?:已?完结|已?完本|完本|全本|连载中?|连载|太监|断更|停更|休更|上架|新书|推荐|置顶)$/;

export function cleanChapterTitle(raw?: string | null): string {
  const s = (raw ?? "")
    .trim()
    .replace(/^[\[【（(]\s*/, "")
    .replace(/\s*[\]】）)]$/, "")
    .replace(/^(?:状态|进度|连载状态)\s*[:：]\s*/, "")
    .trim();
  return !s || STATUS_WORDS.test(s) ? "" : s;
}

const CN_NUM: Record<string, string> = {
  零: "0", 〇: "0", 一: "1", 二: "2", 两: "2", 三: "3", 四: "4",
  五: "5", 六: "6", 七: "7", 八: "8", 九: "9",
};

function normalizeForTitleCompare(s: string): string {
  return s
    .replace(/^正文\s*/u, "")
    .replace(/<[^>]*>/g, "")
    .replace(/[\s\u3000\p{P}\p{S}]+/gu, "")
    .replace(/[零〇一二两三四五六七八九]/g, (c) => CN_NUM[c] ?? c)
    .toLowerCase();
}

/** 自动剔除书源/TXT 正文首段（或前两段）与章节名重复的冗余标题 */
export function stripDuplicateHeading(paragraphs: string[], chapterTitle?: string | null): string[] {
  if (!chapterTitle || paragraphs.length === 0) return paragraphs;
  const normTitle = normalizeForTitleCompare(chapterTitle);
  if (normTitle.length < 2) return paragraphs;

  let start = 0;
  for (let i = 0; i < Math.min(2, paragraphs.length); i += 1) {
    const p = paragraphs[i] ?? "";
    if (p.length > chapterTitle.length * 2.5 + 16) break;
    const normP = normalizeForTitleCompare(p);
    if (normP === normTitle || (normP.length >= 4 && normTitle.includes(normP))) {
      start = i + 1;
    } else {
      break;
    }
  }
  return start > 0 ? paragraphs.slice(start) : paragraphs;
}

export interface ExploreCategory {
  title: string;
  url: string;
}

const PROMO_TITLE_HINTS = ["加群", "交流群", "书源群", "公众号", "点击导入", "发布页"];
const PROMO_URL_DOMAINS = ["qm.qq.com", "t.me", "mp.weixin.qq.com", "bilibili.com", "yckceo.com"];

function isValidBookCategory(title: string, url: string): boolean {
  if (!title || !url || url === "#") return false;
  const lowerTitle = title.toLowerCase();
  if (PROMO_TITLE_HINTS.some((k) => lowerTitle.includes(k))) return false;
  const lowerUrl = url.toLowerCase();
  if (PROMO_URL_DOMAINS.some((d) => lowerUrl.includes(d))) return false;
  return true;
}

/** 解析 legado 书源 exploreUrl（支持 JSON 数组与 `名称::URL` 行格式，自动剔除分栏标题与外链群推广） */
export function parseExploreUrl(raw?: string | null): ExploreCategory[] {
  if (!raw) return [];
  const trimmed = raw.trim();
  if (!trimmed || trimmed.startsWith("@js:") || trimmed.startsWith("<js>")) return [];

  if (trimmed.startsWith("[")) {
    try {
      const sanitized = trimmed.replace(/,\s*([}\]])/g, "$1");
      const arr = JSON.parse(sanitized) as Array<{ title?: string; name?: string; url?: string }>;
      if (!Array.isArray(arr)) return [];
      return arr
        .map((item) => ({
          title: String(item.title ?? item.name ?? "").trim(),
          url: String(item.url ?? "").trim(),
        }))
        .filter((item) => isValidBookCategory(item.title, item.url));
    } catch {
      return [];
    }
  }

  return trimmed
    .split(/\r?\n|&&/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const idx = line.indexOf("::");
      if (idx === -1) return null;
      const title = line.slice(0, idx).trim();
      const url = line.slice(idx + 2).trim();
      return isValidBookCategory(title, url) ? { title, url } : null;
    })
    .filter((item): item is ExploreCategory => item !== null);
}

/** 分割书源分组字符串（逗号/顿号/分号/空白） */
export function splitSourceGroups(groupStr?: string | null): string[] {
  if (!groupStr) return [];
  return groupStr
    .split(/[,，、;；]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

export function buildReaderUrl(bookUrl: string, index?: number, bookSource?: string): string {
  const params = new URLSearchParams();
  params.set("url", bookUrl);
  if (index !== undefined && index >= 0) params.set("index", String(index));
  if (bookSource) params.set("bookSource", bookSource);
  return `/reader?${params.toString()}`;
}
