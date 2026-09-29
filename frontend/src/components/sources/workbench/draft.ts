import { bookSourceSchema, type BookSource, type BookSourceRule } from "@/types/api";

/**
 * 工作台编辑器草稿模型: 基础字段 + 五个规则 JSON 文本 + 整源 JSON 文本.
 * 「所有测试用编辑器当前 JSON」的关键在这里——assembleDraft 每次都从草稿现场组装
 * 完整源对象, 测试请求把它作为 inline bookSource 发给后端, 不落库也能验证.
 */

export const RULE_KEYS = [
  "ruleBookInfo",
  "ruleToc",
  "ruleContent",
  "ruleSearch",
  "ruleExplore",
] as const;

export type RuleKey = (typeof RULE_KEYS)[number];

export const RULE_LABELS: Record<RuleKey, string> = {
  ruleBookInfo: "详情规则 · ruleBookInfo",
  ruleToc: "目录规则 · ruleToc",
  ruleContent: "正文规则 · ruleContent",
  ruleSearch: "搜索规则 · ruleSearch",
  ruleExplore: "发现规则 · ruleExplore",
};

/** 新建源时各规则框的占位示例: 提示 legado 常用字段名 */
export const RULE_PLACEHOLDERS: Record<RuleKey, string> = {
  ruleBookInfo: '{\n  "init": "",\n  "name": "",\n  "author": "",\n  "kind": "",\n  "intro": "",\n  "coverUrl": "",\n  "tocUrl": ""\n}',
  ruleToc: '{\n  "chapterList": "",\n  "chapterName": "",\n  "chapterUrl": "",\n  "nextTocUrl": ""\n}',
  ruleContent: '{\n  "content": "",\n  "nextContentUrl": "",\n  "replaceRegex": ""\n}',
  ruleSearch: '{\n  "bookList": "",\n  "name": "",\n  "author": "",\n  "kind": "",\n  "intro": "",\n  "coverUrl": "",\n  "bookUrl": ""\n}',
  ruleExplore: '{\n  "bookList": "",\n  "name": "",\n  "author": "",\n  "kind": "",\n  "intro": "",\n  "coverUrl": "",\n  "bookUrl": ""\n}',
};

export type EditorMode = "fields" | "json";

export interface SourceDraft {
  /** 载入的完整源对象兜底: 保留 header/loginUrl/weight 等编辑器未展开的字段 */
  base: BookSource;
  mode: EditorMode;
  name: string;
  url: string;
  group: string;
  enabled: boolean;
  searchUrl: string;
  exploreUrl: string;
  /** 五个规则 textarea 的原始 JSON 文本, 空串 = 该规则缺省 */
  rules: Record<RuleKey, string>;
  /** 整源 JSON 模式的原始文本 */
  jsonText: string;
}

export function emptySourceTemplate(): BookSource {
  return {
    bookSourceUrl: "",
    bookSourceName: "",
    bookSourceGroup: "",
    bookSourceType: 0,
    bookSourceComment: "",
    enabled: true,
    published: false,
    enabledExplore: true,
    customOrder: 0,
    lastUpdateTime: 0,
    respondTime: 180000,
    weight: 0,
  };
}

function ruleToText(rule: BookSourceRule | undefined): string {
  if (rule === undefined) return "";
  const entries = Object.entries(rule).filter(([, value]) => value !== undefined);
  return entries.length > 0 ? JSON.stringify(Object.fromEntries(entries), null, 2) : "";
}

/** 完整源对象 → 草稿(分区模式): 规则对象格式化成缩进 JSON 文本 */
export function draftFromSource(source: BookSource): SourceDraft {
  return {
    base: source,
    mode: "fields",
    name: source.bookSourceName,
    url: source.bookSourceUrl,
    group: source.bookSourceGroup,
    enabled: source.enabled,
    searchUrl: source.searchUrl ?? "",
    exploreUrl: source.exploreUrl ?? "",
    rules: {
      ruleBookInfo: ruleToText(source.ruleBookInfo),
      ruleToc: ruleToText(source.ruleToc),
      ruleContent: ruleToText(source.ruleContent),
      ruleSearch: ruleToText(source.ruleSearch),
      ruleExplore: ruleToText(source.ruleExplore),
    },
    jsonText: JSON.stringify(source, null, 2),
  };
}

export function emptyDraft(): SourceDraft {
  return draftFromSource(emptySourceTemplate());
}

/** 单个规则 JSON 文本校验: 必须是「字符串 → 字符串」对象; 空文本 = 规则缺省(合法) */
export function validateRuleText(text: string): { value: BookSourceRule | null; error: string | null } {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { value: null, error: null };
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (error) {
    return { value: null, error: `JSON 无效 — ${(error as Error).message}` };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { value: null, error: "规则必须是 JSON 对象, 例如 {\"name\": \"...\"}" };
  }
  const value: BookSourceRule = {};
  for (const [key, item] of Object.entries(parsed as Record<string, unknown>)) {
    if (item === undefined || item === null) continue;
    if (typeof item !== "string") {
      return { value: null, error: `字段 "${key}" 的值必须是字符串` };
    }
    value[key] = item;
  }
  return { value, error: null };
}

