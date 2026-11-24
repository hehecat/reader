import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  CheckSquare,
  Copy,
  Download,
  FileJson,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Sparkles,
  Square,
  Trash2,
  Upload,
  Wand2,
  X,
} from "lucide-react";
import * as React from "react";
import { featuresApi } from "@/api/endpoints";
import type { ReplaceRule } from "@/api/types";
import { cn } from "@/lib/cn";
import { notify } from "@/stores/toast";
import { Button, IconButton } from "@/ui/Button";
import { Badge, EmptyView, InlineConfirm, Segmented, Spinner, Switch } from "@/ui/Controls";
import { Input, Textarea } from "@/ui/Input";
import { AnimatePresence, motion, SPRING_POP } from "@/ui/motion";
import { Sheet } from "@/ui/Sheet";

const DEFAULT_SAMPLE_TEXT = `夜风拂过长街，灯火渐次熄灭。
【笔趣阁 www.example.com 最快更新，免费阅读！】
他合上手中的古籍，目光越过窗棂望向远山。
本章未完，请点击下一页继续阅读，请收藏本站防止走失。
“原来如此。”他低声自语，唇角浮现一抹笑意。(ps:求月票求推荐票！)`;

const ANTI_SCRAPE_SAMPLE_TEXT = `少年握紧长剑，踏入风雪之中。
天才一秒记住本站地址：[www.demo-book.cc] 最快更新！无广告！
剑光破空而去，山谷间回荡起清越剑鸣。
——请记住本站域名 demo-book.cc，手机访问 m.demo-book.cc 阅读体验更佳——
风雪渐止，天边露出一线微光。`;

interface RuleDraft {
  id: string;
  name: string;
  group: string;
  find: string;
  replace: string;
  scope: string;
  scopeTitle: boolean;
  scopeContent: boolean;
  isRegex: boolean;
  timeoutMillisecond: number;
  enabled: boolean;
  order: number;
}

const EMPTY_DRAFT: RuleDraft = {
  id: "",
  name: "",
  group: "",
  find: "",
  replace: "",
  scope: "",
  scopeTitle: false,
  scopeContent: true,
  isRegex: true,
  timeoutMillisecond: 3000,
  enabled: true,
  order: 0,
};

function toDraft(r: ReplaceRule): RuleDraft {
  return {
    id: r.id,
    name: r.name,
    group: r.group ?? "",
    find: r.find,
    replace: r.replace ?? "",
    scope: r.scope ?? "",
    scopeTitle: Boolean(r.scopeTitle),
    scopeContent: Boolean(r.scopeContent),
    isRegex: Boolean(r.isRegex),
    timeoutMillisecond: r.timeoutMillisecond || 3000,
    enabled: Boolean(r.enabled),
    order: r.order || 0,
  };
}

function errText(err: unknown): string {
  return err instanceof Error ? err.message : "操作失败";
}

