import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowDownToLine,
  BookOpen,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  ChevronsDownUp,
  ChevronsUpDown,
  Clock3,
  Code2,
  Compass,
  Copy,
  FileText,
  Filter,
  History,
  ListTree,
  Play,
  Plus,
  RotateCcw,
  Save,
  Search,
  Settings2,
  Sparkles,
  Square as StopIcon,
  Terminal,
  Trash2,
  XCircle,
  Zap,
} from "lucide-react";
import * as React from "react";
import { useSearchParams } from "react-router-dom";
import { booksApi, sourcesApi } from "@/api/endpoints";
import type { BookChapter, BookSource, DebugStep, SearchBook } from "@/api/types";
import { cn } from "@/lib/cn";
import { parseExploreUrl } from "@/lib/format";
import { notify } from "@/stores/toast";
import { Button } from "@/ui/Button";
import { Badge, EmptyView, InlineConfirm, Segmented, Spinner, Switch } from "@/ui/Controls";
import { Input, Textarea } from "@/ui/Input";
import { AnimatePresence, motion, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";
import { Popover } from "@/ui/Popover";

type EditorView = "form" | "json";
type GroupKey = "basic" | "search" | "bookInfo" | "toc" | "content";
type RuleKey = "ruleSearch" | "ruleBookInfo" | "ruleToc" | "ruleContent";
type DebugAction = "search" | "explore" | "toc" | "content";
type Phase = "search" | "explore" | "bookInfo" | "toc" | "content" | "other";
type LogFilter = "all" | "errors" | "hits";

interface RuleFieldSpec {
  key: string;
  label: string;
  placeholder?: string;
  multiline?: boolean;
}

const RULE_SEARCH_FIELDS: readonly RuleFieldSpec[] = [
  { key: "bookList", label: "书籍列表 (bookList)", placeholder: "CSS / XPath / JSONPath 列表选择器" },
  { key: "name", label: "书名 (name)", placeholder: "如：a@text" },
  { key: "author", label: "作者 (author)" },
  { key: "bookUrl", label: "详情链接 (bookUrl)", placeholder: "如：a@href" },
  { key: "coverUrl", label: "封面链接 (coverUrl)" },
  { key: "intro", label: "简介 (intro)" },
  { key: "kind", label: "分类标签 (kind)" },
  { key: "lastChapter", label: "最新章节 (lastChapter)" },
  { key: "wordCount", label: "字数 (wordCount)" },
  { key: "checkKeyWord", label: "校验关键词 (checkKeyWord)", placeholder: "默认测试书名" },
];

const RULE_BOOK_INFO_FIELDS: readonly RuleFieldSpec[] = [
  { key: "init", label: "预处理规则 (init)", multiline: true },
  { key: "name", label: "书名 (name)" },
  { key: "author", label: "作者 (author)" },
  { key: "kind", label: "分类 (kind)" },
  { key: "intro", label: "简介 (intro)", multiline: true },
  { key: "coverUrl", label: "封面链接 (coverUrl)" },
  { key: "tocUrl", label: "目录链接 (tocUrl)" },
  { key: "lastChapter", label: "最新章节 (lastChapter)" },
  { key: "wordCount", label: "字数 (wordCount)" },
];

const RULE_TOC_FIELDS: readonly RuleFieldSpec[] = [
  { key: "preUpdateJs", label: "目录前置 JS (preUpdateJs)", multiline: true },
  { key: "chapterList", label: "章节列表 (chapterList)", placeholder: "如：#list dd a" },
  { key: "chapterName", label: "章节名 (chapterName)", placeholder: "如：text" },
  { key: "chapterUrl", label: "章节链接 (chapterUrl)", placeholder: "如：href" },
  { key: "nextTocUrl", label: "目录翻页链接 (nextTocUrl)" },
  { key: "isVolume", label: "卷名判断 (isVolume)" },
  { key: "updateTime", label: "更新时间 (updateTime)" },
];

const RULE_CONTENT_FIELDS: readonly RuleFieldSpec[] = [
  { key: "content", label: "正文规则 (content)", placeholder: "如：#content@html", multiline: true },
  { key: "nextContentUrl", label: "正文翻页链接 (nextContentUrl)" },
  { key: "replaceRegex", label: "净化替换规则 (replaceRegex)", multiline: true },
  { key: "webJs", label: "注入 JS (webJs)", multiline: true },
  { key: "sourceRegex", label: "资源嗅探正则 (sourceRegex)" },
  { key: "imageStyle", label: "图片样式 (imageStyle)", placeholder: "FULL / TEXT" },
];

interface BasicFieldSpec {
  key: string;
  label: string;
  placeholder?: string;
  rows?: number;
  nullable?: boolean;
  half?: boolean;
}

const BASIC_FIELDS: readonly BasicFieldSpec[] = [
  { key: "bookSourceName", label: "书源名称 *", placeholder: "如：笔趣阁示例", half: true },
  { key: "bookSourceGroup", label: "分组", placeholder: "逗号分隔，如：玄幻, 精选", half: true },
  { key: "bookSourceUrl", label: "书源唯一 URL (bookSourceUrl) *", placeholder: "https://example.com" },
  {
    key: "searchUrl",
    label: "搜索地址模板 (searchUrl)",
    placeholder: "/search.php?q={{key}}&page={{page}}",
    rows: 2,
    nullable: true,
  },
  {
    key: "exploreUrl",
    label: "发现地址规则 (exploreUrl)",
    placeholder: "玄幻::/sort/1_{{page}}.html\n仙侠::/sort/2_{{page}}.html",
    rows: 3,
    nullable: true,
  },
  { key: "bookUrlPattern", label: "详情 URL 正则 (bookUrlPattern)", nullable: true, half: true },
  {
    key: "concurrentRate",
    label: "并发限流 (concurrentRate)",
    placeholder: "如 1/1000",
    nullable: true,
    half: true,
  },
  {
    key: "header",
    label: "自定义请求头 (header JSON)",
    placeholder: '{"User-Agent": "Mozilla/5.0 ..."}',
    rows: 2,
    nullable: true,
  },
  {
    key: "proxyUrl",
    label: "书源级代理 URL (proxyUrl)",
    placeholder: "可选，如 http://127.0.0.1:7890",
    nullable: true,
  },
  { key: "jsLib", label: "共享 JS 库 (jsLib)", rows: 2, nullable: true },
  { key: "bookSourceComment", label: "书源注释 (bookSourceComment)", rows: 2, nullable: true },
];

const BASIC_SWITCHES = [
  { key: "enabled", label: "启用" },
  { key: "enabledExplore", label: "发现" },
  { key: "enabledCookieJar", label: "CookieJar" },
] as const;

interface RuleGroupSpec {
  key: GroupKey;
  label: string;
  shortLabel: string;
  hint: string;
  icon: React.ComponentType<{ className?: string }>;
  ruleKey?: RuleKey;
  fields?: readonly RuleFieldSpec[];
}

const RULE_GROUPS: readonly RuleGroupSpec[] = [
  {
    key: "basic",
    label: "基础信息",
    shortLabel: "基础",
    hint: "名称 · 地址 · 请求头 · 开关",
    icon: Settings2,
  },
  {
    key: "search",
    label: "搜索规则",
    shortLabel: "搜索",
    hint: "ruleSearch",
    icon: Search,
    ruleKey: "ruleSearch",
    fields: RULE_SEARCH_FIELDS,
  },
  {
    key: "bookInfo",
    label: "详情规则",
    shortLabel: "详情",
    hint: "ruleBookInfo",
    icon: BookOpen,
    ruleKey: "ruleBookInfo",
    fields: RULE_BOOK_INFO_FIELDS,
  },
  {
    key: "toc",
    label: "目录规则",
    shortLabel: "目录",
    hint: "ruleToc",
    icon: ListTree,
    ruleKey: "ruleToc",
    fields: RULE_TOC_FIELDS,
  },
  {
    key: "content",
    label: "正文规则",
    shortLabel: "正文",
    hint: "ruleContent",
    icon: FileText,
    ruleKey: "ruleContent",
    fields: RULE_CONTENT_FIELDS,
  },
];

const SEARCH_PRESETS = ["诡秘之主", "凡人修仙传", "剑来", "斗破苍穹"] as const;
const RECENT_KEY_STORAGE = "mojuan.workbench.recentTargets";
const PICKER_LIMIT = 80;

const PHASE_META: Record<Phase, { label: string; dot: string; text: string; bg: string }> = {
  search: { label: "搜索", dot: "bg-accent", text: "text-accent", bg: "bg-accent-soft/60" },
  explore: { label: "发现", dot: "bg-accent", text: "text-accent", bg: "bg-accent-soft/60" },
  bookInfo: { label: "详情", dot: "bg-warn", text: "text-warn", bg: "bg-warn-soft/60" },
  toc: { label: "目录", dot: "bg-success", text: "text-success", bg: "bg-success-soft/60" },
  content: { label: "正文", dot: "bg-ink-secondary", text: "text-ink", bg: "bg-subtle" },
  other: { label: "其他", dot: "bg-line-strong", text: "text-ink-muted", bg: "bg-subtle/60" },
};

const ACTION_PIPELINE: Record<DebugAction, readonly Phase[]> = {
  search: ["search", "bookInfo", "toc", "content"],
  explore: ["explore", "bookInfo", "toc", "content"],
  toc: ["bookInfo", "toc"],
  content: ["content"],
};

const ACTION_PHASE: Record<DebugAction, Phase> = {
  search: "search",
  explore: "explore",
  toc: "toc",
  content: "content",
};

const IS_MAC =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent);
const MOD_KEY = IS_MAC ? "⌘" : "Ctrl+";

function createBlankSource(): BookSource {
  return {
    bookSourceUrl: "",
    bookSourceName: "",
    bookSourceGroup: "",
    bookSourceType: 0,
    customOrder: 0,
    enabled: true,
    enabledExplore: true,
    enabledCookieJar: false,
    lastUpdateTime: Date.now(),
    respondTime: 180000,
    weight: 0,
    searchUrl: "",
    exploreUrl: "",
    bookUrlPattern: "",
    header: "",
    concurrentRate: "",
    proxyUrl: "",
    jsLib: "",
    bookSourceComment: "",
    ruleSearch: {},
    ruleBookInfo: {},
    ruleToc: {},
    ruleContent: {},
  };
}

function errText(err: unknown): string {
  return err instanceof Error ? err.message : "操作失败";
}