/** zod issue → 中文可读文案 (整源 JSON 校验用) */
function describeIssue(issue: { path: Array<string | number>; code: string; message: string }): string {
  const where = issue.path.length > 0 ? `字段 "${issue.path.join(".")}" ` : "";
  if (issue.code === "invalid_type") return `${where}类型不对`;
  if (issue.code === "unrecognized_keys") return `含未知字段 ${issue.message}`;
  return `${where}${issue.message}`;
}

/** 整源 JSON 文本 → BookSource (不查地址是否为空; 模式切换与最终组装共用) */
export function parseSourceJson(text: string): { source: BookSource | null; error: string | null } {
  const trimmed = text.trim();
  if (trimmed.length === 0) return { source: null, error: "整源 JSON 为空" };
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (error) {
    return { source: null, error: `整源 JSON 无效 — ${(error as Error).message}` };
  }
  const result = bookSourceSchema.safeParse(parsed);
  if (!result.success) {
    const issue = result.error.issues[0];
    return {
      source: null,
      error: `整源 JSON 无效 — ${issue !== undefined ? describeIssue(issue) : "字段不符合书源结构"}`,
    };
  }
  return { source: result.data, error: null };
}

/** 分区模式: 规则文本逐个校验 + 基础字段覆盖 base, 组装源对象 (不查地址) */
function buildFieldsSource(draft: SourceDraft): {
  source: BookSource;
  ruleErrors: Partial<Record<RuleKey, string>>;
} {
  const ruleErrors: Partial<Record<RuleKey, string>> = {};
  const values: Partial<Record<RuleKey, BookSourceRule | null>> = {};
  for (const key of RULE_KEYS) {
    const validated = validateRuleText(draft.rules[key]);
    if (validated.error !== null) ruleErrors[key] = validated.error;
    values[key] = validated.value;
  }
  const searchUrl = draft.searchUrl.trim();
  const exploreUrl = draft.exploreUrl.trim();
  const source: BookSource = {
    ...draft.base,
    bookSourceUrl: draft.url.trim(),
    bookSourceName: draft.name.trim(),
    bookSourceGroup: draft.group.trim(),
    enabled: draft.enabled,
    searchUrl: searchUrl.length > 0 ? searchUrl : undefined,
    exploreUrl: exploreUrl.length > 0 ? exploreUrl : undefined,
    ruleBookInfo: values.ruleBookInfo ?? undefined,
    ruleToc: values.ruleToc ?? undefined,
    ruleContent: values.ruleContent ?? undefined,
    ruleSearch: values.ruleSearch ?? undefined,
    ruleExplore: values.ruleExplore ?? undefined,
  };
  return { source, ruleErrors };
}

export interface AssembleResult {
  /** 组装好的完整源对象; 校验不过为 null */
  source: BookSource | null;
  /** 分区模式各规则框的 JSON 校验错误 */
  ruleErrors: Partial<Record<RuleKey, string>>;
  /** 分区模式基础字段错误(地址为空) */
  fieldError: string | null;
  /** 整源 JSON 模式错误 */
  jsonError: string | null;
  /** 首条错误摘要: 保存/测试面板统一提示, null = 草稿可用 */
  summary: string | null;
}

const URL_EMPTY = "书源地址 (bookSourceUrl) 不能为空: 测试与保存都需要它";

/** 从草稿现场组装完整源对象(测试与保存共用); 任一校验失败时 source 为 null 并给出人话摘要 */
export function assembleDraft(draft: SourceDraft): AssembleResult {
  if (draft.mode === "json") {
    const { source, error } = parseSourceJson(draft.jsonText);
    if (source === null || error !== null) {
      const message = error ?? "整源 JSON 无效";
      return { source: null, ruleErrors: {}, fieldError: null, jsonError: message, summary: message };
    }
    if (source.bookSourceUrl.trim().length === 0) {
      return { source: null, ruleErrors: {}, fieldError: null, jsonError: URL_EMPTY, summary: URL_EMPTY };
    }
    return { source, ruleErrors: {}, fieldError: null, jsonError: null, summary: null };
  }

  const { source, ruleErrors } = buildFieldsSource(draft);
  const fieldError = draft.url.trim().length === 0 ? URL_EMPTY : null;
  if (fieldError !== null || Object.keys(ruleErrors).length > 0) {
    const firstRule = RULE_KEYS.find((key) => ruleErrors[key] !== undefined);
    const summary =
      fieldError ??
      (firstRule !== undefined ? `${RULE_LABELS[firstRule]}: ${ruleErrors[firstRule]}` : "规则 JSON 有误");
    return { source: null, ruleErrors, fieldError, jsonError: null, summary };
  }
  return { source, ruleErrors: {}, fieldError: null, jsonError: null, summary: null };
}

/** 模式切换 fields → 整源 JSON: 规则 JSON 有错时拒绝序列化(避免坏文本被静默丢弃) */
export function serializeDraft(draft: SourceDraft): { text: string | null; error: string | null } {
  const { source, ruleErrors } = buildFieldsSource(draft);
  const firstRule = RULE_KEYS.find((key) => ruleErrors[key] !== undefined);
  if (firstRule !== undefined) {
    return { text: null, error: `${RULE_LABELS[firstRule]}: ${ruleErrors[firstRule]}` };
  }
  return { text: JSON.stringify(source, null, 2), error: null };
}

/** 格式化 JSON 文本(规则框/整源框共用): 解析失败返回 null, 由调用方提示 */
export function formatJsonText(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed.length === 0) return null;
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return null;
  }
}