function downloadJson(filename: string, data: unknown): void {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/* ---------- Live purify sandbox engine ---------- */

const DEFAULT_TITLE_SAMPLE = "第一百二十章 风起云涌（求月票！）【笔趣阁首发】";
const SANDBOX_MAX_CHARS = 20000;
const SANDBOX_MAX_MATCHES = 2000;
const DRAFT_KEY = "__draft__";

type SandboxMode = "chain" | "single";
type SandboxTarget = "content" | "title";
type SandboxView = "split" | "inline" | "clean";

interface SandboxRule {
  key: string;
  name: string;
  find: string;
  replace: string;
  isRegex: boolean;
  timeoutMillisecond: number;
  isDraft: boolean;
  order: number;
}

interface RuleHit {
  key: string;
  name: string;
  isDraft: boolean;
  count: number;
  error: string | null;
  timedOut: boolean;
  capped: boolean;
}

interface Unit {
  ch: string;
  ins: boolean;
  /** inserting rule index (ins units) */
  rule: number;
  /** deleting rule index for original units, -1 = kept */
  del: number;
}

interface Piece {
  text: string;
  kind: "plain" | "del" | "ins";
  rule: number;
}

interface ChainResult {
  rules: SandboxRule[];
  hits: RuleHit[];
  before: Piece[];
  after: Piece[];
  unified: Piece[];
  cleanText: string;
  totalHits: number;
  deletedChars: number;
  insertedChars: number;
  truncated: boolean;
  draftIndex: number;
}

interface RuleMatch {
  start: number;
  end: number;
  replacement: string;
}

function expandReplacement(template: string, match: RegExpExecArray): string {
  return template.replace(/\$(\$|&|\d{1,2}|<[^>]+>)/g, (token, ref: string) => {
    if (ref === "$") return "$";
    if (ref === "&") return match[0] ?? "";
    if (ref.startsWith("<")) return match.groups?.[ref.slice(1, -1)] ?? "";
    const idx = Number(ref);
    if (idx >= 1 && idx < match.length) return match[idx] ?? "";
    return token;
  });
}

/** 将 Java/legado 常见内联标志 (?m) / (?i) / (?s) 转换为 JS RegExp 标志 */
function compileSandboxRegex(find: string): RegExp {
  let body = find;
  const flagSet = new Set<string>(["g", "m"]);
  const leading = body.match(/^\(\?([imsux-]+)\)/);
  if (leading) {
    const rawFlags = leading[1] ?? "";
    let neg = false;
    for (const ch of rawFlags) {
      if (ch === "-") {
        neg = true;
        continue;
      }
      if (ch === "i" || ch === "m" || ch === "s") {
        if (neg) flagSet.delete(ch);
        else flagSet.add(ch);
      }
    }
    body = body.slice(leading[0].length);
  }
  return new RegExp(body, Array.from(flagSet).join(""));
}

function regexError(find: string): string | null {
  try {
    compileSandboxRegex(find);
    return null;
  } catch (err) {
    return err instanceof Error ? err.message : "正则表达式语法有误";
  }
}

function findRuleMatches(
  text: string,
  rule: SandboxRule,
): { matches: RuleMatch[]; error: string | null; timedOut: boolean; capped: boolean } {
  const matches: RuleMatch[] = [];
  if (!rule.find) return { matches, error: null, timedOut: false, capped: false };
  const deadline = performance.now() + Math.max(100, rule.timeoutMillisecond || 3000);

  if (!rule.isRegex) {
    let idx = text.indexOf(rule.find);
    while (idx !== -1) {
      if (matches.length >= SANDBOX_MAX_MATCHES) return { matches, error: null, timedOut: false, capped: true };
      matches.push({ start: idx, end: idx + rule.find.length, replacement: rule.replace });
      idx = text.indexOf(rule.find, idx + rule.find.length);
    }
    return { matches, error: null, timedOut: false, capped: false };
  }

  let regex: RegExp;
  try {
    regex = compileSandboxRegex(rule.find);
  } catch (err) {
    return {
      matches,
      error: err instanceof Error ? err.message : "正则表达式语法有误",
      timedOut: false,
      capped: false,
    };
  }

  let m = regex.exec(text);
  while (m !== null) {
    if (matches.length >= SANDBOX_MAX_MATCHES) return { matches, error: null, timedOut: false, capped: true };
    if (performance.now() > deadline) return { matches, error: null, timedOut: true, capped: false };
    const matched = m[0] ?? "";
    const replacement = expandReplacement(rule.replace, m);
    if (matched.length === 0) {
      if (replacement) matches.push({ start: m.index, end: m.index, replacement });
      regex.lastIndex = m.index + 1;
      if (regex.lastIndex > text.length) break;
    } else {
      matches.push({ start: m.index, end: m.index + matched.length, replacement });
    }
    m = regex.exec(text);
  }
  return { matches, error: null, timedOut: false, capped: false };
}

function applyRuleMatches(units: Unit[], matches: RuleMatch[], ruleIdx: number): Unit[] {
  if (matches.length === 0) return units;
  const visible: number[] = [];
  for (let i = 0; i < units.length; i += 1) {
    if (units[i]!.del === -1) visible.push(i);
  }
  const removed = new Uint8Array(units.length);
  const insertBefore = new Map<number, Unit[]>();
  for (const mt of matches) {
    for (let p = mt.start; p < mt.end; p += 1) {
      const ui = visible[p];
      if (ui === undefined) continue;
      const u = units[ui]!;
      if (u.ins) removed[ui] = 1;
      else u.del = ruleIdx;
    }
    if (!mt.replacement) continue;
    const anchor =
      mt.end > mt.start
        ? (visible[mt.end - 1] ?? units.length - 1) + 1
        : (visible[mt.start] ?? units.length);
    const bucket = insertBefore.get(anchor) ?? [];
    for (let k = 0; k < mt.replacement.length; k += 1) {
      bucket.push({ ch: mt.replacement[k]!, ins: true, rule: ruleIdx, del: -1 });
    }
    insertBefore.set(anchor, bucket);
  }
  const out: Unit[] = [];
  for (let i = 0; i < units.length; i += 1) {
    const extra = insertBefore.get(i);
    if (extra) out.push(...extra);
    if (!removed[i]) out.push(units[i]!);
  }
  const tail = insertBefore.get(units.length);
  if (tail) out.push(...tail);
  return out;
}

function pushPiece(list: Piece[], ch: string, kind: Piece["kind"], rule: number): void {
  const last = list[list.length - 1];
  if (last && last.kind === kind && last.rule === rule) last.text += ch;
  else list.push({ text: ch, kind, rule });
}

function ruleInScope(scope: string | null | undefined, context: string): boolean {
  const s = (scope ?? "").trim();
  const ctx = context.trim();
  if (!s || !ctx) return true;
  if (s.includes(ctx)) return true;
  return s
    .split(/[;,，；\n]+/)
    .map((t) => t.trim())
    .filter(Boolean)
    .some((t) => ctx.includes(t));
}

function buildSandboxChain(
  rules: ReplaceRule[],
  draft: RuleDraft,
  mode: SandboxMode,
  target: SandboxTarget,
  context: string,
): { chain: SandboxRule[]; draftNote: string | null } {
  const inTarget = (title: boolean, content: boolean) => (target === "title" ? title : content);
  let draftNote: string | null = null;
  const entries: SandboxRule[] = [];

  if (!draft.find) {
    draftNote = "当前规则尚未填写查找模式";
  } else if (!inTarget(draft.scopeTitle, draft.scopeContent)) {
    draftNote = `当前规则未勾选「作用于${target === "title" ? "标题" : "正文"}」，未参与预览`;
  } else if (!ruleInScope(draft.scope, context)) {
    draftNote = "当前规则的作用范围与模拟书名/书源不匹配，未参与预览";
  } else {
    entries.push({
      key: DRAFT_KEY,
      name: draft.name.trim() || "当前编辑",
      find: draft.find,
      replace: draft.replace,
      isRegex: draft.isRegex,
      timeoutMillisecond: draft.timeoutMillisecond,
      isDraft: true,
      order: Number(draft.order) || 0,
    });
    if (!draft.enabled && mode === "chain") draftNote = "当前规则未启用，仅在沙箱中预览其效果";
  }

  if (mode === "chain") {
    for (const r of rules) {
      if (!r.enabled || (draft.id && r.id === draft.id)) continue;
      if (!inTarget(Boolean(r.scopeTitle), Boolean(r.scopeContent))) continue;
      if (!ruleInScope(r.scope, context)) continue;
      entries.push({
        key: r.id,
        name: r.name,
        find: r.find,
        replace: r.replace ?? "",
        isRegex: Boolean(r.isRegex),
        timeoutMillisecond: r.timeoutMillisecond || 3000,
        isDraft: false,
        order: r.order || 0,
      });
    }
  }
  const chain = entries
    .map((e, i) => ({ e, i }))
    .sort((a, b) => a.e.order - b.e.order || a.i - b.i)
    .map((x) => x.e);
  return { chain, draftNote };
}

function runSandboxChain(sample: string, chain: SandboxRule[]): ChainResult {
  const truncated = sample.length > SANDBOX_MAX_CHARS;
  const source = truncated ? sample.slice(0, SANDBOX_MAX_CHARS) : sample;
  // One unit per UTF-16 code unit so regex indices map 1:1 onto units.
  let units: Unit[] = [];
  for (let i = 0; i < source.length; i += 1) {
    units.push({ ch: source[i]!, ins: false, rule: -1, del: -1 });
  }

  const hits: RuleHit[] = [];
  let totalHits = 0;
  chain.forEach((rule, idx) => {
    let current = "";
    for (const u of units) if (u.del === -1) current += u.ch;
    const res = findRuleMatches(current, rule);
    units = applyRuleMatches(units, res.matches, idx);
    totalHits += res.matches.length;
    hits.push({
      key: rule.key,
      name: rule.name,
      isDraft: rule.isDraft,
      count: res.matches.length,
      error: res.error,
      timedOut: res.timedOut,
      capped: res.capped,
    });
  });

  const before: Piece[] = [];
  const after: Piece[] = [];
  const unified: Piece[] = [];
  let deletedChars = 0;
  let insertedChars = 0;
  let cleanText = "";
  for (const u of units) {
    if (u.ins) {
      insertedChars += 1;
      cleanText += u.ch;
      pushPiece(after, u.ch, "ins", u.rule);
      pushPiece(unified, u.ch, "ins", u.rule);
    } else if (u.del >= 0) {
      deletedChars += 1;
      pushPiece(before, u.ch, "del", u.del);
      pushPiece(unified, u.ch, "del", u.del);
    } else {
      cleanText += u.ch;
      pushPiece(before, u.ch, "plain", -1);
      pushPiece(after, u.ch, "plain", -1);
      pushPiece(unified, u.ch, "plain", -1);
    }
  }

  return {
    rules: chain,
    hits,
    before,
    after,
    unified,
    cleanText,
    totalHits,
    deletedChars,
    insertedChars,
    truncated,
    draftIndex: chain.findIndex((r) => r.isDraft),
  };
}

function SandboxPieces({
  pieces,
  rules,
  draftIndex,
}: {
  pieces: Piece[];
  rules: SandboxRule[];
  draftIndex: number;
}) {
  return (
    <>
      {pieces.map((p, idx) => {
        if (p.kind === "plain") return <React.Fragment key={idx}>{p.text}</React.Fragment>;
        const ruleName = rules[p.rule]?.name ?? "";
        const fromDraft = p.rule === draftIndex && draftIndex >= 0;
        return (
          <mark
            key={idx}
            title={`${p.kind === "del" ? "删除" : "替换"} · ${ruleName}`}
            className={cn(
              "rounded-xs px-0.5 [box-decoration-break:clone]",
              p.kind === "del"
                ? "bg-danger-soft text-danger line-through decoration-danger/70"
                : "bg-success-soft text-success",
              fromDraft && "ring-1 ring-accent/50",
            )}
          >
            {p.text}
          </mark>
        );
      })}
    </>
  );
}

function parseImportRules(raw: string): ReplaceRule[] {
  const parsed: unknown = JSON.parse(raw);
  let arr: unknown[];
  if (Array.isArray(parsed)) {
    arr = parsed;
  } else if (parsed && typeof parsed === "object") {
    const obj = parsed as Record<string, unknown>;
    if (Array.isArray(obj.rules)) arr = obj.rules;
    else if (Array.isArray(obj.data)) arr = obj.data;
    else arr = [parsed];
  } else {
    arr = [];
  }
  const results: ReplaceRule[] = [];
  for (const item of arr) {
    if (!item || typeof item !== "object") continue;
    const obj = item as Record<string, unknown>;
    const name = String(obj.name ?? "").trim();
    const find = String(obj.find ?? obj.pattern ?? "");
    if (!name || !find) continue;
    results.push({
      id: String(obj.id ?? ""),
      name,
      group: obj.group ? String(obj.group) : null,
      find,
      replace: String(obj.replace ?? obj.replacement ?? ""),
      scope: obj.scope ? String(obj.scope) : null,
      scopeTitle: Boolean(obj.scopeTitle),
      scopeContent: obj.scopeContent === undefined ? true : Boolean(obj.scopeContent),
      isRegex: Boolean(obj.isRegex),
      timeoutMillisecond: Number(obj.timeoutMillisecond) || 3000,
      enabled: obj.enabled === undefined ? Boolean(obj.isEnabled ?? true) : Boolean(obj.enabled),
      order: Number(obj.order) || 0,
    });
  }
  return results;
}

export default function PurifyWorkspace() {
  const qc = useQueryClient();
  const [isDesktop, setIsDesktop] = React.useState(
    () => typeof window !== "undefined" && window.innerWidth >= 1024,
  );

  React.useEffect(() => {
    const mq = window.matchMedia("(min-width: 1024px)");
    const update = () => setIsDesktop(mq.matches);
    update();
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, []);

  const [keyword, setKeyword] = React.useState("");
  const [activeGroup, setActiveGroup] = React.useState<string>("__ALL__");
  const [multiSelect, setMultiSelect] = React.useState(false);
  const [selectedIds, setSelectedIds] = React.useState<Set<string>>(() => new Set());

  const [draft, setDraft] = React.useState<RuleDraft>({ ...EMPTY_DRAFT });
  const [mobileEditorOpen, setMobileEditorOpen] = React.useState(false);
  const [sampleText, setSampleText] = React.useState(DEFAULT_SAMPLE_TEXT);
  const [sandboxView, setSandboxView] = React.useState<SandboxView>("split");
  const [sandboxMode, setSandboxMode] = React.useState<SandboxMode>("chain");
  const [sandboxTarget, setSandboxTarget] = React.useState<SandboxTarget>("content");
  const [sandboxContext, setSandboxContext] = React.useState("");
  const [showZeroHits, setShowZeroHits] = React.useState(false);

  // Import Sheet state
  const [importOpen, setImportOpen] = React.useState(false);
  const [importRaw, setImportRaw] = React.useState("");
  const importFileRef = React.useRef<HTMLInputElement>(null);

  const rulesQuery = useQuery({
    queryKey: ["studio", "replaceRules"],
    queryFn: () => featuresApi.getReplaceRules(),
  });
  const rules = React.useMemo(() => rulesQuery.data ?? [], [rulesQuery.data]);

  const groups = React.useMemo(() => {
    const map = new Map<string, number>();
    let ungrouped = 0;
    for (const r of rules) {
      const g = (r.group ?? "").trim();
      if (!g) {
        ungrouped += 1;
      } else {
        map.set(g, (map.get(g) ?? 0) + 1);
      }
    }
    return {
      list: Array.from(map.entries()).sort((a, b) => b[1] - a[1]),
      ungrouped,
    };
  }, [rules]);

  const filteredRules = React.useMemo(() => {
    const q = keyword.trim().toLowerCase();
    return rules.filter((r) => {
      const g = (r.group ?? "").trim();
      if (activeGroup === "__UNGROUPED__" && g) return false;
      if (activeGroup !== "__ALL__" && activeGroup !== "__UNGROUPED__" && g !== activeGroup) {
        return false;
      }
      if (!q) return true;
      return (
        r.name.toLowerCase().includes(q) ||
        g.toLowerCase().includes(q) ||
        r.find.toLowerCase().includes(q) ||
        (r.replace ?? "").toLowerCase().includes(q)
      );
    });
  }, [rules, keyword, activeGroup]);

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: ["studio", "replaceRules"] });
    void qc.invalidateQueries({ queryKey: ["chapter"] });
    void qc.invalidateQueries({ queryKey: ["toc"] });
  };

  const saveRuleMutation = useMutation({
    mutationFn: async (d: RuleDraft) => {
      if (!d.name.trim()) throw new Error("请填写规则名称");
      if (!d.find) throw new Error("请填写查找模式");
      if (d.isRegex) {
        try {
          new RegExp(d.find);
        } catch {
          throw new Error("正则表达式语法无效，请检查后保存");
        }
      }
      return featuresApi.saveReplaceRule({
        id: d.id || undefined,
        name: d.name.trim(),
        group: d.group.trim() || null,
        find: d.find,
        replace: d.replace,
        scope: d.scope.trim() || null,
        scopeTitle: d.scopeTitle,
        scopeContent: d.scopeContent,
        isRegex: d.isRegex,
        timeoutMillisecond: Math.max(100, Number(d.timeoutMillisecond) || 3000),
        enabled: d.enabled,
        order: Number(d.order) || 0,
      });
    },
    onSuccess: (res, vars) => {
      notify.success(`已保存净化规则「${vars.name.trim()}」`);
      if (res?.id && !vars.id) {
        setDraft((prev) => ({ ...prev, id: res.id }));
      }
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const toggleRuleMutation = useMutation({
    mutationFn: ({ rule, enabled }: { rule: ReplaceRule; enabled: boolean }) =>
      featuresApi.saveReplaceRule({ ...rule, enabled }),
    onSuccess: (_res, vars) => {
      if (draft.id === vars.rule.id) {
        setDraft((prev) => ({ ...prev, enabled: vars.enabled }));
      }
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const deleteRuleMutation = useMutation({
    mutationFn: (id: string) => featuresApi.deleteReplaceRule(id),
    onSuccess: (_res, id) => {
      notify.success("已删除净化规则");
      if (draft.id === id) {
        setDraft({ ...EMPTY_DRAFT });
        setMobileEditorOpen(false);
      }
      setSelectedIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const batchDeleteMutation = useMutation({
    mutationFn: (ids: string[]) => featuresApi.deleteReplaceRules({ ids }),
    onSuccess: (res) => {
      notify.success(`已批量删除 ${res.count} 条净化规则`);
      setSelectedIds(new Set());
      setMultiSelect(false);
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const importMutation = useMutation({
    mutationFn: (list: ReplaceRule[]) => featuresApi.saveReplaceRules(list),
    onSuccess: (res) => {
      notify.success(`成功导入 ${res.count} 条净化规则`);
      setImportOpen(false);
      setImportRaw("");
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const parsedImport = React.useMemo(() => {
    const trimmed = importRaw.trim();
    if (!trimmed) return { rules: [] as ReplaceRule[], error: null as string | null };
    try {
      const list = parseImportRules(trimmed);
      if (list.length === 0) {
        return { rules: [], error: "未解析到有效的规则条目（需包含 name 与 find/pattern）" };
      }
      return { rules: list, error: null };
    } catch (err) {
      return {
        rules: [],
        error: err instanceof Error ? err.message : "JSON 语法错误",
      };
    }
  }, [importRaw]);

  const draftRegexError = React.useMemo(
    () => (draft.isRegex && draft.find ? regexError(draft.find) : null),
    [draft.isRegex, draft.find],
  );

  // Defer heavy chain evaluation so typing in the editor / sample stays fluid.
  const deferredSample = React.useDeferredValue(sampleText);
  const deferredDraft = React.useDeferredValue(draft);
  const sandboxChain = React.useMemo(
    () => buildSandboxChain(rules, deferredDraft, sandboxMode, sandboxTarget, sandboxContext),
    [rules, deferredDraft, sandboxMode, sandboxTarget, sandboxContext],
  );
  const sandbox = React.useMemo(
    () => runSandboxChain(deferredSample, sandboxChain.chain),
    [deferredSample, sandboxChain],
  );
  const sandboxStale = deferredSample !== sampleText || deferredDraft !== draft;
  const draftHit = sandbox.hits.find((h) => h.isDraft) ?? null;
  const visibleHits = showZeroHits ? sandbox.hits : sandbox.hits.filter((h) => h.count > 0 || h.error || h.isDraft);
  const ruleErrors = sandbox.hits.filter((h) => h.error || h.timedOut).length;
  const hitByRuleId = React.useMemo(() => {
    const map = new Map<string, RuleHit>();
    for (const h of sandbox.hits) {
      if (h.isDraft) {
        if (deferredDraft.id) map.set(deferredDraft.id, h);
      } else {
        map.set(h.key, h);
      }
    }
    return map;
  }, [sandbox.hits, deferredDraft.id]);

  const switchSandboxTarget = (next: SandboxTarget) => {
    setSandboxTarget(next);
    setSampleText((prev) => {
      if (next === "title" && prev === DEFAULT_SAMPLE_TEXT) return DEFAULT_TITLE_SAMPLE;
      if (next === "content" && prev === DEFAULT_TITLE_SAMPLE) return DEFAULT_SAMPLE_TEXT;
      return prev;
    });
  };

  const copyCleanText = async () => {
    try {
      await navigator.clipboard.writeText(sandbox.cleanText);
      notify.success("已复制净化结果");
    } catch {
      notify.error("复制失败，请手动选择文本");
    }
  };

  const openNewRule = () => {
    setDraft({ ...EMPTY_DRAFT });
    if (!isDesktop) setMobileEditorOpen(true);
  };

  const selectRule = (rule: ReplaceRule) => {
    setDraft(toDraft(rule));
    if (!isDesktop) setMobileEditorOpen(true);
  };

  const toggleSelectId = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const editorBody = (
    <div className="flex flex-col gap-5">
      {/* Rule fields */}
      <div className="flex flex-col gap-3.5">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Input
            label="规则名称 *"
            placeholder="如：去除网站广告水印"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
          <Input
            label="分组"
            placeholder="如：通用广告"
            value={draft.group}
            onChange={(e) => setDraft({ ...draft, group: e.target.value })}
          />
        </div>

        <Textarea
          label={draft.isRegex ? "查找正则 *" : "查找文本 *"}
          mono
          rows={2}
          placeholder={
            draft.isRegex
              ? "例如：【[^】]*(?:笔趣阁|www\\.)[^】]*】"
              : "输入要精确匹配的文本片段"
          }
          value={draft.find}
          onChange={(e) => setDraft({ ...draft, find: e.target.value })}
        />

        {draftRegexError ? (
          <div className="flex items-center gap-2 rounded-xl border border-danger/30 bg-danger-soft/60 px-3 py-2 text-xs text-danger">
            <AlertTriangle className="size-3.5 shrink-0" />
            <span className="truncate font-mono" title={draftRegexError}>
              {draftRegexError}
            </span>
          </div>
        ) : draft.find && draftHit ? (
          <p className="-mt-1.5 text-[11px] text-ink-muted tabular-nums">
            {draftHit.timedOut
              ? "匹配超时，表达式可能存在灾难性回溯"
              : `示例文本中命中 ${draftHit.count} 处${draftHit.capped ? "（已达上限）" : ""}`}
          </p>
        ) : null}

        <Textarea
          label="替换为（留空表示删除命中文本）"
          mono
          rows={2}
          placeholder="支持 $1、$2 捕获组引用，留空即删除"
          value={draft.replace}
          onChange={(e) => setDraft({ ...draft, replace: e.target.value })}
        />

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Input
            label="作用范围（可选）"
            placeholder="留空作用于全部"
            value={draft.scope}
            onChange={(e) => setDraft({ ...draft, scope: e.target.value })}
            className="sm:col-span-2"
          />
          <Input
            label="超时 (ms)"
            type="number"
            min={100}
            value={String(draft.timeoutMillisecond)}
            onChange={(e) =>
              setDraft({ ...draft, timeoutMillisecond: Number(e.target.value) || 3000 })
            }
          />
        </div>

        <div className="grid grid-cols-2 gap-2 rounded-2xl border border-line bg-subtle/40 p-3">
          <label className="flex items-center justify-between gap-2 text-xs text-ink">
            <span>使用正则</span>
            <Switch checked={draft.isRegex} onChange={(v) => setDraft({ ...draft, isRegex: v })} />
          </label>
          <label className="flex items-center justify-between gap-2 text-xs text-ink">
            <span>启用规则</span>
            <Switch checked={draft.enabled} onChange={(v) => setDraft({ ...draft, enabled: v })} />
          </label>
          <label className="flex items-center justify-between gap-2 text-xs text-ink">
            <span>作用于正文</span>
            <Switch
              checked={draft.scopeContent}
              onChange={(v) => setDraft({ ...draft, scopeContent: v })}
            />
          </label>
          <label className="flex items-center justify-between gap-2 text-xs text-ink">
            <span>作用于标题</span>
            <Switch
              checked={draft.scopeTitle}
              onChange={(v) => setDraft({ ...draft, scopeTitle: v })}
            />
          </label>
        </div>
      </div>

      {/* Live Purify Preview Sandbox */}
      <div className="flex flex-col gap-3 rounded-2xl border border-line bg-subtle/35 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <Wand2 className="size-4 shrink-0 text-accent" />
            <span className="font-serif text-sm font-semibold text-ink">实时净化预览</span>
            <Badge
              tone={
                draftRegexError || ruleErrors > 0
                  ? "danger"
                  : sandbox.totalHits > 0
                    ? "success"
                    : "default"
              }
            >
              {draftRegexError ? "语法有误" : `命中 ${sandbox.totalHits} 处`}
            </Badge>
            {sandboxStale ? <Spinner /> : null}
          </div>
          <div className="flex items-center gap-1.5">
            <Segmented<SandboxMode>
              layoutId="v2-purify-sandbox-mode"
              size="sm"
              value={sandboxMode}
              onChange={setSandboxMode}
              options={[
                { value: "chain", label: "全部启用规则" },
                { value: "single", label: "仅当前规则" },
              ]}
            />
            <IconButton
              size="sm"
              aria-label="重置示例文本"
              onClick={() =>
                setSampleText(sandboxTarget === "title" ? DEFAULT_TITLE_SAMPLE : DEFAULT_SAMPLE_TEXT)
              }
            >
              <RotateCcw className="size-3.5" />
            </IconButton>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Segmented<SandboxTarget>
            layoutId="v2-purify-sandbox-target"
            size="sm"
            value={sandboxTarget}
            onChange={switchSandboxTarget}
            options={[
              { value: "content", label: "模拟正文" },
              { value: "title", label: "模拟标题" },
            ]}
          />
          <Input
            size="sm"
            value={sandboxContext}
            onChange={(e) => setSandboxContext(e.target.value)}
            clearable
            onClear={() => setSandboxContext("")}
            placeholder="模拟书名 / 书源 URL（留空忽略作用范围）"
            className="min-w-[180px] flex-1"
          />
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-[11px] font-medium text-ink-secondary">测试示例文本</span>
          <div className="flex flex-wrap items-center gap-1">
            <button
              type="button"
              onClick={() => {
                setSandboxTarget("content");
                setSampleText(DEFAULT_SAMPLE_TEXT);
              }}
              className="cursor-pointer rounded-full border border-line bg-elevated px-2 py-0.5 text-[10px] text-ink-secondary transition-colors hover:border-accent/50 hover:text-ink"
            >
              水印求票
            </button>
            <button
              type="button"
              onClick={() => {
                setSandboxTarget("content");
                setSampleText(ANTI_SCRAPE_SAMPLE_TEXT);
              }}
              className="cursor-pointer rounded-full border border-line bg-elevated px-2 py-0.5 text-[10px] text-ink-secondary transition-colors hover:border-accent/50 hover:text-ink"
            >
              域名推广
            </button>
            <button
              type="button"
              onClick={() => {
                setSandboxTarget("title");
                setSampleText(DEFAULT_TITLE_SAMPLE);
              }}
              className="cursor-pointer rounded-full border border-line bg-elevated px-2 py-0.5 text-[10px] text-ink-secondary transition-colors hover:border-accent/50 hover:text-ink"
            >
              章节标题
            </button>
          </div>
        </div>

        <Textarea
          rows={sandboxTarget === "title" ? 2 : 4}
          placeholder="粘贴一段带广告水印的小说文本，即时查看净化效果…"
          value={sampleText}
          onChange={(e) => setSampleText(e.target.value)}
        />

        {sandboxChain.draftNote ? (
          <p className="flex items-center gap-1.5 rounded-lg bg-warn-soft/60 px-2.5 py-1.5 text-[11px] text-warn">
            <AlertTriangle className="size-3 shrink-0" />
            {sandboxChain.draftNote}
          </p>
        ) : null}
        {sandbox.truncated ? (
          <p className="text-[11px] text-ink-muted tabular-nums">
            示例文本过长，仅预览前 {SANDBOX_MAX_CHARS} 字
          </p>
        ) : null}

        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ink-muted tabular-nums">
            <span>
              规则 <b className="font-semibold text-ink">{sandbox.hits.filter((h) => h.count > 0).length}</b>/
              {sandbox.rules.length} 生效
            </span>
            <span>
              删除 <b className="font-semibold text-danger">{sandbox.deletedChars}</b> 字
            </span>
            <span>
              新增 <b className="font-semibold text-success">{sandbox.insertedChars}</b> 字
            </span>
            <span>
              {deferredSample.length} → {sandbox.cleanText.length} 字
            </span>
          </div>
          <div className="flex items-center gap-1">
            <Segmented<SandboxView>
              layoutId="v2-purify-sandbox-view"
              size="sm"
              value={sandboxView}
              onChange={setSandboxView}
              options={[
                { value: "split", label: "并排" },
                { value: "inline", label: "差异" },
                { value: "clean", label: "纯净" },
              ]}
            />
            <IconButton size="sm" aria-label="复制净化结果" onClick={() => void copyCleanText()}>
              <Copy className="size-3.5" />
            </IconButton>
          </div>
        </div>

        <AnimatePresence mode="wait" initial={false}>
          <motion.div
            key={sandboxView}
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -4 }}
            transition={{ duration: 0.14 }}
          >
            {sandboxView === "split" ? (
              <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
                <div className="rounded-xl border border-line bg-elevated p-3">
                  <p className="mb-1.5 text-[11px] font-medium text-ink-muted">净化前（删除内容标红）</p>
                  <div className="max-h-52 overflow-y-auto whitespace-pre-wrap break-words font-serif text-xs leading-relaxed text-ink">
                    <SandboxPieces pieces={sandbox.before} rules={sandbox.rules} draftIndex={sandbox.draftIndex} />
                  </div>
                </div>
                <div className="rounded-xl border border-line bg-elevated p-3">
                  <p className="mb-1.5 text-[11px] font-medium text-ink-muted">净化后（替换内容标绿）</p>
                  <div className="max-h-52 overflow-y-auto whitespace-pre-wrap break-words font-serif text-xs leading-relaxed text-ink">
                    {sandbox.after.length > 0 ? (
                      <SandboxPieces pieces={sandbox.after} rules={sandbox.rules} draftIndex={sandbox.draftIndex} />
                    ) : (
                      <span className="text-ink-muted">（结果为空）</span>
                    )}
                  </div>
                </div>
              </div>
            ) : sandboxView === "inline" ? (
              <div className="rounded-xl border border-line bg-elevated p-3">
                <p className="mb-1.5 text-[11px] font-medium text-ink-muted">
                  差异高亮 · <span className="text-danger">删除</span> /{" "}
                  <span className="text-success">新增</span>
                  {sandbox.draftIndex >= 0 ? " · 描边为当前编辑规则" : ""}
                </p>
                <div className="max-h-60 overflow-y-auto whitespace-pre-wrap break-words font-serif text-[13px] leading-relaxed text-ink">
                  <SandboxPieces pieces={sandbox.unified} rules={sandbox.rules} draftIndex={sandbox.draftIndex} />
                </div>
              </div>
            ) : (
              <div className="rounded-xl border border-line bg-elevated p-3.5">
                <p className="mb-1.5 text-[11px] font-medium text-ink-muted">纯净正文效果</p>
                <div className="max-h-60 overflow-y-auto whitespace-pre-wrap break-words font-serif text-[13px] leading-relaxed text-ink">
                  {sandbox.cleanText || <span className="text-ink-muted">（结果为空）</span>}
                </div>
              </div>
            )}
          </motion.div>
        </AnimatePresence>

        {sandbox.hits.length > 0 ? (
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center justify-between">
              <span className="text-[11px] font-medium text-ink-secondary">
                每条规则命中（按执行顺序）
              </span>
              {sandbox.hits.some((h) => h.count === 0 && !h.error && !h.isDraft) ? (
                <button
                  type="button"
                  onClick={() => setShowZeroHits((v) => !v)}
                  className="cursor-pointer text-[11px] text-accent hover:underline"
                >
                  {showZeroHits
                    ? "隐藏未命中"
                    : `显示未命中 ${sandbox.hits.filter((h) => h.count === 0 && !h.error && !h.isDraft).length} 条`}
                </button>
              ) : null}
            </div>
            <div className="max-h-44 divide-y divide-line overflow-y-auto rounded-xl border border-line bg-elevated">
              {visibleHits.length === 0 ? (
                <p className="px-3 py-2 text-[11px] text-ink-muted">暂无规则命中示例文本</p>
              ) : (
                visibleHits.map((h) => {
                  const saved = h.isDraft ? null : rules.find((r) => r.id === h.key);
                  return (
                    <button
                      key={h.key}
                      type="button"
                      disabled={!saved}
                      onClick={() => saved && selectRule(saved)}
                      className={cn(
                        "flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors",
                        saved ? "cursor-pointer hover:bg-subtle/60" : "cursor-default",
                        h.isDraft && "bg-accent-soft/35",
                      )}
                    >
                      <span
                        className={cn(
                          "min-w-0 flex-1 truncate",
                          h.count > 0 ? "text-ink" : "text-ink-muted",
                        )}
                      >
                        {h.name}
                      </span>
                      {h.isDraft ? <Badge tone="accent">编辑中</Badge> : null}
                      {h.error ? (
                        <Badge tone="danger">
                          <span className="max-w-[140px] truncate" title={h.error}>
                            正则错误
                          </span>
                        </Badge>
                      ) : h.timedOut ? (
                        <Badge tone="warn">超时</Badge>
                      ) : (
                        <Badge tone={h.count > 0 ? "success" : "default"}>
                          {h.count}
                          {h.capped ? "+" : ""}
                        </Badge>
                      )}
                    </button>
                  );
                })
              )}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );

  const editorFooter = (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div>
        {draft.id ? (
          <InlineConfirm
            confirmLabel="确认删除"
            busy={deleteRuleMutation.isPending}
            onConfirm={() => deleteRuleMutation.mutateAsync(draft.id).then(() => undefined)}
            trigger={(open) => (
              <Button variant="danger" size="sm" onClick={open}>
                <Trash2 className="size-3.5" />
                删除
              </Button>
            )}
          />
        ) : null}
      </div>
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" onClick={openNewRule}>
          新建空白
        </Button>
        <Button
          variant="primary"
          size="sm"
          loading={saveRuleMutation.isPending}
          onClick={() => saveRuleMutation.mutate(draft)}
        >
          保存规则
        </Button>
      </div>
    </div>
  );

  return (
    <div className="flex flex-col gap-4">
      {/* Compact top action strip */}
      <div className="flex flex-col gap-2.5 lg:flex-row lg:items-center lg:justify-between">
        <Input
          size="sm"
          value={keyword}
          onChange={(e) => setKeyword(e.target.value)}
          clearable
          onClear={() => setKeyword("")}
          leftIcon={<Search className="size-3.5" />}
          placeholder="搜索规则名、分组或表达式…"
          className="w-full lg:max-w-xs"
        />

        <div className="flex flex-wrap items-center justify-between gap-1 md:justify-end md:gap-1.5">
          <button
            type="button"
            onClick={() => {
              setMultiSelect((v) => !v);
              setSelectedIds(new Set());
            }}
            className={cn(
              "inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium transition-colors md:rounded-full md:px-3",
              multiSelect
                ? "bg-accent-soft text-accent"
                : "text-ink-secondary hover:bg-subtle hover:text-ink",
            )}
          >
            <CheckSquare className="size-3.5" />
            <span>多选</span>
          </button>
          <button
            type="button"
            onClick={() => setImportOpen(true)}
            className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-3"
          >
            <Upload className="size-3.5" />
            <span>导入</span>
            <span className="hidden md:inline">JSON</span>
          </button>
          <button
            type="button"
            disabled={rules.length === 0}
            onClick={() => downloadJson("replace-rules.json", filteredRules)}
            className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:opacity-45 md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-3"
          >
            <Download className="size-3.5" />
            <span>导出</span>
          </button>
          <button
            type="button"
            aria-label="刷新规则列表"
            onClick={() => void rulesQuery.refetch()}
            className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink md:size-8 md:justify-center md:rounded-full md:px-0"
          >
            <RefreshCw className={cn("size-3.5", rulesQuery.isFetching && "animate-spin")} />
            <span className="md:hidden">刷新</span>
          </button>
          <button
            type="button"
            onClick={openNewRule}
            className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-semibold text-accent transition-colors hover:bg-accent-soft md:rounded-full md:bg-accent md:px-3.5 md:font-medium md:text-accent-ink md:hover:brightness-110"
          >
            <Plus className="size-3.5" />
            <span>新建</span>
            <span className="hidden md:inline">规则</span>
          </button>
        </div>
      </div>

      {/* Group tabs (underline on mobile, pills on desktop) */}
      <div className="flex items-center gap-3 overflow-x-auto pb-0.5 no-scrollbar md:gap-1.5">
        <button
          type="button"
          onClick={() => setActiveGroup("__ALL__")}
          className={cn(
            "shrink-0 cursor-pointer border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:rounded-full md:border md:px-3",
            activeGroup === "__ALL__"
              ? "border-accent text-accent md:bg-accent-soft"
              : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
          )}
        >
          全部 {rules.length}
        </button>
        {groups.ungrouped > 0 ? (
          <button
            type="button"
            onClick={() => setActiveGroup("__UNGROUPED__")}
            className={cn(
              "shrink-0 cursor-pointer border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:rounded-full md:border md:px-3",
              activeGroup === "__UNGROUPED__"
                ? "border-accent text-accent md:bg-accent-soft"
                : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
            )}
          >
            未分组 {groups.ungrouped}
          </button>
        ) : null}
        {groups.list.map(([group, count]) => (
          <button
            key={group}
            type="button"
            onClick={() => setActiveGroup(group)}
            className={cn(
              "shrink-0 cursor-pointer border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:rounded-full md:border md:px-3",
              activeGroup === group
                ? "border-accent text-accent md:bg-accent-soft"
                : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
            )}
          >
            {group} {count}
          </button>
        ))}
      </div>

      {/* Master-Detail Split */}
      <div className="flex flex-col gap-5 lg:flex-row lg:items-start">
        {/* Left List */}
        <div className="min-w-0 flex-1">
          {rulesQuery.isLoading ? (
            <div className="flex justify-center py-16 md:rounded-2xl md:border md:border-line md:bg-elevated">
              <Spinner label="加载净化规则…" />
            </div>
          ) : filteredRules.length === 0 ? (
            <div className="md:rounded-2xl md:border md:border-line md:bg-elevated">
              <EmptyView
                icon={<Sparkles className="size-5" />}
                title={keyword ? "没有匹配的净化规则" : "尚未添加正文净化规则"}
                description="编写规则以自动清除章节中的站点水印、求票广告或乱码字符"
                action={
                  <Button variant="primary" size="sm" onClick={openNewRule}>
                    <Plus className="size-3.5" />
                    新建第一条规则
                  </Button>
                }
              />
            </div>
          ) : (
            <div className="divide-y divide-line/70 border-y border-line/70 md:divide-line md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated">
              {filteredRules.map((rule) => {
                const isSelected = draft.id === rule.id;
                const isChecked = selectedIds.has(rule.id);
                const ruleHit = hitByRuleId.get(rule.id);
                return (
                  <div
                    key={rule.id}
                    onClick={() =>
                      multiSelect ? toggleSelectId(rule.id) : selectRule(rule)
                    }
                    className={cn(
                      "relative flex cursor-pointer items-center gap-3 px-1 py-3 transition-colors md:px-4",
                      isSelected && !multiSelect
                        ? "bg-accent-soft/45"
                        : isChecked
                          ? "bg-accent-soft/30"
                          : "hover:bg-subtle/50",
                    )}
                  >
                    {isSelected && !multiSelect ? (
                      <span
                        aria-hidden
                        className="pointer-events-none absolute inset-y-1.5 left-0 w-[3px] rounded-r-full bg-accent"
                      />
                    ) : null}

                    {multiSelect ? (
                      <span className="shrink-0 text-accent">
                        {isChecked ? (
                          <CheckSquare className="size-4" />
                        ) : (
                          <Square className="size-4 text-ink-muted" />
                        )}
                      </span>
                    ) : null}

                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span
                          className={cn(
                            "truncate text-[13px] font-medium",
                            rule.enabled ? "text-ink" : "text-ink-muted line-through",
                          )}
                        >
                          {rule.name}
                        </span>
                        {rule.group ? <Badge>{rule.group}</Badge> : null}
                        {rule.isRegex ? <Badge tone="accent">正则</Badge> : null}
                        {rule.scopeContent ? <Badge>正文</Badge> : null}
                        {rule.scopeTitle ? <Badge>标题</Badge> : null}
                        {ruleHit?.error ? (
                          <Badge tone="danger">语法有误</Badge>
                        ) : ruleHit && ruleHit.count > 0 ? (
                          <Badge tone="success">命中 {ruleHit.count}</Badge>
                        ) : null}
                      </div>
                      <p className="mt-1 truncate font-mono text-[11px] text-ink-muted">
                        <span>{rule.find}</span>
                        <span className="mx-1.5 text-ink-muted/60">→</span>
                        <span className={rule.replace ? "text-success" : "italic opacity-75"}>
                          {rule.replace || "删除匹配"}
                        </span>
                      </p>
                    </div>

                    <div className="shrink-0">
                      <Switch
                        checked={rule.enabled}
                        ariaLabel={`切换规则 ${rule.name}`}
                        onChange={(enabled) => toggleRuleMutation.mutate({ rule, enabled })}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Desktop Sticky Editor + Live Sandbox */}
        <aside className="hidden lg:sticky lg:top-20 lg:flex lg:max-h-[calc(100dvh-6.5rem)] lg:w-[460px] lg:shrink-0 lg:flex-col lg:overflow-hidden lg:rounded-2xl lg:border lg:border-line lg:bg-elevated lg:shadow-[var(--shadow-sm)] xl:w-[540px]">
          <header className="flex shrink-0 items-center justify-between border-b border-line px-5 py-3.5">
            <div className="min-w-0">
              <h2 className="truncate font-serif text-base font-semibold text-ink">
                {draft.id ? `编辑规则 · ${draft.name || "未命名"}` : "新建净化规则"}
              </h2>
              <p className="mt-0.5 text-[11px] text-ink-muted">
                实时在下方沙箱验证替换效果，满意后一键保存
              </p>
            </div>
            {draft.id ? <Badge tone="accent">已保存</Badge> : <Badge>草稿</Badge>}
          </header>
          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
            {isDesktop ? editorBody : null}
          </div>
          <footer className="shrink-0 border-t border-line bg-subtle/30 px-5 py-3">
            {editorFooter}
          </footer>
        </aside>
      </div>

      {/* Mobile Editor Sheet */}
      <Sheet
        open={!isDesktop && mobileEditorOpen}
        onClose={() => setMobileEditorOpen(false)}
        size="lg"
        title={draft.id ? `编辑规则 · ${draft.name || "未命名"}` : "新建净化规则"}
        subtitle="内置实时净化预览沙盒"
        footer={editorFooter}
      >
        {isDesktop ? null : editorBody}
      </Sheet>

      {/* Import JSON Sheet */}
      <Sheet
        open={importOpen}
        onClose={() => setImportOpen(false)}
        size="md"
        title="导入净化规则"
        subtitle="支持 Legado / Reader3 标准 JSON 数组或单条规则对象"
        footer={
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-ink-muted tabular-nums">
              {parsedImport.rules.length > 0
                ? `已解析 ${parsedImport.rules.length} 条有效规则`
                : "等待输入"}
            </span>
            <div className="flex items-center gap-2">
              <Button variant="ghost" size="sm" onClick={() => setImportOpen(false)}>
                取消
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={parsedImport.rules.length === 0}
                loading={importMutation.isPending}
                onClick={() => importMutation.mutate(parsedImport.rules)}
              >
                导入 {parsedImport.rules.length || ""} 条
              </Button>
            </div>
          </div>
        }
      >
        <div className="flex flex-col gap-4">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-ink-secondary">粘贴 JSON 或上传文件</span>
            <Button
              variant="subtle"
              size="xs"
              onClick={() => importFileRef.current?.click()}
            >
              <FileJson className="size-3.5" />
              选择 .json 文件
            </Button>
            <input
              ref={importFileRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={async (e) => {
                const file = e.target.files?.[0];
                if (!file) return;
                const text = await file.text();
                setImportRaw(text);
                e.target.value = "";
              }}
            />
          </div>

          <Textarea
            mono
            rows={10}
            placeholder='[{"name": "去水印", "find": "【.*?笔趣阁.*?】", "replace": "", "isRegex": true}]'
            value={importRaw}
            onChange={(e) => setImportRaw(e.target.value)}
          />

          {parsedImport.error ? (
            <p className="rounded-xl border border-danger/30 bg-danger-soft/50 px-3 py-2 text-xs text-danger">
              {parsedImport.error}
            </p>
          ) : parsedImport.rules.length > 0 ? (
            <div className="max-h-48 divide-y divide-line overflow-y-auto rounded-xl border border-line bg-elevated">
              {parsedImport.rules.slice(0, 30).map((r, i) => (
                <div key={`${r.name}-${i}`} className="flex items-center justify-between gap-2 px-3 py-2 text-xs">
                  <span className="truncate font-medium text-ink">{r.name}</span>
                  <span className="truncate font-mono text-[11px] text-ink-muted">{r.find}</span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      </Sheet>

      {/* Multi-select floating island */}
      <AnimatePresence>
        {multiSelect && selectedIds.size > 0 ? (
          <motion.div
            initial={{ opacity: 0, y: 24, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 20, scale: 0.96 }}
            transition={SPRING_POP}
            className="surface-glass fixed bottom-[calc(4.25rem+env(safe-area-inset-bottom,0px))] left-1/2 z-40 flex w-[calc(100vw-1.5rem)] max-w-fit -translate-x-1/2 flex-wrap items-center justify-center gap-1.5 rounded-2xl border border-line-strong/80 px-3 py-2 shadow-[var(--shadow-float)] sm:w-auto sm:flex-nowrap sm:gap-2 sm:rounded-full sm:px-4 md:bottom-5"
          >
            <span className="text-xs font-medium text-ink tabular-nums">
              已选 {selectedIds.size} 条
            </span>
            <Button
              variant="ghost"
              size="xs"
              onClick={() => {
                const picked = rules.filter((r) => selectedIds.has(r.id));
                downloadJson("replace-rules-selected.json", picked);
              }}
            >
              <Download className="size-3" />
              导出选中
            </Button>
            <InlineConfirm
              confirmLabel="确认批量删除"
              busy={batchDeleteMutation.isPending}
              onConfirm={() =>
                batchDeleteMutation
                  .mutateAsync(Array.from(selectedIds))
                  .then(() => undefined)
              }
              trigger={(open) => (
                <Button variant="danger" size="xs" onClick={open}>
                  <Trash2 className="size-3" />
                  删除选中
                </Button>
              )}
            />
            <IconButton
              size="sm"
              aria-label="取消多选"
              onClick={() => {
                setSelectedIds(new Set());
                setMultiSelect(false);
              }}
            >
              <X className="size-3.5" />
            </IconButton>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </div>
  );
}