function asRuleRecord(val: unknown): Record<string, unknown> {
  return val && typeof val === "object" && !Array.isArray(val)
    ? (val as Record<string, unknown>)
    : {};
}

function asString(val: unknown): string {
  if (val === undefined || val === null) return "";
  return typeof val === "string" ? val : JSON.stringify(val);
}

function stableStringify(val: unknown): string {
  if (val === null || typeof val !== "object") return JSON.stringify(val ?? null);
  if (Array.isArray(val)) return `[${val.map(stableStringify).join(",")}]`;
  const rec = val as Record<string, unknown>;
  return `{${Object.keys(rec)
    .filter((k) => rec[k] !== undefined)
    .sort()
    .map((k) => `${JSON.stringify(k)}:${stableStringify(rec[k])}`)
    .join(",")}}`;
}

function basicValueOf(src: BookSource, key: string): string {
  const v = src[key];
  if (typeof v === "boolean") return v ? "1" : "0";
  return asString(v);
}

function isFieldModified(
  group: RuleGroupSpec,
  key: string,
  draft: BookSource,
  baseline: BookSource,
): boolean {
  if (group.ruleKey) {
    return (
      asString(asRuleRecord(draft[group.ruleKey])[key]) !==
      asString(asRuleRecord(baseline[group.ruleKey])[key])
    );
  }
  if (BASIC_SWITCHES.some((s) => s.key === key)) {
    return Boolean(draft[key]) !== Boolean(baseline[key]);
  }
  return basicValueOf(draft, key) !== basicValueOf(baseline, key);
}

function groupFieldKeys(group: RuleGroupSpec): string[] {
  if (group.fields) return group.fields.map((f) => f.key);
  return [...BASIC_FIELDS.map((f) => f.key), ...BASIC_SWITCHES.map((s) => s.key)];
}

function countModified(group: RuleGroupSpec, draft: BookSource, baseline: BookSource): number {
  let n = groupFieldKeys(group).filter((k) => isFieldModified(group, k, draft, baseline)).length;
  if (group.ruleKey) {
    const known = new Set(groupFieldKeys(group));
    const a = asRuleRecord(draft[group.ruleKey]);
    const b = asRuleRecord(baseline[group.ruleKey]);
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) {
      if (!known.has(k) && stableStringify(a[k]) !== stableStringify(b[k])) n += 1;
    }
  }
  return n;
}

function countFilled(group: RuleGroupSpec, draft: BookSource): { filled: number; total: number } {
  if (group.ruleKey && group.fields) {
    const rec = asRuleRecord(draft[group.ruleKey]);
    return {
      filled: group.fields.filter((f) => asString(rec[f.key]).trim() !== "").length,
      total: group.fields.length,
    };
  }
  return {
    filled: BASIC_FIELDS.filter((f) => basicValueOf(draft, f.key).trim() !== "").length,
    total: BASIC_FIELDS.length,
  };
}

function matchesFilter(q: string, ...parts: string[]): boolean {
  if (!q) return true;
  return parts.some((p) => p.toLowerCase().includes(q));
}

function detectPhase(ruleName: string, fallback: Phase): Phase {
  const n = ruleName.toLowerCase();
  if (/explore|发现/.test(n)) return "explore";
  if (/search|搜索|booklist|书籍列表/.test(n)) return "search";
  if (/bookinfo|book_info|详情|tocurl/.test(n)) return "bookInfo";
  if (/toc|chapterlist|chaptername|chapterurl|目录|章节列表/.test(n)) return "toc";
  if (/content|正文/.test(n)) return "content";
  return fallback;
}

function loadRecentTargets(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENT_KEY_STORAGE);
    const arr: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string").slice(0, 6) : [];
  } catch {
    return [];
  }
}

function saveRecentTargets(list: string[]) {
  try {
    window.localStorage.setItem(RECENT_KEY_STORAGE, JSON.stringify(list.slice(0, 6)));
  } catch {
    /* ignore quota / privacy mode */
  }
}

function fmtMs(ms: number | undefined): string {
  if (ms === undefined) return "";
  if (ms >= 1000) return `${(ms / 1000).toFixed(ms >= 10000 ? 0 : 1)} s`;
  return `${Math.round(ms)} ms`;
}

function shortUrl(url: string, max = 44): string {
  const s = url.replace(/^https?:\/\//, "");
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

interface NormalizedStep {
  id: number;
  ruleName: string;
  phase: Phase;
  at?: number;
  url?: string;
  elapsedMs?: number;
  resultLen?: number;
  error?: string | null;
  detail?: unknown;
}

function normalizeStep(step: DebugStep, id: number, fallback: Phase, at?: number): NormalizedStep {
  const ruleName = step.ruleName ?? step.rule_name ?? `步骤 ${id}`;
  return {
    id,
    ruleName,
    phase: detectPhase(ruleName, fallback),
    at,
    url: step.url,
    elapsedMs: step.elapsedMs ?? step.elapsed_ms,
    resultLen: step.resultLen ?? step.result_len,
    error: step.error,
    detail: step.detail,
  };
}

function buildLogText(
  steps: NormalizedStep[],
  startInfo: string,
  streamError: string | null,
): string {
  const lines: string[] = [];
  if (startInfo) lines.push(`# ${startInfo}`);
  for (const s of steps) {
    const parts = [
      s.at !== undefined ? `[+${(s.at / 1000).toFixed(2)}s]` : "",
      `#${s.id}`,
      `[${PHASE_META[s.phase].label}]`,
      s.ruleName,
      s.elapsedMs !== undefined ? `· ${Math.round(s.elapsedMs)}ms` : "",
      s.resultLen !== undefined ? `· 命中 ${s.resultLen}` : "",
    ].filter(Boolean);
    lines.push(parts.join(" "));
    if (s.url) lines.push(`    URL: ${s.url}`);
    if (s.error) lines.push(`    ERROR: ${s.error}`);
  }
  if (streamError) lines.push(`!! ${streamError}`);
  return lines.join("\n");
}

interface StepGroup {
  phase: Phase;
  key: string;
  steps: NormalizedStep[];
  errorCount: number;
  hitCount: number;
  totalMs: number;
}

function groupSteps(steps: NormalizedStep[]): StepGroup[] {
  const groups: StepGroup[] = [];
  for (const s of steps) {
    const last = groups[groups.length - 1];
    if (last && last.phase === s.phase) {
      last.steps.push(s);
      if (s.error) last.errorCount += 1;
      if (s.resultLen !== undefined && s.resultLen > 0) last.hitCount += 1;
      if (s.elapsedMs !== undefined) last.totalMs += s.elapsedMs;
    } else {
      groups.push({
        phase: s.phase,
        key: `${s.phase}-${s.id}`,
        steps: [s],
        errorCount: s.error ? 1 : 0,
        hitCount: s.resultLen !== undefined && s.resultLen > 0 ? 1 : 0,
        totalMs: s.elapsedMs ?? 0,
      });
    }
  }
  return groups;
}

interface ChainHints {
  bookUrl?: string;
  bookName?: string;
  chapterUrl?: string;
  chapterTitle?: string;
}

async function copyText(text: string, okMsg: string) {
  try {
    await navigator.clipboard.writeText(text);
    notify.success(okMsg);
  } catch {
    notify.error("复制失败：浏览器拒绝访问剪贴板");
  }
}

export default function WorkbenchPage() {
  const qc = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const urlParam = searchParams.get("url") ?? "";
  const reduced = useReducedMotion();

  const sourcesQuery = useQuery({
    queryKey: ["studio", "sources"],
    queryFn: () => sourcesApi.list(),
  });
  const sources = React.useMemo(() => sourcesQuery.data ?? [], [sourcesQuery.data]);

  const initialBlank = React.useMemo(() => createBlankSource(), []);
  const [baseline, setBaseline] = React.useState<BookSource>(initialBlank);
  const [draft, setDraft] = React.useState<BookSource>(initialBlank);
  const [rawJson, setRawJson] = React.useState(() => JSON.stringify(initialBlank, null, 2));
  const [jsonError, setJsonError] = React.useState<string | null>(null);

  const [editorView, setEditorView] = React.useState<EditorView>("form");
  const [fieldFilter, setFieldFilter] = React.useState("");
  const [openGroups, setOpenGroups] = React.useState<Record<GroupKey, boolean>>({
    basic: true,
    search: true,
    bookInfo: false,
    toc: false,
    content: false,
  });

  // Source Picker state
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [pickerQuery, setPickerQuery] = React.useState("");
  const [pickerActiveIdx, setPickerActiveIdx] = React.useState(0);
  const pickerInputRef = React.useRef<HTMLInputElement>(null);
  const loadedUrlRef = React.useRef<string>("");

  // Debug stream state
  const [action, setAction] = React.useState<DebugAction>("search");
  const [targetInput, setTargetInput] = React.useState("诡秘之主");
  const [recentTargets, setRecentTargets] = React.useState<string[]>(() => loadRecentTargets());
  const [running, setRunning] = React.useState(false);
  const [debugStatus, setDebugStatus] = React.useState<"idle" | "running" | "done" | "error">(
    "idle",
  );
  const [startInfo, setStartInfo] = React.useState<string>("");
  const [steps, setSteps] = React.useState<NormalizedStep[]>([]);
  const [resultData, setResultData] = React.useState<unknown>(null);
  const [streamError, setStreamError] = React.useState<string | null>(null);
  const [showRawResult, setShowRawResult] = React.useState(false);
  const [tocQuickLoading, setTocQuickLoading] = React.useState(false);
  const [logFilter, setLogFilter] = React.useState<LogFilter>("all");
  const [autoFollow, setAutoFollow] = React.useState(true);
  const [unseenCount, setUnseenCount] = React.useState(0);
  const [chainHints, setChainHints] = React.useState<ChainHints>({});
  const [runElapsedMs, setRunElapsedMs] = React.useState<number | null>(null);

  const stopStreamRef = React.useRef<(() => void) | null>(null);
  const stepCounterRef = React.useRef(0);
  const runStartRef = React.useRef<number>(0);
  const timelineRef = React.useRef<HTMLDivElement>(null);
  const autoFollowRef = React.useRef(autoFollow);
  const groupRefs = React.useRef<Partial<Record<GroupKey, HTMLDivElement | null>>>({});

  React.useEffect(() => {
    autoFollowRef.current = autoFollow;
  }, [autoFollow]);

  // Exact dirty calculation
  const dirty = React.useMemo(() => {
    if (jsonError) return true;
    return stableStringify(draft) !== stableStringify(baseline);
  }, [draft, baseline, jsonError]);

  const groupModCounts = React.useMemo(() => {
    const map: Record<GroupKey, number> = {
      basic: 0,
      search: 0,
      bookInfo: 0,
      toc: 0,
      content: 0,
    };
    for (const g of RULE_GROUPS) {
      map[g.key] = countModified(g, draft, baseline);
    }
    return map;
  }, [draft, baseline]);

  const totalModifiedCount = React.useMemo(
    () => Object.values(groupModCounts).reduce((a, b) => a + b, 0),
    [groupModCounts],
  );

  // Warn before closing tab with unsaved changes
  React.useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty]);

  const loadSourceIntoDraft = React.useCallback((src: BookSource) => {
    setBaseline(src);
    setDraft(src);
    setRawJson(JSON.stringify(src, null, 2));
    setJsonError(null);
    loadedUrlRef.current = src.bookSourceUrl;
    const checkKw = asString(asRuleRecord(src.ruleSearch).checkKeyWord).trim();
    if (checkKw) {
      setTargetInput(checkKw);
    }
  }, []);

  // Auto-load source from ?url=...
  React.useEffect(() => {
    if (!urlParam || loadedUrlRef.current === urlParam) return;
    const found = sources.find((s) => s.bookSourceUrl === urlParam);
    if (found) {
      loadSourceIntoDraft(found);
      return;
    }
    if (!sourcesQuery.isLoading) {
      void sourcesApi
        .get(urlParam)
        .then((src) => {
          if (src) loadSourceIntoDraft(src);
        })
        .catch(() => undefined);
    }
  }, [urlParam, sources, sourcesQuery.isLoading, loadSourceIntoDraft]);

  React.useEffect(
    () => () => {
      stopStreamRef.current?.();
    },
    [],
  );

  const updateDraft = React.useCallback((updater: (prev: BookSource) => BookSource) => {
    setDraft((prev) => {
      const next = updater(prev);
      setRawJson(JSON.stringify(next, null, 2));
      setJsonError(null);
      return next;
    });
  }, []);

  const updateRuleField = React.useCallback(
    (ruleKey: RuleKey, field: string, value: string) => {
      updateDraft((prev) => {
        const cur = { ...asRuleRecord(prev[ruleKey]) };
        if (value === "") {
          delete cur[field];
        } else {
          cur[field] = value;
        }
        return { ...prev, [ruleKey]: cur };
      });
    },
    [updateDraft],
  );

  const handleRawJsonChange = (text: string) => {
    setRawJson(text);
    try {
      const parsed: unknown = JSON.parse(text);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        setJsonError("根节点必须是 JSON 对象");
        return;
      }
      setJsonError(null);
      setDraft(parsed as BookSource);
    } catch (err) {
      setJsonError(err instanceof Error ? err.message : "JSON 语法错误");
    }
  };

  const revertDraft = () => {
    setDraft(baseline);
    setRawJson(JSON.stringify(baseline, null, 2));
    setJsonError(null);
    notify.info("已还原未保存的修改");
  };

  const resetToBlank = () => {
    const blank = createBlankSource();
    loadSourceIntoDraft(blank);
    setSearchParams({}, { replace: true });
    setSteps([]);
    setResultData(null);
    setStreamError(null);
    setDebugStatus("idle");
    setRunElapsedMs(null);
    setChainHints({});
    setOpenGroups({
      basic: true,
      search: true,
      bookInfo: false,
      toc: false,
      content: false,
    });
  };

  const saveMutation = useMutation({
    mutationFn: async () => {
      if (jsonError) throw new Error(`JSON 存在语法错误：${jsonError}`);
      const cleanUrl = (draft.bookSourceUrl ?? "").trim();
      if (!cleanUrl) throw new Error("请先在基础信息中填写 bookSourceUrl");
      const payload: BookSource = {
        ...draft,
        bookSourceUrl: cleanUrl,
        bookSourceName: (draft.bookSourceName ?? "").trim() || cleanUrl,
        lastUpdateTime: Date.now(),
      };
      await sourcesApi.save(payload);
      return payload;
    },
    onSuccess: (saved) => {
      setBaseline(saved);
      setDraft(saved);
      setRawJson(JSON.stringify(saved, null, 2));
      loadedUrlRef.current = saved.bookSourceUrl;
      setSearchParams({ url: saved.bookSourceUrl }, { replace: true });
      notify.success(`已保存书源「${saved.bookSourceName}」`);
      void qc.invalidateQueries({ queryKey: ["studio", "sources"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  // Cmd+S / Ctrl+S quick save shortcut
  const saveHandlerRef = React.useRef<() => void>(() => {});
  saveHandlerRef.current = () => {
    if (saveMutation.isPending) return;
    if (jsonError) {
      notify.error("JSON 存在语法错误，请修正后再保存");
      return;
    }
    if (!draft.bookSourceUrl?.trim()) {
      notify.error("请先在基础信息中填写 bookSourceUrl");
      setEditorView("form");
      setOpenGroups((prev) => ({ ...prev, basic: true }));
      return;
    }
    saveMutation.mutate();
  };

  React.useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        saveHandlerRef.current();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // Auto-focus picker input when popover opens
  React.useEffect(() => {
    if (pickerOpen) {
      setPickerActiveIdx(0);
      const id = window.setTimeout(() => pickerInputRef.current?.focus(), 30);
      return () => window.clearTimeout(id);
    }
  }, [pickerOpen]);

  const stopDebug = () => {
    stopStreamRef.current?.();
    stopStreamRef.current = null;
    setRunning(false);
    setDebugStatus((prev) => (prev === "running" ? "idle" : prev));
    if (runStartRef.current > 0) {
      setRunElapsedMs(Math.round(performance.now() - runStartRef.current));
    }
  };

  const recordChainHints = React.useCallback((act: DebugAction, data: unknown) => {
    if ((act === "search" || act === "explore") && Array.isArray(data) && data.length > 0) {
      const first = data[0] as SearchBook;
      if (first?.bookUrl) {
        setChainHints((prev) => ({
          ...prev,
          bookUrl: first.bookUrl,
          bookName: first.name || first.bookUrl,
        }));
      }
    } else if (act === "toc" && Array.isArray(data) && data.length > 0) {
      const firstCh = (data as BookChapter[]).find((c) => c.url && !c.isVolume);
      if (firstCh?.url) {
        setChainHints((prev) => ({
          ...prev,
          chapterUrl: firstCh.url,
          chapterTitle: firstCh.title || firstCh.url,
        }));
      }
    }
  }, []);

  const scrollTimelineToBottom = React.useCallback((smooth = false) => {
    const el = timelineRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  }, []);

  // Auto-follow scroll when new steps or results arrive
  React.useEffect(() => {
    if (autoFollow) {
      scrollTimelineToBottom(false);
      setUnseenCount(0);
    }
  }, [steps.length, resultData, streamError, autoFollow, scrollTimelineToBottom]);

  const handleTimelineScroll = () => {
    const el = timelineRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceFromBottom > 40) {
      if (autoFollowRef.current) {
        setAutoFollow(false);
      }
    } else if (distanceFromBottom <= 16) {
      if (!autoFollowRef.current) {
        setAutoFollow(true);
        setUnseenCount(0);
      }
    }
  };

  const runDebug = (overrideAction?: DebugAction, overrideInput?: string) => {
    const act = overrideAction ?? action;
    const input = (overrideInput ?? targetInput).trim();
    if (overrideAction) setAction(overrideAction);
    if (overrideInput !== undefined) setTargetInput(overrideInput);

    if (jsonError) {
      notify.error("当前书源 JSON 语法有误，请修正后再调试");
      return;
    }
    if (!draft.bookSourceUrl?.trim()) {
      notify.error("请先填写或选择一个书源（需有 bookSourceUrl）");
      return;
    }
    if (act === "search" && !input) {
      notify.error("请输入搜索测试关键词");
      return;
    }
    if (act !== "search" && !input) {
      notify.error("请输入要调试的目标 URL");
      return;
    }

    if (act === "search") {
      setRecentTargets((prev) => {
        const next = [input, ...prev.filter((k) => k !== input)].slice(0, 6);
        saveRecentTargets(next);
        return next;
      });
    }

    stopStreamRef.current?.();
    stepCounterRef.current = 0;
    runStartRef.current = performance.now();
    setSteps([]);
    setResultData(null);
    setStreamError(null);
    setStartInfo("");
    setShowRawResult(false);
    setLogFilter("all");
    setAutoFollow(true);
    setUnseenCount(0);
    setRunElapsedMs(null);
    setRunning(true);
    setDebugStatus("running");

    const fallbackPhase = ACTION_PHASE[act];
    const serializedSource = JSON.stringify(draft);
    const stop = sourcesApi.debugStream(
      {
        action: act,
        bookSource: serializedSource,
        key: act === "search" ? input : undefined,
        chapterUrl: act !== "search" ? input : undefined,
      },
      {
        onData: (frame) => {
          if (frame.type === "start") {
            setStartInfo(
              `${frame.message.bookSource || draft.bookSourceName} · ${frame.message.action}`,
            );
          } else if (frame.type === "step") {
            stepCounterRef.current += 1;
            const at = Math.max(0, Math.round(performance.now() - runStartRef.current));
            const norm = normalizeStep(frame.message, stepCounterRef.current, fallbackPhase, at);
            setSteps((prev) => [...prev, norm]);
            if (!autoFollowRef.current) {
              setUnseenCount((c) => c + 1);
            }
          } else if (frame.type === "result") {
            setResultData(frame.data);
            recordChainHints(act, frame.data);
            setDebugStatus("done");
            setRunElapsedMs(Math.round(performance.now() - runStartRef.current));
          } else if (frame.type === "error") {
            setStreamError(frame.message || "规则执行失败");
            setDebugStatus("error");
            setRunElapsedMs(Math.round(performance.now() - runStartRef.current));
          }
        },
        onError: (err) => {
          setStreamError(err.message || "调试流异常中断");
          setDebugStatus("error");
          setRunElapsedMs(Math.round(performance.now() - runStartRef.current));
        },
        onClose: () => {
          stopStreamRef.current = null;
          setRunning(false);
          setDebugStatus((prev) => (prev === "running" ? "done" : prev));
          setRunElapsedMs((prev) =>
            prev ?? (runStartRef.current > 0 ? Math.round(performance.now() - runStartRef.current) : null),
          );
        },
      },
    );
    stopStreamRef.current = stop;
  };

  const runQuickToc = async () => {
    const input = targetInput.trim();
    if (!input) {
      notify.info("请输入书籍目录页 URL 后再运行单页目录速测");
      return;
    }
    if (jsonError || !draft.bookSourceUrl?.trim()) {
      notify.error("请先确保书源基础信息与 JSON 有效");
      return;
    }
    setTocQuickLoading(true);
    setStreamError(null);
    const start = performance.now();
    try {
      const res = await booksApi.getChaptersByRule({
        url: input,
        bookSource: JSON.stringify(draft),
      });
      const chapters = Array.isArray(res) ? res : res.chapters;
      const elapsed = Math.round(performance.now() - start);
      setAction("toc");
      setStartInfo(`单页目录速测 · ${draft.bookSourceName || draft.bookSourceUrl}`);
      setSteps([
        {
          id: 1,
          ruleName: "getChapterListByRule (单页速测)",
          phase: "toc",
          at: elapsed,
          url: input,
          elapsedMs: elapsed,
          resultLen: chapters.length,
        },
      ]);
      setResultData(chapters);
      recordChainHints("toc", chapters);
      setRunElapsedMs(elapsed);
      setDebugStatus("done");
      notify.success(`单页速测完成：解析到 ${chapters.length} 章 (${elapsed} ms)`);
    } catch (err) {
      const msg = errText(err);
      setStreamError(msg);
      setDebugStatus("error");
      notify.error(msg);
    } finally {
      setTocQuickLoading(false);
    }
  };

  const exploreCategories = React.useMemo(
    () => parseExploreUrl(draft.exploreUrl),
    [draft.exploreUrl],
  );

  const checkKeyWord = React.useMemo(
    () => asString(asRuleRecord(draft.ruleSearch).checkKeyWord).trim(),
    [draft.ruleSearch],
  );

  const pickerMatches = React.useMemo(() => {
    const q = pickerQuery.trim().toLowerCase();
    const list = q
      ? sources.filter(
          (s) =>
            s.bookSourceName.toLowerCase().includes(q) ||
            s.bookSourceUrl.toLowerCase().includes(q) ||
            (s.bookSourceGroup ?? "").toLowerCase().includes(q),
        )
      : sources;
    return { total: list.length, items: list.slice(0, PICKER_LIMIT) };
  }, [sources, pickerQuery]);

  // Step filter & phase grouping
  const errorStepCount = React.useMemo(() => steps.filter((s) => Boolean(s.error)).length, [steps]);
  const hitStepCount = React.useMemo(
    () => steps.filter((s) => s.resultLen !== undefined && s.resultLen > 0).length,
    [steps],
  );

  const visibleSteps = React.useMemo(() => {
    if (logFilter === "errors") return steps.filter((s) => Boolean(s.error));
    if (logFilter === "hits") {
      return steps.filter((s) => s.resultLen !== undefined && s.resultLen > 0);
    }
    return steps;
  }, [steps, logFilter]);

  const stepGroups = React.useMemo(() => groupSteps(visibleSteps), [visibleSteps]);

  // Pipeline phase statuses for milestone strip
  const pipelinePhases = ACTION_PIPELINE[action];
  const phaseStatusMap = React.useMemo(() => {
    const status: Partial<Record<Phase, "pending" | "active" | "done" | "error">> = {};
    const seenPhases = new Set<Phase>();
    for (const s of steps) {
      seenPhases.add(s.phase);
      if (s.error) {
        status[s.phase] = "error";
      } else if (status[s.phase] !== "error") {
        status[s.phase] = "done";
      }
    }
    const lastPhase = steps.length > 0 ? steps[steps.length - 1].phase : ACTION_PHASE[action];
    if (running && status[lastPhase] !== "error") {
      status[lastPhase] = "active";
    }
    if (streamError && seenPhases.size === 0) {
      status[ACTION_PHASE[action]] = "error";
    }
    return status;
  }, [steps, running, streamError, action]);

  const allGroupsOpen = React.useMemo(
    () => RULE_GROUPS.every((g) => openGroups[g.key]),
    [openGroups],
  );

  const toggleAllGroups = () => {
    const next = !allGroupsOpen;
    setOpenGroups({
      basic: next,
      search: next,
      bookInfo: next,
      toc: next,
      content: next,
    });
  };

  const jumpToGroup = (key: GroupKey) => {
    setEditorView("form");
    setOpenGroups((prev) => ({ ...prev, [key]: true }));
    window.requestAnimationFrame(() => {
      groupRefs.current[key]?.scrollIntoView({ behavior: reduced ? "auto" : "smooth", block: "start" });
    });
  };

  const normalizedFieldQuery = fieldFilter.trim().toLowerCase();

  const selectSourceFromPicker = (src: BookSource, close: () => void) => {
    loadSourceIntoDraft(src);
    setSearchParams({ url: src.bookSourceUrl }, { replace: true });
    close();
  };

  return (
    <div className="flex flex-col gap-3.5">
      {/* Top IDE Command Bar (flat hairline strip on mobile, elevated card on desktop) */}
      <div className="relative z-30 flex flex-wrap items-center justify-between gap-2 border-b border-line/70 pb-3 md:gap-2.5 md:rounded-2xl md:border md:border-line md:bg-elevated md:px-3.5 md:py-2.5 md:shadow-[var(--shadow-sm)]">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
          <Popover
            open={pickerOpen}
            onOpenChange={setPickerOpen}
            align="start"
            className="w-[min(340px,calc(100vw-16px))] p-2.5 sm:w-[420px]"
            trigger={
              <button
                type="button"
                className="inline-flex h-8 max-w-[calc(100vw-8.5rem)] cursor-pointer items-center gap-1.5 rounded-lg px-1.5 text-left text-xs font-medium text-ink transition-colors hover:bg-subtle md:max-w-[340px] md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-3"
              >
                <BookOpen className="size-3.5 shrink-0 text-accent" />
                <span className="truncate font-serif text-[14px] font-semibold md:font-sans md:text-xs md:font-medium">
                  {draft.bookSourceName || draft.bookSourceUrl || "选择已有书源…"}
                </span>
                {sources.length > 0 ? (
                  <span className="hidden rounded bg-subtle px-1.5 py-0.5 font-mono text-[10px] text-ink-muted tabular-nums sm:inline">
                    {sources.length}
                  </span>
                ) : null}
                <ChevronDown className="size-3.5 shrink-0 opacity-60" />
              </button>
            }
          >
            {(close) => (
              <div className="flex flex-col gap-2">
                <Input
                  ref={pickerInputRef}
                  size="sm"
                  value={pickerQuery}
                  onChange={(e) => {
                    setPickerQuery(e.target.value);
                    setPickerActiveIdx(0);
                  }}
                  clearable
                  onClear={() => {
                    setPickerQuery("");
                    setPickerActiveIdx(0);
                  }}
                  leftIcon={<Search className="size-3.5" />}
                  placeholder="按名称、分组或域名速搜…"
                  onKeyDown={(e) => {
                    if (e.key === "ArrowDown") {
                      e.preventDefault();
                      setPickerActiveIdx((i) =>
                        Math.min(i + 1, Math.max(0, pickerMatches.items.length - 1)),
                      );
                    } else if (e.key === "ArrowUp") {
                      e.preventDefault();
                      setPickerActiveIdx((i) => Math.max(0, i - 1));
                    } else if (e.key === "Enter" && pickerMatches.items[pickerActiveIdx]) {
                      e.preventDefault();
                      selectSourceFromPicker(pickerMatches.items[pickerActiveIdx], close);
                    }
                  }}
                />
                <div className="flex items-center justify-between px-1 text-[11px] text-ink-muted">
                  <span className="tabular-nums">
                    {pickerQuery.trim()
                      ? `命中 ${pickerMatches.total} / ${sources.length} 个书源`
                      : `共 ${sources.length} 个书源`}
                  </span>
                  <span className="hidden font-mono text-[10px] sm:inline">↑↓ 选择 · ↵ 载入</span>
                </div>
                <div className="max-h-72 overflow-y-auto pr-0.5">
                  {pickerMatches.items.length === 0 ? (
                    <p className="py-6 text-center text-xs text-ink-muted">无匹配书源</p>
                  ) : (
                    pickerMatches.items.map((src, idx) => {
                      const isCurrent = draft.bookSourceUrl === src.bookSourceUrl;
                      const isHighlighted = idx === pickerActiveIdx;
                      return (
                        <button
                          key={src.bookSourceUrl}
                          type="button"
                          onMouseEnter={() => setPickerActiveIdx(idx)}
                          onClick={() => selectSourceFromPicker(src, close)}
                          className={cn(
                            "flex w-full cursor-pointer flex-col gap-0.5 rounded-xl px-2.5 py-2 text-left transition-colors",
                            isCurrent
                              ? "bg-accent-soft/60 ring-1 ring-accent/25"
                              : isHighlighted
                                ? "bg-subtle"
                                : "hover:bg-subtle/70",
                          )}
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="truncate text-xs font-medium text-ink">
                              {src.bookSourceName}
                            </span>
                            <div className="flex shrink-0 items-center gap-1">
                              {src.bookSourceGroup ? (
                                <span className="max-w-[84px] truncate rounded bg-subtle px-1.5 py-0.5 text-[10px] text-ink-muted">
                                  {src.bookSourceGroup}
                                </span>
                              ) : null}
                              <Badge tone={src.enabled ? "success" : "default"}>
                                {src.enabled ? "启用" : "停用"}
                              </Badge>
                            </div>
                          </div>
                          <span className="truncate font-mono text-[10px] text-ink-muted">
                            {src.bookSourceUrl}
                          </span>
                        </button>
                      );
                    })
                  )}
                </div>
              </div>
            )}
          </Popover>

          {dirty ? (
            <InlineConfirm
              confirmLabel="放弃更改并新建"
              onConfirm={resetToBlank}
              trigger={(open) => (
                <Button variant="ghost" size="sm" onClick={open}>
                  <Plus className="size-3.5" />
                  新建空白
                </Button>
              )}
            />
          ) : (
            <Button variant="ghost" size="sm" onClick={resetToBlank}>
              <Plus className="size-3.5" />
              新建空白
            </Button>
          )}

          {/* Dirty & Error status badges */}
          {dirty ? (
            <div className="flex items-center gap-1.5">
              <Badge tone="warn" className="gap-1.5">
                <span className="size-1.5 animate-pulse rounded-full bg-warn" />
                未保存{totalModifiedCount > 0 ? ` · ${totalModifiedCount} 项` : ""}
              </Badge>
              <Button
                variant="ghost"
                size="xs"
                onClick={revertDraft}
                title="还原到上次保存状态"
                className="text-ink-muted hover:text-ink"
              >
                <RotateCcw className="size-3" />
                还原
              </Button>
            </div>
          ) : draft.bookSourceUrl ? (
            <Badge tone="success" className="hidden sm:inline-flex">
              已同步
            </Badge>
          ) : null}

          {jsonError ? <Badge tone="danger">JSON 语法有误</Badge> : null}
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            disabled={Boolean(jsonError) || saveMutation.isPending}
            onClick={() => saveMutation.mutate()}
            title={`保存书源 (${MOD_KEY}S)`}
            className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg px-2.5 text-xs font-semibold text-accent transition-colors hover:bg-accent-soft disabled:opacity-45 md:rounded-full md:bg-accent md:px-3.5 md:font-medium md:text-accent-ink md:hover:brightness-110"
          >
            <Save className="size-3.5" />
            <span>{saveMutation.isPending ? "保存中…" : "保存"}</span>
            <span className="hidden md:inline">书源</span>
            <kbd className="ml-0.5 hidden rounded bg-white/15 px-1.5 py-0.5 font-mono text-[10px] leading-none font-normal sm:inline">
              {MOD_KEY}S
            </kbd>
          </button>
        </div>
      </div>

      {/* Dual-Pane IDE: Structured Rule Editor + Real-time SSE Debug Console */}
      <div className="grid grid-cols-1 gap-5 md:gap-4 lg:grid-cols-12 lg:items-start">
        {/* Left Pane: Collapsible Rule Groups & Raw JSON (flat on mobile, card on desktop) */}
        <section className="flex flex-col md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated lg:col-span-6">
          {/* Left Pane Header */}
          <div className="flex flex-col gap-2.5 border-b border-line/70 pb-3 md:border-line md:px-4 md:py-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-5 md:hidden" role="tablist">
                {(
                  [
                    { value: "form" as const, label: "规则分组", icon: Settings2 },
                    { value: "json" as const, label: "原始 JSON", icon: Code2 },
                  ]
                ).map((item) => {
                  const Icon = item.icon;
                  const active = editorView === item.value;
                  return (
                    <button
                      key={item.value}
                      type="button"
                      role="tab"
                      aria-selected={active}
                      onClick={() => setEditorView(item.value)}
                      className={cn(
                        "inline-flex cursor-pointer items-center gap-1.5 border-b-2 py-1.5 text-[13px] transition-colors",
                        active
                          ? "border-accent font-semibold text-accent"
                          : "border-transparent text-ink-muted hover:text-ink",
                      )}
                    >
                      <Icon className="size-3.5" />
                      {item.label}
                    </button>
                  );
                })}
              </div>
              <div className="hidden md:block">
                <Segmented<EditorView>
                  layoutId="v2-wb-editor-view"
                  size="sm"
                  value={editorView}
                  onChange={setEditorView}
                  options={[
                    { value: "form", label: "规则分组", icon: <Settings2 className="size-3.5" /> },
                    { value: "json", label: "原始 JSON", icon: <Code2 className="size-3.5" /> },
                  ]}
                />
              </div>

              {editorView === "form" ? (
                <div className="flex items-center gap-1.5">
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={toggleAllGroups}
                    title={allGroupsOpen ? "全部折叠" : "全部展开"}
                  >
                    {allGroupsOpen ? (
                      <>
                        <ChevronsDownUp className="size-3.5" />
                        全部折叠
                      </>
                    ) : (
                      <>
                        <ChevronsUpDown className="size-3.5" />
                        全部展开
                      </>
                    )}
                  </Button>
                </div>
              ) : (
                <div className="flex items-center gap-1.5">
                  <Badge tone={jsonError ? "danger" : "success"}>
                    {jsonError ? "语法有误" : "JSON 有效"}
                  </Badge>
                  <Button
                    variant="ghost"
                    size="xs"
                    disabled={Boolean(jsonError)}
                    onClick={() => setRawJson(JSON.stringify(draft, null, 2))}
                  >
                    <Code2 className="size-3" />
                    格式化
                  </Button>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={() => void copyText(rawJson, "已复制书源 JSON")}
                  >
                    <Copy className="size-3" />
                    复制
                  </Button>
                </div>
              )}
            </div>

            {editorView === "form" ? (
              <div className="flex flex-col gap-2">
                <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1 md:gap-1.5">
                  {RULE_GROUPS.map((g) => {
                    const mod = groupModCounts[g.key];
                    const { filled } = countFilled(g, draft);
                    const isOpen = openGroups[g.key];
                    return (
                      <button
                        key={g.key}
                        type="button"
                        onClick={() => jumpToGroup(g.key)}
                        className={cn(
                          "inline-flex cursor-pointer items-center gap-1 py-0.5 text-xs transition-colors md:rounded-full md:border md:px-2.5 md:text-[11px] md:font-medium",
                          isOpen
                            ? "font-semibold text-accent md:border-accent/35 md:bg-accent-soft/70"
                            : "text-ink-muted hover:text-ink md:border-line md:bg-subtle/60 md:text-ink-secondary md:hover:border-line-strong",
                        )}
                      >
                        <span>{g.shortLabel}</span>
                        <span className="font-mono text-[10px] opacity-70 tabular-nums">
                          {filled}
                        </span>
                        {mod > 0 ? (
                          <span
                            className="size-1.5 rounded-full bg-warn"
                            title={`${mod} 项未保存修改`}
                          />
                        ) : null}
                      </button>
                    );
                  })}
                </div>

                <Input
                  size="sm"
                  value={fieldFilter}
                  onChange={(e) => setFieldFilter(e.target.value)}
                  clearable
                  onClear={() => setFieldFilter("")}
                  leftIcon={<Search className="size-3.5" />}
                  placeholder="过滤规则字段名或规则内容，如 bookList、tocUrl…"
                />
              </div>
            ) : null}
          </div>

          {/* Left Pane Body (natural page scroll on mobile, fixed pane scroll on desktop) */}
          <div className="pt-2 md:max-h-[calc(100dvh-16.5rem)] md:min-h-[480px] md:overflow-y-auto md:p-4">
            {editorView === "form" ? (
              <div className="divide-y divide-line/70 md:flex md:flex-col md:gap-3 md:divide-y-0">
                {RULE_GROUPS.map((group) => {
                  const isSearching = normalizedFieldQuery.length > 0;
                  const modCount = groupModCounts[group.key];
                  const { filled, total } = countFilled(group, draft);

                  // Filter fields inside this group when search query is active
                  const filteredBasicFields =
                    group.key === "basic"
                      ? BASIC_FIELDS.filter((f) =>
                          matchesFilter(
                            normalizedFieldQuery,
                            f.key,
                            f.label,
                            basicValueOf(draft, f.key),
                          ),
                        )
                      : [];
                  const filteredRuleFields = group.fields
                    ? group.fields.filter((f) =>
                        matchesFilter(
                          normalizedFieldQuery,
                          f.key,
                          f.label,
                          asString(asRuleRecord(draft[group.ruleKey!])[f.key]),
                        ),
                      )
                    : [];

                  const matchedCount =
                    group.key === "basic" ? filteredBasicFields.length : filteredRuleFields.length;

                  if (isSearching && matchedCount === 0) return null;

                  const isOpen = isSearching || openGroups[group.key];
                  const Icon = group.icon;

                  return (
                    <div
                      key={group.key}
                      ref={(el) => {
                        groupRefs.current[group.key] = el;
                      }}
                      className={cn(
                        "transition-colors md:overflow-hidden md:rounded-xl md:border",
                        modCount > 0
                          ? "md:border-warn/40 md:bg-elevated"
                          : "md:border-line md:bg-elevated",
                      )}
                    >
                      <button
                        type="button"
                        onClick={() =>
                          setOpenGroups((prev) => ({ ...prev, [group.key]: !prev[group.key] }))
                        }
                        aria-expanded={isOpen}
                        className="flex w-full cursor-pointer items-center justify-between gap-2 py-3 text-left transition-colors hover:bg-subtle/40 md:bg-subtle/45 md:px-3.5 md:py-2.5 md:hover:bg-subtle/80"
                      >
                        <div className="flex min-w-0 items-center gap-2">
                          {isOpen ? (
                            <ChevronDown className="size-3.5 shrink-0 text-ink-muted" />
                          ) : (
                            <ChevronRight className="size-3.5 shrink-0 text-ink-muted" />
                          )}
                          <Icon className="size-3.5 shrink-0 text-accent" />
                          <span className="text-xs font-semibold text-ink">{group.label}</span>
                          <span className="hidden truncate font-mono text-[10px] text-ink-muted sm:inline">
                            {group.hint}
                          </span>
                        </div>
                        <div className="flex shrink-0 items-center gap-1.5">
                          {modCount > 0 ? (
                            <Badge tone="warn">已改 {modCount}</Badge>
                          ) : null}
                          <Badge tone={filled > 0 ? "default" : "default"}>
                            {filled}/{total}
                          </Badge>
                        </div>
                      </button>

                      <AnimatePresence initial={false}>
                        {isOpen ? (
                          <motion.div
                            initial={reduced ? { opacity: 0 } : { height: 0, opacity: 0 }}
                            animate={{ height: "auto", opacity: 1 }}
                            exit={reduced ? { opacity: 0 } : { height: 0, opacity: 0 }}
                            transition={reduced ? { duration: 0.1 } : SPRING_SNAPPY}
                            className="overflow-hidden md:border-t md:border-line"
                          >
                            <div className="pb-4 md:p-3.5">
                              {group.key === "basic" ? (
                                <BasicFieldsEditor
                                  fields={filteredBasicFields}
                                  draft={draft}
                                  baseline={baseline}
                                  showSwitches={!isSearching}
                                  onUpdateDraft={updateDraft}
                                />
                              ) : (
                                <RuleFieldsEditor
                                  group={group}
                                  fields={filteredRuleFields}
                                  record={asRuleRecord(draft[group.ruleKey!])}
                                  draft={draft}
                                  baseline={baseline}
                                  onChange={(k, v) => updateRuleField(group.ruleKey!, k, v)}
                                />
                              )}
                            </div>
                          </motion.div>
                        ) : null}
                      </AnimatePresence>
                    </div>
                  );
                })}

                {normalizedFieldQuery &&
                RULE_GROUPS.every((g) => {
                  if (g.key === "basic") {
                    return !BASIC_FIELDS.some((f) =>
                      matchesFilter(
                        normalizedFieldQuery,
                        f.key,
                        f.label,
                        basicValueOf(draft, f.key),
                      ),
                    );
                  }
                  return !g.fields?.some((f) =>
                    matchesFilter(
                      normalizedFieldQuery,
                      f.key,
                      f.label,
                      asString(asRuleRecord(draft[g.ruleKey!])[f.key]),
                    ),
                  );
                }) ? (
                  <EmptyView
                    compact
                    icon={<Search className="size-4" />}
                    title="未找到匹配的规则字段"
                    description={`没有包含「${fieldFilter}」的字段名或规则值`}
                    action={
                      <Button variant="ghost" size="xs" onClick={() => setFieldFilter("")}>
                        清空过滤
                      </Button>
                    }
                  />
                ) : null}
              </div>
            ) : (
              <div className="flex flex-col gap-3">
                {jsonError ? (
                  <p className="rounded-xl border border-danger/30 bg-danger-soft/50 px-3 py-2 font-mono text-[11px] text-danger">
                    {jsonError}
                  </p>
                ) : null}
                <Textarea
                  mono
                  rows={24}
                  value={rawJson}
                  onChange={(e) => handleRawJsonChange(e.target.value)}
                />
              </div>
            )}
          </div>
        </section>

        {/* Right Pane: Real-time SSE Debug Console (flat hairline section on mobile, card on desktop) */}
        <section className="flex flex-col border-t border-line pt-4 md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated md:pt-0 lg:col-span-6">
          {/* Debug Action & Target Controls */}
          <div className="flex flex-col gap-2.5 border-b border-line/70 pb-3.5 md:border-line md:p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-4 md:hidden" role="tablist">
                {(
                  [
                    { value: "search" as const, label: "搜索" },
                    { value: "explore" as const, label: "发现" },
                    { value: "toc" as const, label: "目录" },
                    { value: "content" as const, label: "正文" },
                  ]
                ).map((item) => (
                  <button
                    key={item.value}
                    type="button"
                    role="tab"
                    aria-selected={action === item.value}
                    onClick={() => setAction(item.value)}
                    className={cn(
                      "cursor-pointer border-b-2 py-1 text-xs transition-colors",
                      action === item.value
                        ? "border-accent font-semibold text-accent"
                        : "border-transparent text-ink-muted hover:text-ink",
                    )}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
              <div className="hidden md:block">
                <Segmented<DebugAction>
                  layoutId="v2-wb-action"
                  size="sm"
                  value={action}
                  onChange={setAction}
                  options={[
                    { value: "search", label: "搜索" },
                    { value: "explore", label: "发现" },
                    { value: "toc", label: "目录" },
                    { value: "content", label: "正文" },
                  ]}
                />
              </div>

              <Button
                variant="subtle"
                size="xs"
                loading={tocQuickLoading}
                onClick={() => void runQuickToc()}
                title="调用 getChapterListByRule 快速测试单页目录解析"
              >
                <Zap className="size-3" />
                单页目录速测
              </Button>
            </div>

            <div className="flex items-center gap-2">
              <Input
                size="sm"
                value={targetInput}
                onChange={(e) => setTargetInput(e.target.value)}
                clearable
                onClear={() => setTargetInput("")}
                placeholder={
                  action === "search"
                    ? "输入测试书名，如：诡秘之主"
                    : action === "explore"
                      ? "输入发现分类 URL（或点下方分类一键填入）"
                      : action === "toc"
                        ? "输入书籍详情页或目录页 URL"
                        : "输入章节正文页 URL"
                }
                className="flex-1"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !running) runDebug();
                }}
              />

              {running ? (
                <Button variant="danger" size="sm" onClick={stopDebug}>
                  <StopIcon className="size-3.5" />
                  停止
                </Button>
              ) : (
                <Button variant="primary" size="sm" onClick={() => runDebug()}>
                  <Play className="size-3.5" />
                  运行调试
                </Button>
              )}
            </div>

            {/* Quick Presets Strip (Search Keywords / Explore URLs / Chained URLs) */}
            {action === "search" ? (
              <div className="flex items-center gap-1.5 overflow-x-auto pb-0.5 no-scrollbar">
                <span className="inline-flex shrink-0 items-center gap-1 text-[11px] text-ink-muted">
                  <Sparkles className="size-3 text-accent" />
                  快捷词:
                </span>
                {checkKeyWord ? (
                  <button
                    type="button"
                    onClick={() => setTargetInput(checkKeyWord)}
                    className={cn(
                      "shrink-0 cursor-pointer rounded-full border px-2.5 py-0.5 text-[11px] font-medium transition-colors",
                      targetInput === checkKeyWord
                        ? "border-accent bg-accent-soft text-accent"
                        : "border-accent/30 bg-accent-soft/40 text-accent hover:bg-accent-soft",
                    )}
                    title="书源自带的校验关键词 (checkKeyWord)"
                  >
                    ★ {checkKeyWord}
                  </button>
                ) : null}
                {SEARCH_PRESETS.filter((kw) => kw !== checkKeyWord).map((kw) => (
                  <button
                    key={kw}
                    type="button"
                    onClick={() => setTargetInput(kw)}
                    className={cn(
                      "shrink-0 cursor-pointer rounded-full border px-2.5 py-0.5 text-[11px] transition-colors",
                      targetInput === kw
                        ? "border-accent bg-accent-soft text-accent"
                        : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong hover:text-ink",
                    )}
                  >
                    {kw}
                  </button>
                ))}
                {recentTargets
                  .filter((kw) => kw !== checkKeyWord && !(SEARCH_PRESETS as readonly string[]).includes(kw))
                  .slice(0, 3)
                  .map((kw) => (
                    <button
                      key={kw}
                      type="button"
                      onClick={() => setTargetInput(kw)}
                      className={cn(
                        "inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] transition-colors",
                        targetInput === kw
                          ? "border-accent bg-accent-soft text-accent"
                          : "border-line bg-subtle/40 text-ink-muted hover:border-line-strong hover:text-ink",
                      )}
                      title="最近调试过的关键词"
                    >
                      <History className="size-2.5" />
                      {kw}
                    </button>
                  ))}
              </div>
            ) : action === "explore" ? (
              <div className="flex items-center gap-1.5 overflow-x-auto pb-0.5 no-scrollbar">
                <span className="inline-flex shrink-0 items-center gap-1 text-[11px] text-ink-muted">
                  <Compass className="size-3 text-accent" />
                  发现分类:
                </span>
                {exploreCategories.length > 0 ? (
                  exploreCategories.slice(0, 18).map((cat) => (
                    <button
                      key={`${cat.title}-${cat.url}`}
                      type="button"
                      onClick={() => setTargetInput(cat.url)}
                      title={cat.url}
                      className={cn(
                        "shrink-0 cursor-pointer rounded-full border px-2.5 py-0.5 text-[11px] transition-colors",
                        targetInput === cat.url
                          ? "border-accent bg-accent-soft text-accent"
                          : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong hover:text-ink",
                      )}
                    >
                      {cat.title}
                    </button>
                  ))
                ) : (
                  <span className="text-[11px] text-ink-muted">
                    当前书源未配置静态 exploreUrl 分类，请直接输入发现页 URL
                  </span>
                )}
              </div>
            ) : action === "toc" && chainHints.bookUrl ? (
              <div className="flex items-center gap-1.5 overflow-x-auto pb-0.5 no-scrollbar">
                <span className="shrink-0 text-[11px] text-ink-muted">最近搜索命中:</span>
                <button
                  type="button"
                  onClick={() => setTargetInput(chainHints.bookUrl!)}
                  title={chainHints.bookUrl}
                  className={cn(
                    "inline-flex max-w-full shrink-0 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-[11px] transition-colors",
                    targetInput === chainHints.bookUrl
                      ? "border-accent bg-accent-soft text-accent"
                      : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong hover:text-ink",
                  )}
                >
                  <BookOpen className="size-3 shrink-0" />
                  <span className="max-w-[180px] truncate">{chainHints.bookName}</span>
                  <span className="font-mono text-[10px] text-ink-muted">
                    {shortUrl(chainHints.bookUrl, 24)}
                  </span>
                </button>
              </div>
            ) : action === "content" && chainHints.chapterUrl ? (
              <div className="flex items-center gap-1.5 overflow-x-auto pb-0.5 no-scrollbar">
                <span className="shrink-0 text-[11px] text-ink-muted">最近目录首章:</span>
                <button
                  type="button"
                  onClick={() => setTargetInput(chainHints.chapterUrl!)}
                  title={chainHints.chapterUrl}
                  className={cn(
                    "inline-flex max-w-full shrink-0 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-[11px] transition-colors",
                    targetInput === chainHints.chapterUrl
                      ? "border-accent bg-accent-soft text-accent"
                      : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong hover:text-ink",
                  )}
                >
                  <FileText className="size-3 shrink-0" />
                  <span className="max-w-[200px] truncate">{chainHints.chapterTitle}</span>
                </button>
              </div>
            ) : null}

            {/* Stage Milestone Pipeline Strip */}
            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line/70 pt-2.5">
              <div className="flex flex-wrap items-center gap-1">
                {pipelinePhases.map((p, idx) => {
                  const st = phaseStatusMap[p] ?? "pending";
                  return (
                    <React.Fragment key={p}>
                      {idx > 0 ? (
                        <ChevronRight className="size-3 text-line-strong" aria-hidden />
                      ) : null}
                      <span
                        className={cn(
                          "inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium transition-colors",
                          st === "active"
                            ? "bg-accent-soft text-accent ring-1 ring-accent/30"
                            : st === "done"
                              ? "bg-success-soft/80 text-success"
                              : st === "error"
                                ? "bg-danger-soft text-danger"
                                : "bg-subtle/60 text-ink-muted",
                        )}
                      >
                        <span
                          className={cn(
                            "size-1.5 rounded-full",
                            st === "active"
                              ? "animate-pulse bg-accent"
                              : st === "done"
                                ? "bg-success"
                                : st === "error"
                                  ? "bg-danger"
                                  : "bg-line-strong",
                          )}
                        />
                        {PHASE_META[p].label}
                      </span>
                    </React.Fragment>
                  );
                })}
              </div>

              {/* Log Filter + Auto-Follow + Copy Tools */}
              <div className="flex flex-wrap items-center gap-1">
                {steps.length > 0 ? (
                  <>
                    <div className="inline-flex items-center gap-0.5 rounded-full border border-line bg-subtle/60 p-0.5 text-[11px]">
                      <Filter className="ml-1.5 size-3 text-ink-muted" />
                      {(
                        [
                          { key: "all", label: `全部 ${steps.length}` },
                          { key: "hits", label: `命中 ${hitStepCount}` },
                          { key: "errors", label: `错误 ${errorStepCount}` },
                        ] as const
                      ).map((opt) => (
                        <button
                          key={opt.key}
                          type="button"
                          onClick={() => setLogFilter(opt.key)}
                          className={cn(
                            "cursor-pointer rounded-full px-2 py-0.5 tabular-nums transition-colors",
                            logFilter === opt.key
                              ? opt.key === "errors" && errorStepCount > 0
                                ? "bg-danger text-white"
                                : "bg-elevated text-ink shadow-[var(--shadow-sm)]"
                              : "text-ink-muted hover:text-ink",
                          )}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>

                    <button
                      type="button"
                      onClick={() => {
                        const next = !autoFollow;
                        setAutoFollow(next);
                        if (next) {
                          setUnseenCount(0);
                          scrollTimelineToBottom(true);
                        }
                      }}
                      title={
                        autoFollow
                          ? "已开启自动跟随滚动（向上滚动查看历史时自动暂停）"
                          : "已暂停自动跟随，点击恢复并滚到底部"
                      }
                      className={cn(
                        "inline-flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium transition-colors",
                        autoFollow
                          ? "border-accent/30 bg-accent-soft/70 text-accent"
                          : "border-line bg-subtle/60 text-ink-muted hover:border-line-strong hover:text-ink",
                      )}
                    >
                      <ArrowDownToLine className="size-3" />
                      <span>{autoFollow ? "跟随中" : "已暂停"}</span>
                      {!autoFollow && unseenCount > 0 ? (
                        <span className="rounded-full bg-accent px-1.5 text-[10px] text-white tabular-nums">
                          +{unseenCount}
                        </span>
                      ) : null}
                    </button>

                    <Button
                      variant="ghost"
                      size="xs"
                      onClick={() =>
                        void copyText(
                          buildLogText(steps, startInfo, streamError),
                          "已复制结构化调试日志",
                        )
                      }
                      title="复制全部调试日志"
                    >
                      <Copy className="size-3" />
                      日志
                    </Button>

                    {!running ? (
                      <Button
                        variant="ghost"
                        size="xs"
                        onClick={() => {
                          setSteps([]);
                          setResultData(null);
                          setStreamError(null);
                          setStartInfo("");
                          setRunElapsedMs(null);
                          setDebugStatus("idle");
                        }}
                        title="清空日志与结果"
                      >
                        <Trash2 className="size-3" />
                      </Button>
                    ) : null}
                  </>
                ) : null}
              </div>
            </div>
          </div>

          {/* Streaming Timeline & Result */}
          <div className="relative flex flex-col">
            <div
              ref={timelineRef}
              onScroll={handleTimelineScroll}
              className="flex max-h-[calc(100dvh-19.5rem)] min-h-[460px] flex-col gap-3.5 overflow-y-auto p-4"
            >
              {debugStatus === "idle" && steps.length === 0 && !resultData && !streamError ? (
                <EmptyView
                  className="my-auto"
                  icon={<Terminal className="size-5" />}
                  title="实时规则追踪时间线"
                  description="左侧编辑的规则无需保存即可直接流式运行，按阶段高亮展示每一步解析耗时与命中结果，并支持一键串行追踪目录与正文"
                />
              ) : (
                <>
                  {/* Status Banner */}
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-line bg-subtle/45 px-3.5 py-2 text-xs">
                    <div className="flex min-w-0 items-center gap-2">
                      {running ? (
                        <Spinner label="正在实时追踪规则执行…" />
                      ) : debugStatus === "error" ? (
                        <span className="inline-flex shrink-0 items-center gap-1.5 font-medium text-danger">
                          <XCircle className="size-4" />
                          调试出错
                        </span>
                      ) : (
                        <span className="inline-flex shrink-0 items-center gap-1.5 font-medium text-success">
                          <CheckCircle2 className="size-4" />
                          追踪完成
                        </span>
                      )}
                      {startInfo ? (
                        <span className="truncate text-ink-muted">· {startInfo}</span>
                      ) : null}
                    </div>
                    <div className="flex shrink-0 items-center gap-1.5 tabular-nums">
                      {runElapsedMs !== null ? (
                        <Badge>
                          <Clock3 className="size-3" />
                          {fmtMs(runElapsedMs)}
                        </Badge>
                      ) : null}
                      <Badge>{steps.length} 步</Badge>
                    </div>
                  </div>

                  {/* Error Alert */}
                  {streamError ? (
                    <div className="flex items-start gap-2 rounded-xl border border-danger/30 bg-danger-soft/50 px-3.5 py-2.5 text-xs text-danger">
                      <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                      <span className="break-all">{streamError}</span>
                    </div>
                  ) : null}

                  {/* Phase-Grouped Step Timeline */}
                  {steps.length > 0 ? (
                    visibleSteps.length === 0 ? (
                      <div className="rounded-xl border border-dashed border-line py-6 text-center text-xs text-ink-muted">
                        当前过滤条件下无匹配步骤（共 {steps.length} 步）
                        <button
                          type="button"
                          onClick={() => setLogFilter("all")}
                          className="ml-2 cursor-pointer text-accent underline underline-offset-2"
                        >
                          查看全部
                        </button>
                      </div>
                    ) : (
                      <div className="flex flex-col gap-3">
                        {stepGroups.map((grp) => {
                          const meta = PHASE_META[grp.phase];
                          return (
                            <div
                              key={grp.key}
                              className="flex flex-col gap-2 rounded-xl border border-line/80 bg-subtle/20 p-2.5"
                            >
                              <div className="flex flex-wrap items-center justify-between gap-2 px-1">
                                <div className="flex items-center gap-1.5">
                                  <span className={cn("size-2 rounded-full", meta.dot)} />
                                  <span className={cn("text-xs font-semibold", meta.text)}>
                                    {meta.label}阶段
                                  </span>
                                  <span className="font-mono text-[10px] text-ink-muted tabular-nums">
                                    {grp.steps.length} 步
                                  </span>
                                </div>
                                <div className="flex items-center gap-1.5 tabular-nums">
                                  {grp.totalMs > 0 ? (
                                    <span className="font-mono text-[10px] text-ink-muted">
                                      {fmtMs(grp.totalMs)}
                                    </span>
                                  ) : null}
                                  {grp.hitCount > 0 ? (
                                    <Badge tone="success">提取 {grp.hitCount}</Badge>
                                  ) : null}
                                  {grp.errorCount > 0 ? (
                                    <Badge tone="danger">异常 {grp.errorCount}</Badge>
                                  ) : null}
                                </div>
                              </div>

                              <ol className="relative flex flex-col gap-2 pl-4 before:absolute before:top-2 before:bottom-2 before:left-1.5 before:w-px before:bg-line-strong">
                                {grp.steps.map((step) => (
                                  <StepCard key={step.id} step={step} />
                                ))}
                              </ol>
                            </div>
                          );
                        })}
                      </div>
                    )
                  ) : null}

                  {/* Final Parsed Result */}
                  {resultData !== null && resultData !== undefined ? (
                    <DebugResultPanel
                      action={action}
                      data={resultData}
                      showRaw={showRawResult}
                      onToggleRaw={() => setShowRawResult((v) => !v)}
                      onChainToc={(bookUrl) => runDebug("toc", bookUrl)}
                      onChainContent={(chapterUrl) => runDebug("content", chapterUrl)}
                    />
                  ) : null}
                </>
              )}
            </div>

            {/* Floating resume auto-follow pill when user scrolled up during output */}
            <AnimatePresence>
              {!autoFollow && unseenCount > 0 ? (
                <motion.button
                  type="button"
                  initial={reduced ? { opacity: 0 } : { opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={reduced ? { opacity: 0 } : { opacity: 0, y: 8 }}
                  transition={SPRING_SNAPPY}
                  onClick={() => {
                    setAutoFollow(true);
                    setUnseenCount(0);
                    scrollTimelineToBottom(true);
                  }}
                  className="absolute right-4 bottom-4 inline-flex cursor-pointer items-center gap-1.5 rounded-full border border-accent/30 bg-elevated/95 px-3 py-1.5 text-xs font-medium text-accent shadow-[var(--shadow-float)] backdrop-blur-sm transition-colors hover:bg-accent-soft"
                >
                  <ArrowDownToLine className="size-3.5" />
                  <span>{unseenCount} 条新日志 · 回到底部</span>
                </motion.button>
              ) : null}
            </AnimatePresence>
          </div>
        </section>
      </div>
    </div>
  );
}

/* ==========================================================================
   Basic Info Fields Helper
   ========================================================================== */

function BasicFieldsEditor({
  fields,
  draft,
  baseline,
  showSwitches,
  onUpdateDraft,
}: {
  fields: readonly BasicFieldSpec[];
  draft: BookSource;
  baseline: BookSource;
  showSwitches: boolean;
  onUpdateDraft: (updater: (prev: BookSource) => BookSource) => void;
}) {
  const basicGroup = RULE_GROUPS[0];
  return (
    <div className="flex flex-col gap-3">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        {fields.map((f) => {
          const val = asString(draft[f.key]);
          const modified = isFieldModified(basicGroup, f.key, draft, baseline);
          const onChange = (nextVal: string) =>
            onUpdateDraft((p) => ({
              ...p,
              [f.key]: f.nullable ? nextVal || null : nextVal,
            }));

          return (
            <div
              key={f.key}
              className={cn(
                f.half ? "sm:col-span-1" : "sm:col-span-2",
                modified && "relative pl-2.5 before:absolute before:top-1 before:bottom-1 before:left-0 before:w-0.5 before:rounded-full before:bg-warn",
              )}
            >
              {f.rows ? (
                <Textarea
                  label={f.label}
                  mono={f.key !== "bookSourceComment"}
                  rows={f.rows}
                  placeholder={f.placeholder}
                  value={val}
                  onChange={(e) => onChange(e.target.value)}
                />
              ) : (
                <Input
                  label={f.label}
                  placeholder={f.placeholder}
                  value={val}
                  onChange={(e) => onChange(e.target.value)}
                />
              )}
            </div>
          );
        })}
      </div>

      {showSwitches ? (
        <div className="grid grid-cols-3 gap-2 rounded-xl border border-line bg-subtle/40 p-3">
          {BASIC_SWITCHES.map((sw) => {
            const modified = isFieldModified(basicGroup, sw.key, draft, baseline);
            return (
              <label
                key={sw.key}
                className="flex items-center justify-between gap-2 text-xs text-ink"
              >
                <span className="inline-flex items-center gap-1">
                  {modified ? <span className="size-1.5 rounded-full bg-warn" /> : null}
                  {sw.label}
                </span>
                <Switch
                  checked={Boolean(draft[sw.key])}
                  onChange={(v) => onUpdateDraft((p) => ({ ...p, [sw.key]: v }))}
                />
              </label>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}

/* ==========================================================================
   Structured Rule Fields Helper
   ========================================================================== */

function RuleFieldsEditor({
  group,
  fields,
  record,
  draft,
  baseline,
  onChange,
}: {
  group: RuleGroupSpec;
  fields: readonly RuleFieldSpec[];
  record: Record<string, unknown>;
  draft: BookSource;
  baseline: BookSource;
  onChange: (key: string, value: string) => void;
}) {
  return (
    <div className="flex flex-col gap-3">
      {fields.map((f) => {
        const val = asString(record[f.key]);
        const modified = isFieldModified(group, f.key, draft, baseline);
        return (
          <div
            key={f.key}
            className={cn(
              modified &&
                "relative pl-2.5 before:absolute before:top-1 before:bottom-1 before:left-0 before:w-0.5 before:rounded-full before:bg-warn",
            )}
          >
            {f.multiline ? (
              <Textarea
                label={f.label}
                mono
                rows={2}
                placeholder={f.placeholder}
                value={val}
                onChange={(e) => onChange(f.key, e.target.value)}
              />
            ) : (
              <Input
                label={f.label}
                placeholder={f.placeholder}
                value={val}
                onChange={(e) => onChange(f.key, e.target.value)}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

/* ==========================================================================
   Collapsible SSE Step Card
   ========================================================================== */

function StepCard({ step }: { step: NormalizedStep }) {
  const [expanded, setExpanded] = React.useState(Boolean(step.error));
  const hasDetail = step.detail !== undefined && step.detail !== null;
  const isMilestone = Boolean(step.error) || (step.resultLen !== undefined && step.resultLen > 0);

  return (
    <li
      className={cn(
        "relative rounded-xl border px-3 py-2 transition-colors",
        step.error
          ? "border-danger/35 bg-danger-soft/25"
          : isMilestone
            ? "border-accent/25 bg-elevated"
            : "border-line bg-elevated/80",
      )}
    >
      <span
        aria-hidden
        className={cn(
          "absolute top-3 -left-[15px] size-2.5 rounded-full border-2 border-elevated",
          step.error
            ? "bg-danger"
            : step.resultLen !== undefined && step.resultLen > 0
              ? "bg-success"
              : "bg-accent",
        )}
      />

      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="font-mono text-[10px] text-ink-muted tabular-nums">#{step.id}</span>
          {step.at !== undefined ? (
            <span className="font-mono text-[10px] text-ink-muted/80 tabular-nums">
              +{(step.at / 1000).toFixed(2)}s
            </span>
          ) : null}
          <span className="truncate text-xs font-medium text-ink">{step.ruleName}</span>
        </div>

        <div className="flex shrink-0 items-center gap-1.5 tabular-nums">
          {step.elapsedMs !== undefined ? <Badge>{fmtMs(step.elapsedMs)}</Badge> : null}
          {step.resultLen !== undefined ? (
            <Badge tone={step.resultLen > 0 ? "success" : "warn"}>命中 {step.resultLen}</Badge>
          ) : null}
          {step.error ? <Badge tone="danger">异常</Badge> : null}
          {hasDetail ? (
            <button
              type="button"
              onClick={() => setExpanded((v) => !v)}
              className="inline-flex cursor-pointer items-center gap-0.5 rounded-full px-1.5 py-0.5 text-[11px] text-ink-secondary hover:bg-subtle hover:text-ink"
            >
              {expanded ? (
                <ChevronDown className="size-3.5" />
              ) : (
                <ChevronRight className="size-3.5" />
              )}
              详情
            </button>
          ) : null}
        </div>
      </div>

      {step.url ? (
        <div className="mt-1 flex items-center gap-1.5">
          <p className="min-w-0 flex-1 truncate font-mono text-[10px] text-ink-muted" title={step.url}>
            {step.url}
          </p>
          <button
            type="button"
            onClick={() => void copyText(step.url!, "已复制请求 URL")}
            className="shrink-0 cursor-pointer rounded p-0.5 text-ink-muted hover:bg-subtle hover:text-ink"
            title="复制 URL"
          >
            <Copy className="size-2.5" />
          </button>
        </div>
      ) : null}

      {step.error ? (
        <p className="mt-1.5 rounded-lg bg-danger-soft/60 px-2.5 py-1.5 font-mono text-[11px] break-all text-danger">
          {step.error}
        </p>
      ) : null}

      {expanded && hasDetail ? (
        <pre className="mt-2 max-h-56 overflow-auto rounded-lg border border-line bg-subtle/40 p-2.5 font-mono text-[11px] leading-relaxed text-ink-secondary">
          {typeof step.detail === "string" ? step.detail : JSON.stringify(step.detail, null, 2)}
        </pre>
      ) : null}
    </li>
  );
}

/* ==========================================================================
   Formatted Debug Result Panel (Chainable Search -> TOC -> Content!)
   ========================================================================== */

function DebugResultPanel({
  action,
  data,
  showRaw,
  onToggleRaw,
  onChainToc,
  onChainContent,
}: {
  action: DebugAction;
  data: unknown;
  showRaw: boolean;
  onToggleRaw: () => void;
  onChainToc: (bookUrl: string) => void;
  onChainContent: (chapterUrl: string) => void;
}) {
  const jsonText = React.useMemo(() => JSON.stringify(data, null, 2), [data]);

  const books =
    (action === "search" || action === "explore") && Array.isArray(data)
      ? (data as SearchBook[])
      : null;

  const chapters =
    action === "toc" && Array.isArray(data) ? (data as BookChapter[]) : null;

  const contentString =
    action === "content"
      ? typeof data === "string"
        ? data
        : data && typeof data === "object" && "content" in data
          ? String((data as { content?: unknown }).content ?? "")
          : null
      : null;

  return (
    <div className="flex flex-col gap-2.5 rounded-2xl border border-line bg-subtle/30 p-3.5">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <ListTree className="size-4 text-accent" />
          <span className="font-serif text-sm font-semibold text-ink">解析结果</span>
          {books ? <Badge tone="success">{books.length} 本</Badge> : null}
          {chapters ? <Badge tone="success">{chapters.length} 章</Badge> : null}
          {contentString !== null ? (
            <Badge tone="success">{contentString.length} 字</Badge>
          ) : null}
        </div>
        <div className="flex items-center gap-1">
          <Button variant="ghost" size="xs" onClick={onToggleRaw}>
            <Code2 className="size-3" />
            {showRaw ? "友好视图" : "原始 JSON"}
          </Button>
          <Button
            variant="ghost"
            size="xs"
            onClick={() => void copyText(jsonText, "已复制解析结果 JSON")}
          >
            <Copy className="size-3" />
            复制
          </Button>
        </div>
      </div>

      {showRaw ? (
        <pre className="max-h-96 overflow-auto rounded-xl border border-line bg-elevated p-3 font-mono text-[11px] leading-relaxed text-ink">
          {jsonText}
        </pre>
      ) : books ? (
        books.length === 0 ? (
          <p className="py-4 text-center text-xs text-ink-muted">返回结果为空数组</p>
        ) : (
          <div className="max-h-96 divide-y divide-line overflow-y-auto rounded-xl border border-line bg-elevated">
            {books.slice(0, 30).map((book, idx) => (
              <div
                key={`${book.bookUrl}-${idx}`}
                className="flex items-center justify-between gap-3 px-3.5 py-2.5 transition-colors hover:bg-subtle/40"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate text-xs font-medium text-ink">
                    {book.name || "无书名"}
                    {book.author ? (
                      <span className="ml-1.5 font-normal text-ink-muted">{book.author}</span>
                    ) : null}
                  </p>
                  <p className="mt-0.5 truncate font-mono text-[10px] text-ink-muted">
                    {book.bookUrl}
                  </p>
                </div>
                {book.bookUrl ? (
                  <Button
                    variant="subtle"
                    size="xs"
                    onClick={() => onChainToc(book.bookUrl)}
                  >
                    测目录
                  </Button>
                ) : null}
              </div>
            ))}
          </div>
        )
      ) : chapters ? (
        chapters.length === 0 ? (
          <p className="py-4 text-center text-xs text-ink-muted">未解析到章节条目</p>
        ) : (
          <div className="max-h-96 divide-y divide-line overflow-y-auto rounded-xl border border-line bg-elevated">
            {chapters.slice(0, 80).map((ch, idx) => (
              <div
                key={`${ch.url}-${idx}`}
                className="flex items-center justify-between gap-2 px-3.5 py-2 transition-colors hover:bg-subtle/40"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate text-xs text-ink">
                    <span className="mr-1.5 font-mono text-[10px] text-ink-muted tabular-nums">
                      {idx + 1}.
                    </span>
                    {ch.title}
                  </p>
                  <p className="truncate font-mono text-[10px] text-ink-muted">{ch.url}</p>
                </div>
                {ch.url && !ch.isVolume ? (
                  <Button
                    variant="subtle"
                    size="xs"
                    onClick={() => onChainContent(ch.url)}
                  >
                    测正文
                  </Button>
                ) : null}
              </div>
            ))}
          </div>
        )
      ) : contentString !== null ? (
        <div className="max-h-96 overflow-y-auto rounded-xl border border-line bg-elevated p-4 font-serif text-[13px] leading-relaxed whitespace-pre-wrap text-ink">
          {contentString || <span className="text-ink-muted">（正文为空）</span>}
        </div>
      ) : (
        <pre className="max-h-96 overflow-auto rounded-xl border border-line bg-elevated p-3 font-mono text-[11px] leading-relaxed text-ink">
          {jsonText}
        </pre>
      )}
    </div>
  );
}
