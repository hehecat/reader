import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowDownWideNarrow,
  Check,
  CheckSquare,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Code2,
  Cookie,
  Download,
  ExternalLink,
  FileJson,
  Globe,
  KeyRound,
  Layers,
  Link2,
  Play,
  Plus,
  RefreshCw,
  Search,
  Shield,
  SlidersHorizontal,
  Square,
  Terminal,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { resolveImageUrl } from "@/api/client";
import { discoverApi, sourcesApi } from "@/api/endpoints";
import type {
  BookSource,
  InvalidBookSource,
  SearchBook,
  SourceCookie,
  SourceLoginResult,
  SourceStat,
} from "@/api/types";
import { cn } from "@/lib/cn";
import { buildReaderUrl, formatRelativeTime, splitSourceGroups } from "@/lib/format";
import { useAuthStore } from "@/stores/auth";
import { usePrefsStore } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button, IconButton } from "@/ui/Button";
import {
  Badge,
  type BadgeTone,
  EmptyView,
  InlineConfirm,
  Segmented,
  Spinner,
  Switch,
} from "@/ui/Controls";
import { Input, Textarea } from "@/ui/Input";
import { AnimatePresence, EASE_PAPER, motion, SPRING_POP, useReducedMotion } from "@/ui/motion";
import { Sheet } from "@/ui/Sheet";

type StatusFilter = "all" | "enabled" | "invalid" | "quality";
type InspectorTab = "quick" | "json" | "test" | "auth";
type ImportMode = "paste" | "file" | "remote";

const PAGE_SIZE_KEY = "reader.sources.pageSize";
const PAGE_SIZE_OPTIONS = [20, 50, 100, 200] as const;

function buildPageNumbers(current: number, total: number): Array<number | "ellipsis"> {
  if (total <= 7) {
    return Array.from({ length: total }, (_, i) => i + 1);
  }
  const pages: Array<number | "ellipsis"> = [1];
  const left = Math.max(2, current - 1);
  const right = Math.min(total - 1, current + 1);
  if (left > 2) pages.push("ellipsis");
  for (let p = left; p <= right; p += 1) pages.push(p);
  if (right < total - 1) pages.push("ellipsis");
  pages.push(total);
  return pages;
}

const SOURCE_TYPE_LABELS: Record<number, string> = {
  0: "文本",
  1: "音频",
  2: "漫画",
  3: "文件",
  4: "视频",
};

function errText(err: unknown): string {
  return err instanceof Error ? err.message : "操作失败";
}

function safeHost(rawUrl: string): string {
  try {
    return new URL(rawUrl).host || rawUrl;
  } catch {
    return rawUrl;
  }
}

function downloadJson(filename: string, payload: unknown): void {
  const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function parseSourcesPayload(raw: string): BookSource[] {
  const parsed: unknown = JSON.parse(raw);
  let rawArr: unknown[] = [];
  if (Array.isArray(parsed)) {
    rawArr = parsed;
  } else if (parsed && typeof parsed === "object") {
    const obj = parsed as Record<string, unknown>;
    if (Array.isArray(obj.bookSourceList)) {
      rawArr = obj.bookSourceList;
    } else if (Array.isArray(obj.sources)) {
      rawArr = obj.sources;
    } else if (typeof obj.bookSourceUrl === "string") {
      rawArr = [obj];
    }
  }
  const dedup = new Map<string, BookSource>();
  for (const item of rawArr) {
    if (!item || typeof item !== "object") continue;
    const obj = item as Record<string, unknown>;
    const url = String(obj.bookSourceUrl ?? "").trim();
    if (!url) continue;
    const name = String(obj.bookSourceName ?? "").trim() || safeHost(url);
    dedup.set(url, {
      ...(obj as Partial<BookSource>),
      bookSourceUrl: url,
      bookSourceName: name,
      bookSourceType: Number(obj.bookSourceType) || 0,
      customOrder: Number(obj.customOrder) || 0,
      enabled: obj.enabled === undefined ? true : Boolean(obj.enabled),
      enabledExplore: obj.enabledExplore === undefined ? true : Boolean(obj.enabledExplore),
      lastUpdateTime: Number(obj.lastUpdateTime) || Date.now(),
      respondTime: Number(obj.respondTime) || 180000,
      weight: Number(obj.weight) || 0,
    });
  }
  return Array.from(dedup.values());
}

function isQualitySource(
  src: BookSource,
  stat: SourceStat | undefined,
  invalid: InvalidBookSource | undefined,
): boolean {
  if (!src.enabled || invalid) return false;
  if (!stat) return false;
  if (stat.confidence !== null) return stat.confidence >= 0.65;
  return stat.attempts > 0 && stat.successRate >= 0.75;
}

function scoreForSort(
  src: BookSource,
  stat: SourceStat | undefined,
  invalid: InvalidBookSource | undefined,
): number {
  if (invalid) return -2;
  if (!src.enabled) return -1;
  if (!stat) return 0;
  if (stat.confidence !== null) return stat.confidence;
  return stat.attempts > 0 ? stat.successRate * 0.6 : 0;
}

const COLLAPSED_GROUP_LIMIT = 8;

type LatencyGrade = "fast" | "medium" | "slow";

const LATENCY_FAST_MS = 800;
const LATENCY_MEDIUM_MS = 2000;

function getLatencyGrade(ms: number): LatencyGrade {
  if (ms < LATENCY_FAST_MS) return "fast";
  if (ms < LATENCY_MEDIUM_MS) return "medium";
  return "slow";
}

function formatLatency(ms: number): string {
  const rounded = Math.round(ms);
  if (rounded >= 1000) {
    return `${(rounded / 1000).toFixed(rounded >= 10000 ? 0 : 1)}s`;
  }
  return `${rounded}ms`;
}

const LATENCY_META: Record<
  LatencyGrade,
  { label: string; tone: BadgeTone; dotClass: string; textClass: string }
> = {
  fast: {
    label: "快",
    tone: "success",
    dotClass: "bg-success",
    textClass: "text-success",
  },
  medium: {
    label: "中",
    tone: "warn",
    dotClass: "bg-warn",
    textClass: "text-warn",
  },
  slow: {
    label: "慢",
    tone: "danger",
    dotClass: "bg-danger",
    textClass: "text-danger",
  },
};

function getHealthMeta(stat: SourceStat): {
  pct: number;
  tone: BadgeTone;
  failed: boolean;
  dotClass: string;
  textClass: string;
  tooltip: string;
} {
  const isConfidence = stat.confidence !== null;
  const pct = Math.round((isConfidence ? (stat.confidence ?? 0) : stat.successRate) * 100);
  const failed = stat.consecFailures >= 3 || (stat.attempts > 0 && pct === 0);
  const tone: BadgeTone = failed || pct < 45 ? "danger" : pct >= 75 ? "success" : "warn";
  const dotClass =
    tone === "success" ? "bg-success" : tone === "warn" ? "bg-warn" : "bg-danger";
  const textClass =
    tone === "success" ? "text-success" : tone === "warn" ? "text-warn" : "text-danger";

  const parts = [
    `${isConfidence ? "综合置信度" : "成功率"} ${pct}%`,
    stat.attempts > 0 ? `样本 ${stat.effectiveSamples || stat.attempts}` : null,
    stat.avgLatencyMs > 0 ? `平均延迟 ${Math.round(stat.avgLatencyMs)}ms` : null,
    stat.consecFailures > 0 ? `连续失败 ${stat.consecFailures} 次` : null,
  ].filter(Boolean);

  return { pct, tone, failed, dotClass, textClass, tooltip: parts.join(" · ") };
}

function SourceHealthBadges({
  stat,
  invalid,
  compact = false,
}: {
  stat: SourceStat | undefined;
  invalid: InvalidBookSource | undefined;
  compact?: boolean;
}) {
  if (invalid) {
    const reason = invalid.error || invalid.errorMsg || "最近一次探测已标记失效";
    if (compact) {
      return (
        <span
          title={`失败：${reason}`}
          className="inline-flex shrink-0 items-center gap-1 text-[11px] font-medium text-danger"
        >
          <AlertTriangle className="size-3 shrink-0" />
          <span>失败</span>
        </span>
      );
    }
    return (
      <span title={`失败：${reason}`} className="inline-flex shrink-0 items-center">
        <Badge tone="danger" className="font-medium">
          <AlertTriangle className="size-3 shrink-0" />
          <span>失败</span>
        </Badge>
      </span>
    );
  }

  const hasStat = Boolean(stat && (stat.confidence !== null || stat.attempts > 0));
  if (!hasStat || !stat) {
    if (compact) return null;
    return (
      <span className="inline-flex shrink-0 items-center rounded-full border border-line bg-subtle/50 px-2 py-0.5 text-[11px] leading-none text-ink-muted tabular-nums">
        未测
      </span>
    );
  }

  const health = getHealthMeta(stat);
  const hasLatency = stat.avgLatencyMs > 0;
  const latencyGrade = hasLatency ? getLatencyGrade(stat.avgLatencyMs) : null;
  const latencyMeta = latencyGrade ? LATENCY_META[latencyGrade] : null;

  if (compact) {
    return (
      <span
        title={health.tooltip}
        className="inline-flex shrink-0 items-center gap-1.5 text-[11px] tabular-nums"
      >
        <span className={cn("font-medium", health.textClass)}>
          {health.failed ? "失败" : `${health.pct}%`}
        </span>
        {latencyMeta ? (
          <span className="text-ink-muted">{formatLatency(stat.avgLatencyMs)}</span>
        ) : null}
      </span>
    );
  }

  return (
    <span title={health.tooltip} className="inline-flex shrink-0 items-center gap-1 tabular-nums">
      <Badge
        tone={health.tone}
        className={cn(
          compact ? "px-1.5 py-0.5 text-[10px]" : "min-w-[46px] justify-center",
        )}
      >
        {health.failed ? (
          <>
            <AlertTriangle className={cn("shrink-0", compact ? "size-2.5" : "size-3")} />
            <span>失败</span>
          </>
        ) : (
          <>
            <span className={cn("size-1.5 shrink-0 rounded-full", health.dotClass)} />
            <span>{health.pct}%</span>
          </>
        )}
      </Badge>
      {latencyMeta ? (
        <Badge
          tone={latencyMeta.tone}
          className={cn(
            compact ? "px-1.5 py-0.5 text-[10px]" : "min-w-[66px] justify-center",
          )}
        >
          <span>{latencyMeta.label}</span>
          <span className="opacity-65">·</span>
          <span>{formatLatency(stat.avgLatencyMs)}</span>
        </Badge>
      ) : null}
    </span>
  );
}

interface SourcesUiSnapshot {
  userKey: string;
  keyword: string;
  statusFilter: StatusFilter;
  activeGroup: string;
  groupsExpanded: boolean;
  sortByConfidence: boolean;
  inspectedUrl: string | null;
  page: number;
}

interface SourceTestSnapshot {
  tab: InspectorTab;
  testKey: string;
  testElapsed: number | null;
  testResults: SearchBook[] | null;
  testError: string | null;
}

let cachedSourcesUi: SourcesUiSnapshot | null = null;
const cachedSourceTestByUrl = new Map<string, SourceTestSnapshot>();

export default function SourcesWorkspace() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const isAdmin = useAuthStore((s) => Boolean(s.system?.isAdmin || s.user?.isAdmin));
  const currentUserKey = useAuthStore((s) => s.user?.username ?? "__local__");
  const restoredUi =
    cachedSourcesUi && cachedSourcesUi.userKey === currentUserKey ? cachedSourcesUi : null;

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

  const [keyword, setKeyword] = React.useState(() => restoredUi?.keyword ?? "");
  const [statusFilter, setStatusFilter] = React.useState<StatusFilter>(
    () => restoredUi?.statusFilter ?? "all",
  );
  const [activeGroup, setActiveGroup] = React.useState<string>(
    () => restoredUi?.activeGroup ?? "__ALL__",
  );
  const [groupsExpanded, setGroupsExpanded] = React.useState(
    () => restoredUi?.groupsExpanded ?? false,
  );
  const [sortByConfidence, setSortByConfidence] = React.useState(
    () => restoredUi?.sortByConfidence ?? false,
  );
  const [multiSelect, setMultiSelect] = React.useState(false);
  const [selectedUrls, setSelectedUrls] = React.useState<Set<string>>(() => new Set());
  const [inspectedUrl, setInspectedUrl] = React.useState<string | null>(
    () => restoredUi?.inspectedUrl ?? null,
  );
  const [importOpen, setImportOpen] = React.useState(false);
  const [page, setPage] = React.useState(() => restoredUi?.page ?? 1);
  const [pageSize, setPageSize] = React.useState<number>(() => {
    try {
      const saved = Number(localStorage.getItem(PAGE_SIZE_KEY));
      if (Number.isFinite(saved) && saved >= 10) return saved;
    } catch {
      // ignore storage access errors
    }
    return 50;
  });
  const listTopRef = React.useRef<HTMLDivElement>(null);
  const filterDidMountRef = React.useRef(false);

  React.useEffect(() => {
    cachedSourcesUi = {
      userKey: currentUserKey,
      keyword,
      statusFilter,
      activeGroup,
      groupsExpanded,
      sortByConfidence,
      inspectedUrl,
      page,
    };
  }, [
    currentUserKey,
    keyword,
    statusFilter,
    activeGroup,
    groupsExpanded,
    sortByConfidence,
    inspectedUrl,
    page,
  ]);

  // Parallel data queries
  const sourcesQuery = useQuery({
    queryKey: ["studio", "sources"],
    queryFn: () => sourcesApi.list(),
  });
  const statsQuery = useQuery({
    queryKey: ["studio", "sourceStats"],
    queryFn: () => sourcesApi.stats(),
  });
  const invalidsQuery = useQuery({
    queryKey: ["studio", "invalidSources"],
    queryFn: () => sourcesApi.invalids(),
  });
  const cookiesQuery = useQuery({
    queryKey: ["studio", "sourceCookies"],
    queryFn: () => sourcesApi.getCookies(),
  });

  const sources = React.useMemo(() => sourcesQuery.data ?? [], [sourcesQuery.data]);

  const statMap = React.useMemo(() => {
    const map = new Map<string, SourceStat>();
    for (const s of statsQuery.data ?? []) map.set(s.sourceUrl, s);
    return map;
  }, [statsQuery.data]);

  const invalidMap = React.useMemo(() => {
    const map = new Map<string, InvalidBookSource>();
    for (const inv of invalidsQuery.data ?? []) {
      map.set(inv.sourceUrl || inv.bookSourceUrl || "", inv);
    }
    return map;
  }, [invalidsQuery.data]);

  const cookieMap = React.useMemo(() => {
    const map = new Map<string, SourceCookie>();
    for (const c of cookiesQuery.data ?? []) map.set(c.sourceUrl, c);
    return map;
  }, [cookiesQuery.data]);

  const existingUrlSet = React.useMemo(
    () => new Set(sources.map((s) => s.bookSourceUrl)),
    [sources],
  );

  // Counts for status segmented control
  const statusCounts = React.useMemo(() => {
    let enabled = 0;
    let invalid = 0;
    let quality = 0;
    for (const src of sources) {
      const inv = invalidMap.get(src.bookSourceUrl);
      const stat = statMap.get(src.bookSourceUrl);
      if (src.enabled) enabled += 1;
      if (inv) invalid += 1;
      if (isQualitySource(src, stat, inv)) quality += 1;
    }
    return { all: sources.length, enabled, invalid, quality };
  }, [sources, invalidMap, statMap]);

  // Groups parsed with splitSourceGroups
  const groupsMeta = React.useMemo(() => {
    const counts = new Map<string, number>();
    let ungrouped = 0;
    for (const src of sources) {
      const gs = splitSourceGroups(src.bookSourceGroup);
      if (gs.length === 0) {
        ungrouped += 1;
      } else {
        for (const g of new Set(gs)) {
          counts.set(g, (counts.get(g) ?? 0) + 1);
        }
      }
    }
    return {
      list: Array.from(counts.entries()).sort((a, b) => b[1] - a[1]),
      ungrouped,
    };
  }, [sources]);

  const { visibleGroups, hiddenGroupCount, canCollapseGroups } = React.useMemo(() => {
    const all = groupsMeta.list;
    const collapsible = all.length > COLLAPSED_GROUP_LIMIT;
    if (groupsExpanded || !collapsible) {
      return { visibleGroups: all, hiddenGroupCount: 0, canCollapseGroups: collapsible };
    }
    const base = all.slice(0, COLLAPSED_GROUP_LIMIT);
    const activeOutside =
      activeGroup !== "__ALL__" &&
      activeGroup !== "__UNGROUPED__" &&
      !base.some(([g]) => g === activeGroup)
        ? all.find(([g]) => g === activeGroup) ?? null
        : null;
    const shown = activeOutside ? [...base, activeOutside] : base;
    return {
      visibleGroups: shown,
      hiddenGroupCount: Math.max(0, all.length - shown.length),
      canCollapseGroups: collapsible,
    };
  }, [groupsMeta.list, groupsExpanded, activeGroup]);

  // Filtered & sorted list
  const filteredSources = React.useMemo(() => {
    const q = keyword.trim().toLowerCase();
    const list = sources.filter((src) => {
      const inv = invalidMap.get(src.bookSourceUrl);
      const stat = statMap.get(src.bookSourceUrl);

      if (statusFilter === "enabled" && !src.enabled) return false;
      if (statusFilter === "invalid" && !inv) return false;
      if (statusFilter === "quality" && !isQualitySource(src, stat, inv)) return false;

      if (activeGroup !== "__ALL__") {
        const gs = splitSourceGroups(src.bookSourceGroup);
        if (activeGroup === "__UNGROUPED__" && gs.length > 0) return false;
        if (activeGroup !== "__UNGROUPED__" && !gs.includes(activeGroup)) return false;
      }

      if (!q) return true;
      return (
        src.bookSourceName.toLowerCase().includes(q) ||
        src.bookSourceUrl.toLowerCase().includes(q) ||
        (src.bookSourceGroup ?? "").toLowerCase().includes(q) ||
        (src.bookSourceComment ?? "").toLowerCase().includes(q)
      );
    });

    if (sortByConfidence) {
      return [...list].sort((a, b) => {
        const sa = scoreForSort(a, statMap.get(a.bookSourceUrl), invalidMap.get(a.bookSourceUrl));
        const sb = scoreForSort(b, statMap.get(b.bookSourceUrl), invalidMap.get(b.bookSourceUrl));
        if (sb !== sa) return sb - sa;
        const la = statMap.get(a.bookSourceUrl)?.avgLatencyMs ?? 99999;
        const lb = statMap.get(b.bookSourceUrl)?.avgLatencyMs ?? 99999;
        return la - lb;
      });
    }
    return list;
  }, [sources, keyword, statusFilter, activeGroup, sortByConfidence, invalidMap, statMap]);

  const inspectedSource = React.useMemo(
    () => (inspectedUrl ? sources.find((s) => s.bookSourceUrl === inspectedUrl) ?? null : null),
    [inspectedUrl, sources],
  );

  const invalidateAll = () => {
    void qc.invalidateQueries({ queryKey: ["studio", "sources"] });
    void qc.invalidateQueries({ queryKey: ["studio", "sourceStats"] });
    void qc.invalidateQueries({ queryKey: ["studio", "invalidSources"] });
    void qc.invalidateQueries({ queryKey: ["studio", "sourceCookies"] });
  };

  const toggleEnabledMutation = useMutation({
    mutationFn: ({ src, enabled }: { src: BookSource; enabled: boolean }) =>
      sourcesApi.save({ ...src, enabled }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["studio", "sources"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const batchEnableMutation = useMutation({
    mutationFn: ({ enabled }: { enabled: boolean }) => {
      const targets = sources
        .filter((s) => selectedUrls.has(s.bookSourceUrl))
        .map((s) => ({ ...s, enabled }));
      return sourcesApi.saveMany(targets);
    },
    onSuccess: (res, vars) => {
      notify.success(`已批量${vars.enabled ? "启用" : "禁用"} ${res.count} 个书源`);
      void qc.invalidateQueries({ queryKey: ["studio", "sources"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const batchDeleteMutation = useMutation({
    mutationFn: (urls: string[]) => sourcesApi.removeMany(urls),
    onSuccess: (res) => {
      notify.success(`已批量删除 ${res.deleted} 个书源`);
      if (inspectedUrl && selectedUrls.has(inspectedUrl)) {
        setInspectedUrl(null);
      }
      setSelectedUrls(new Set());
      setMultiSelect(false);
      invalidateAll();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const batchPublishMutation = useMutation({
    mutationFn: (urls: string[]) => sourcesApi.publishDefault(urls, "publish"),
    onSuccess: (res) => {
      notify.success(`已发布 ${res.published ?? 0} 个公共书源`);
      void qc.invalidateQueries({ queryKey: ["studio", "sources"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const toggleSelectUrl = (url: string) => {
    setSelectedUrls((prev) => {
      const next = new Set(prev);
      if (next.has(url)) next.delete(url);
      else next.add(url);
      return next;
    });
  };

  const allFilteredSelected =
    filteredSources.length > 0 &&
    filteredSources.every((s) => selectedUrls.has(s.bookSourceUrl));

  // Pagination state derived from filteredSources
  React.useEffect(() => {
    if (!filterDidMountRef.current) {
      filterDidMountRef.current = true;
      return;
    }
    setPage(1);
  }, [keyword, statusFilter, activeGroup, sortByConfidence]);

  const pageCount = Math.max(1, Math.ceil(filteredSources.length / pageSize));
  const safePage = Math.min(Math.max(1, page), pageCount);
  const pageStart = (safePage - 1) * pageSize;
  const pagedSources = React.useMemo(
    () => filteredSources.slice(pageStart, pageStart + pageSize),
    [filteredSources, pageStart, pageSize],
  );
  const pageTokens = React.useMemo(
    () => buildPageNumbers(safePage, pageCount),
    [safePage, pageCount],
  );

  const allPageSelected =
    pagedSources.length > 0 && pagedSources.every((s) => selectedUrls.has(s.bookSourceUrl));

  const toggleSelectPage = () => {
    setSelectedUrls((prev) => {
      const next = new Set(prev);
      if (allPageSelected) {
        for (const s of pagedSources) next.delete(s.bookSourceUrl);
      } else {
        for (const s of pagedSources) next.add(s.bookSourceUrl);
      }
      return next;
    });
  };

  const handlePageSizeChange = (nextSize: number) => {
    setPageSize(nextSize);
    setPage(1);
    try {
      localStorage.setItem(PAGE_SIZE_KEY, String(nextSize));
    } catch {
      // ignore storage quota
    }
  };

  const goToPage = (targetPage: number) => {
    const clamped = Math.min(Math.max(1, targetPage), pageCount);
    setPage(clamped);
    listTopRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
  };

  const toggleSelectAllFiltered = () => {
    if (allFilteredSelected) {
      setSelectedUrls(new Set());
    } else {
      setSelectedUrls(new Set(filteredSources.map((s) => s.bookSourceUrl)));
    }
  };

  return (
    <div className="flex flex-col gap-4">
      {/* Compact Top Action Strip */}
      <div className="flex flex-col gap-2.5">
        <div className="flex flex-col gap-2.5 xl:flex-row xl:items-center xl:justify-between">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center xl:min-w-0 xl:flex-1">
            <Input
              size="sm"
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
              clearable
              onClear={() => setKeyword("")}
              leftIcon={<Search className="size-3.5" />}
              placeholder="搜索书源名、域名、分组或注释…"
              className="w-full sm:min-w-[200px] sm:flex-1 xl:max-w-[300px]"
            />

            {/* Mobile: clean borderless status tabs; Desktop: Segmented */}
            <div className="flex items-center gap-4 overflow-x-auto no-scrollbar md:hidden">
              {(
                [
                  { value: "all" as const, label: "全部", count: statusCounts.all, tone: "" },
                  { value: "enabled" as const, label: "已启用", count: statusCounts.enabled, tone: "" },
                  {
                    value: "invalid" as const,
                    label: "失效",
                    count: statusCounts.invalid,
                    tone: statusCounts.invalid > 0 ? "text-danger" : "",
                  },
                  { value: "quality" as const, label: "优质", count: statusCounts.quality, tone: "text-success" },
                ]
              ).map((item) => {
                const active = statusFilter === item.value;
                return (
                  <button
                    key={item.value}
                    type="button"
                    onClick={() => setStatusFilter(item.value)}
                    className={cn(
                      "inline-flex shrink-0 cursor-pointer items-center gap-1 py-1 text-xs transition-colors tabular-nums",
                      active ? "font-semibold text-accent" : "text-ink-muted hover:text-ink",
                    )}
                  >
                    <span>{item.label}</span>
                    <span className={cn("text-[11px] opacity-75", !active && item.tone)}>
                      {item.count}
                    </span>
                  </button>
                );
              })}
            </div>

            <div className="hidden max-w-full shrink-0 overflow-x-auto no-scrollbar md:block">
              <Segmented<StatusFilter>
                layoutId="v2-src-status"
                size="sm"
                value={statusFilter}
                onChange={setStatusFilter}
                options={[
                  {
                    value: "all",
                    label: "全部",
                    badge: <span className="text-[10px] opacity-65 tabular-nums">{statusCounts.all}</span>,
                  },
                  {
                    value: "enabled",
                    label: "已启用",
                    badge: (
                      <span className="text-[10px] opacity-65 tabular-nums">{statusCounts.enabled}</span>
                    ),
                  },
                  {
                    value: "invalid",
                    label: "失效",
                    badge:
                      statusCounts.invalid > 0 ? (
                        <span className="text-[10px] text-danger tabular-nums">
                          {statusCounts.invalid}
                        </span>
                      ) : null,
                  },
                  {
                    value: "quality",
                    label: "优质",
                    badge: (
                      <span className="text-[10px] text-success tabular-nums">
                        {statusCounts.quality}
                      </span>
                    ),
                  },
                ]}
              />
            </div>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-1 md:justify-end md:gap-1.5 xl:shrink-0">
            <button
              type="button"
              onClick={() => setSortByConfidence((v) => !v)}
              title="按综合置信度与响应耗时排序"
              className={cn(
                "inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium transition-colors md:rounded-full md:px-3",
                sortByConfidence
                  ? "bg-accent-soft text-accent"
                  : "text-ink-secondary hover:bg-subtle hover:text-ink",
              )}
            >
              <ArrowDownWideNarrow className="size-3.5" />
              <span>{sortByConfidence ? "置信度" : "排序"}</span>
            </button>

            <button
              type="button"
              onClick={() => {
                setMultiSelect((v) => !v);
                setSelectedUrls(new Set());
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

            {multiSelect ? (
              <>
                <Button variant="ghost" size="sm" onClick={toggleSelectPage}>
                  {allPageSelected ? "取消本页" : `全选本页 (${pagedSources.length})`}
                </Button>
                <Button variant="ghost" size="sm" onClick={toggleSelectAllFiltered}>
                  {allFilteredSelected ? "取消全部" : `全选筛选 (${filteredSources.length})`}
                </Button>
              </>
            ) : null}

            <button
              type="button"
              disabled={filteredSources.length === 0}
              onClick={() => downloadJson("book-sources.json", filteredSources)}
              className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:opacity-45 md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-3"
            >
              <Download className="size-3.5" />
              <span>导出</span>
            </button>

            <button
              type="button"
              aria-label="刷新书源数据"
              onClick={() => invalidateAll()}
              className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink md:size-8 md:justify-center md:rounded-full md:px-0"
            >
              <RefreshCw
                className={cn(
                  "size-3.5",
                  (sourcesQuery.isFetching || statsQuery.isFetching) && "animate-spin",
                )}
              />
              <span className="md:hidden">刷新</span>
            </button>

            <button
              type="button"
              onClick={() => setImportOpen(true)}
              className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-semibold text-accent transition-colors hover:bg-accent-soft md:rounded-full md:bg-accent md:px-3.5 md:font-medium md:text-accent-ink md:hover:brightness-110"
            >
              <Plus className="size-3.5" />
              <span>导入</span>
              <span className="hidden md:inline">书源</span>
            </button>
          </div>
        </div>

        {/* Group Filter Pills (single scrollable row on mobile when collapsed, wraps smoothly when expanded or on desktop) */}
        <div
          className={cn(
            "flex items-center gap-1.5",
            groupsExpanded
              ? "flex-wrap"
              : "scroll-fade-x flex-nowrap overflow-x-auto pr-4 pb-0.5 no-scrollbar lg:flex-wrap lg:overflow-visible lg:pr-0 lg:pb-0",
          )}
        >
          <button
            type="button"
            onClick={() => setActiveGroup("__ALL__")}
            className={cn(
              "inline-flex shrink-0 cursor-pointer items-center gap-1 border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:gap-1.5 md:rounded-full md:border md:px-2.5",
              activeGroup === "__ALL__"
                ? "border-accent text-accent md:bg-accent-soft"
                : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
            )}
          >
            <span>全部分组</span>
            <span className="text-[10px] opacity-70">{sources.length}</span>
          </button>
          {groupsMeta.ungrouped > 0 ? (
            <button
              type="button"
              onClick={() => setActiveGroup("__UNGROUPED__")}
              className={cn(
                "inline-flex shrink-0 cursor-pointer items-center gap-1 border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:gap-1.5 md:rounded-full md:border md:px-2.5",
                activeGroup === "__UNGROUPED__"
                  ? "border-accent text-accent md:bg-accent-soft"
                  : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
              )}
            >
              <span>未分组</span>
              <span className="text-[10px] opacity-70">{groupsMeta.ungrouped}</span>
            </button>
          ) : null}
          {visibleGroups.map(([group, count]) => (
            <button
              key={group}
              type="button"
              onClick={() => setActiveGroup(group)}
              className={cn(
                "inline-flex shrink-0 cursor-pointer items-center gap-1 border-b-2 px-1 py-1 text-xs font-medium transition-colors tabular-nums md:gap-1.5 md:rounded-full md:border md:px-2.5",
                activeGroup === group
                  ? "border-accent text-accent md:bg-accent-soft"
                  : "border-transparent text-ink-secondary hover:text-ink md:border-line md:bg-elevated md:hover:border-line-strong",
              )}
            >
              <span className="max-w-[140px] truncate">{group}</span>
              <span className="text-[10px] opacity-70">{count}</span>
            </button>
          ))}
          {canCollapseGroups ? (
            <button
              type="button"
              aria-expanded={groupsExpanded}
              title={`共 ${groupsMeta.list.length} 个分组`}
              onClick={() => setGroupsExpanded((v) => !v)}
              className="inline-flex shrink-0 cursor-pointer items-center gap-1 px-1.5 py-1 text-xs font-medium text-ink-muted transition-colors tabular-nums hover:text-ink md:rounded-full md:border md:border-dashed md:border-line-strong md:bg-subtle/60 md:px-2.5 md:hover:border-accent/50"
            >
              {groupsExpanded ? (
                <>
                  <ChevronUp className="size-3.5" />
                  <span>收起分组</span>
                  <span className="text-[10px] opacity-70">({groupsMeta.list.length})</span>
                </>
              ) : (
                <>
                  <ChevronDown className="size-3.5" />
                  <span>展开其余 {hiddenGroupCount} 个</span>
                  <span className="text-[10px] opacity-65">/ 共 {groupsMeta.list.length}</span>
                </>
              )}
            </button>
          ) : null}
        </div>
      </div>

      {/* Master-Detail Split Workspace */}
      <div className="flex flex-col gap-5 lg:flex-row lg:items-start">
        {/* Left Master List */}
        <div ref={listTopRef} className="min-w-0 flex-1 space-y-3">
          {sourcesQuery.isLoading ? (
            <div className="flex justify-center py-16 md:rounded-2xl md:border md:border-line md:bg-elevated">
              <Spinner label="加载书源库…" />
            </div>
          ) : filteredSources.length === 0 ? (
            <div className="md:rounded-2xl md:border md:border-line md:bg-elevated">
              <EmptyView
                icon={<Layers className="size-5" />}
                title={keyword || statusFilter !== "all" ? "没有符合条件的书源" : "书源库为空"}
                description={
                  keyword || statusFilter !== "all"
                    ? "尝试清空搜索词或切换筛选状态"
                    : "支持粘贴 JSON、拖拽本地文件或远程订阅一键导入书源"
                }
                action={
                  sources.length === 0 ? (
                    <Button variant="primary" size="sm" onClick={() => setImportOpen(true)}>
                      <Plus className="size-3.5" />
                      导入首批书源
                    </Button>
                  ) : null
                }
              />
            </div>
          ) : (
            <>
              <div className="border-y border-line/70 md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated">
                {pagedSources.map((src, index) => {
                  const stat = statMap.get(src.bookSourceUrl);
                  const inv = invalidMap.get(src.bookSourceUrl);
                  const hasCookie = cookieMap.has(src.bookSourceUrl);
                  const groups = splitSourceGroups(src.bookSourceGroup);
                  const isActive = inspectedUrl === src.bookSourceUrl;
                  const isChecked = selectedUrls.has(src.bookSourceUrl);
                  const host = safeHost(src.bookSourceUrl);

                  return (
                    <div
                      key={src.bookSourceUrl}
                      onClick={() => {
                        if (multiSelect) {
                          toggleSelectUrl(src.bookSourceUrl);
                        } else {
                          setInspectedUrl(src.bookSourceUrl);
                        }
                      }}
                      className={cn(
                        "relative flex cursor-pointer items-center gap-2.5 px-1 py-2.5 transition-colors md:px-3.5",
                        index > 0 && "border-t border-line/70 md:border-line",
                        !src.enabled && "opacity-75",
                        isActive && !multiSelect
                          ? "bg-accent-soft/45"
                          : isChecked
                            ? "bg-accent-soft/30"
                            : "hover:bg-subtle/50",
                      )}
                    >
                      {isActive && !multiSelect ? (
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
                        <div className="flex min-w-0 items-center gap-1.5">
                          <span
                            className={cn(
                              "truncate text-[13px] font-medium leading-snug",
                              src.enabled ? "text-ink" : "text-ink-muted",
                            )}
                          >
                            {src.bookSourceName || host}
                          </span>
                          {src.bookSourceType !== 0 ? (
                            <Badge className="shrink-0 px-1.5 py-0.5 text-[10px]">
                              {SOURCE_TYPE_LABELS[src.bookSourceType] ?? "其他"}
                            </Badge>
                          ) : null}
                          {groups[0] ? (
                            <span className="hidden max-w-[96px] shrink-0 sm:inline-flex">
                              <Badge className="max-w-[96px] px-1.5 py-0.5 text-[10px]">
                                <span className="truncate">{groups[0]}</span>
                              </Badge>
                            </span>
                          ) : null}
                          {groups.length > 1 ? (
                            <span
                              title={groups.join(" / ")}
                              className="hidden shrink-0 text-[10px] text-ink-muted tabular-nums sm:inline"
                            >
                              +{groups.length - 1}
                            </span>
                          ) : null}
                          {src.published ? (
                            <span className="shrink-0 text-[10.5px] font-medium text-accent sm:rounded-full sm:bg-accent-soft sm:px-1.5 sm:py-0.5 sm:text-[10px]">
                              公共
                            </span>
                          ) : null}
                          {hasCookie ? (
                            <span
                              title="已保存 Cookie"
                              className="inline-flex shrink-0 items-center text-accent"
                            >
                              <Cookie className="size-3" />
                            </span>
                          ) : null}
                        </div>
                        <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] text-ink-muted">
                          <span className="max-w-[52%] shrink-0 truncate font-mono sm:max-w-[45%]">
                            {host}
                          </span>
                          {groups[0] ? (
                            <>
                              <span className="shrink-0 opacity-50 sm:hidden">·</span>
                              <span className="min-w-0 truncate opacity-85 sm:hidden">
                                {groups[0]}
                              </span>
                            </>
                          ) : null}
                          {src.bookSourceComment ? (
                            <>
                              <span className="hidden shrink-0 opacity-50 sm:inline">·</span>
                              <span className="hidden min-w-0 truncate opacity-85 sm:inline">
                                {src.bookSourceComment}
                              </span>
                            </>
                          ) : null}
                          <span className="ml-auto shrink-0 sm:hidden">
                            <SourceHealthBadges stat={stat} invalid={inv} compact />
                          </span>
                        </div>
                      </div>

                      {/* Health & Latency Badges (Graded: 快 / 中 / 慢 / 失败, tabular-nums) */}
                      <div className="hidden shrink-0 items-center justify-end sm:flex sm:min-w-[124px]">
                        <SourceHealthBadges stat={stat} invalid={inv} />
                      </div>

                      <div className="shrink-0 pl-0.5">
                        <Switch
                          size="sm"
                          checked={src.enabled}
                          ariaLabel={`启停书源 ${src.bookSourceName}`}
                          onChange={(enabled) => toggleEnabledMutation.mutate({ src, enabled })}
                        />
                      </div>
                    </div>
                  );
                })}
              </div>

              {/* Pagination Bar (flat on mobile, card on desktop) */}
              <div className="flex flex-wrap items-center justify-between gap-3 pt-2 text-xs text-ink-secondary md:rounded-2xl md:border md:border-line md:bg-elevated md:px-4 md:py-2.5">
                <span className="tabular-nums">
                  共 {filteredSources.length} 个书源 · 显示 {pageStart + 1}–
                  {Math.min(pageStart + pageSize, filteredSources.length)} · 第 {safePage} /{" "}
                  {pageCount} 页
                </span>

                <div className="flex flex-wrap items-center gap-2">
                  <label className="inline-flex items-center gap-1.5 text-ink-muted">
                    <span>每页</span>
                    <select
                      aria-label="每页显示条数"
                      value={pageSize}
                      onChange={(e) => handlePageSizeChange(Number(e.target.value))}
                      className="cursor-pointer rounded-lg border border-line bg-bg px-2 py-1 text-xs text-ink outline-none focus-visible:border-accent"
                    >
                      {PAGE_SIZE_OPTIONS.map((n) => (
                        <option key={n} value={n}>
                          {n}
                        </option>
                      ))}
                    </select>
                    <span>条</span>
                  </label>

                  <div className="flex items-center gap-1">
                    <Button
                      variant="secondary"
                      size="xs"
                      disabled={safePage <= 1}
                      onClick={() => goToPage(safePage - 1)}
                    >
                      <ChevronLeft className="size-3.5" />
                      上一页
                    </Button>

                    <div className="hidden items-center gap-1 sm:flex">
                      {pageTokens.map((tok, idx) =>
                        tok === "ellipsis" ? (
                          <span
                            key={`e-${idx}`}
                            className="px-1 text-xs text-ink-muted select-none"
                          >
                            …
                          </span>
                        ) : (
                          <button
                            key={tok}
                            type="button"
                            onClick={() => goToPage(tok)}
                            className={cn(
                              "inline-flex h-7 min-w-7 cursor-pointer items-center justify-center rounded-lg border px-2 text-xs font-medium transition-colors tabular-nums",
                              tok === safePage
                                ? "border-accent bg-accent text-accent-ink"
                                : "border-line bg-bg text-ink-secondary hover:border-line-strong hover:text-ink",
                            )}
                          >
                            {tok}
                          </button>
                        ),
                      )}
                    </div>

                    <Button
                      variant="secondary"
                      size="xs"
                      disabled={safePage >= pageCount}
                      onClick={() => goToPage(safePage + 1)}
                    >
                      下一页
                      <ChevronRight className="size-3.5" />
                    </Button>
                  </div>
                </div>
              </div>
            </>
          )}
        </div>

        {/* Right Sticky Inspector (Desktop) */}
        <aside className="hidden lg:sticky lg:top-20 lg:flex lg:max-h-[calc(100dvh-6.5rem)] lg:w-[460px] lg:shrink-0 lg:flex-col lg:overflow-hidden lg:rounded-2xl lg:border lg:border-line lg:bg-elevated xl:w-[500px]">
          {isDesktop && inspectedSource ? (
            <SourceInspector
              key={inspectedSource.bookSourceUrl}
              source={inspectedSource}
              stat={statMap.get(inspectedSource.bookSourceUrl)}
              invalid={invalidMap.get(inspectedSource.bookSourceUrl)}
              cookieRow={cookieMap.get(inspectedSource.bookSourceUrl)}
              isAdmin={isAdmin}
              onClose={() => setInspectedUrl(null)}
              onDeleted={() => {
                setInspectedUrl(null);
                invalidateAll();
              }}
              onMutated={invalidateAll}
              onOpenWorkbench={() =>
                navigate(`/studio/workbench?url=${encodeURIComponent(inspectedSource.bookSourceUrl)}`)
              }
            />
          ) : (
            <EmptyView
              className="my-auto"
              icon={<SlidersHorizontal className="size-5" />}
              title="书源检视器"
              description="点击左侧任意书源行，可在此快捷编辑、校验 JSON、原地试搜验证或管理登录 Cookie"
              action={
                <Button
                  variant="subtle"
                  size="sm"
                  onClick={() => navigate("/studio/workbench")}
                >
                  <Terminal className="size-3.5" />
                  前往规则调试台
                </Button>
              }
            />
          )}
        </aside>
      </div>

      {/* Mobile Inspector Sheet */}
      <Sheet
        open={!isDesktop && inspectedSource !== null}
        onClose={() => setInspectedUrl(null)}
        size="lg"
        bodyClassName="p-0 flex flex-col"
      >
        {!isDesktop && inspectedSource ? (
          <SourceInspector
            key={inspectedSource.bookSourceUrl}
            source={inspectedSource}
            stat={statMap.get(inspectedSource.bookSourceUrl)}
            invalid={invalidMap.get(inspectedSource.bookSourceUrl)}
            cookieRow={cookieMap.get(inspectedSource.bookSourceUrl)}
            isAdmin={isAdmin}
            inSheet
            onClose={() => setInspectedUrl(null)}
            onDeleted={() => {
              setInspectedUrl(null);
              invalidateAll();
            }}
            onMutated={invalidateAll}
            onOpenWorkbench={() =>
              navigate(`/studio/workbench?url=${encodeURIComponent(inspectedSource.bookSourceUrl)}`)
            }
          />
        ) : null}
      </Sheet>

      {/* Import Sources Sheet */}
      <ImportSourcesSheet
        open={importOpen}
        existingUrls={existingUrlSet}
        onClose={() => setImportOpen(false)}
        onImported={() => {
          setImportOpen(false);
          invalidateAll();
        }}
      />

      {/* Multi-select floating island */}
      <AnimatePresence>
        {multiSelect && selectedUrls.size > 0 ? (
          <motion.div
            initial={{ opacity: 0, y: 24, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 20, scale: 0.96 }}
            transition={SPRING_POP}
            className="surface-glass fixed bottom-[calc(4.25rem+env(safe-area-inset-bottom,0px))] left-1/2 z-40 flex w-[calc(100vw-1.5rem)] max-w-fit -translate-x-1/2 flex-wrap items-center justify-center gap-1.5 rounded-2xl border border-line-strong/80 px-3 py-2 shadow-[var(--shadow-float)] sm:w-auto sm:flex-nowrap sm:rounded-full sm:px-4 md:bottom-5"
          >
            <span className="mr-1 text-xs font-medium text-ink tabular-nums">
              已选 {selectedUrls.size} 个
            </span>
            <Button
              variant="ghost"
              size="xs"
              loading={batchEnableMutation.isPending}
              onClick={() => batchEnableMutation.mutate({ enabled: true })}
            >
              批量启用
            </Button>
            <Button
              variant="ghost"
              size="xs"
              loading={batchEnableMutation.isPending}
              onClick={() => batchEnableMutation.mutate({ enabled: false })}
            >
              批量禁用
            </Button>
            <Button
              variant="ghost"
              size="xs"
              onClick={() => {
                const picked = sources.filter((s) => selectedUrls.has(s.bookSourceUrl));
                downloadJson("book-sources-selected.json", picked);
              }}
            >
              <Download className="size-3" />
              导出
            </Button>
            {isAdmin ? (
              <Button
                variant="subtle"
                size="xs"
                loading={batchPublishMutation.isPending}
                onClick={() => batchPublishMutation.mutate(Array.from(selectedUrls))}
              >
                <Shield className="size-3" />
                发布公共
              </Button>
            ) : null}
            <InlineConfirm
              confirmLabel="确认批量删除"
              busy={batchDeleteMutation.isPending}
              onConfirm={() =>
                batchDeleteMutation
                  .mutateAsync(Array.from(selectedUrls))
                  .then(() => undefined)
              }
              trigger={(open) => (
                <Button variant="danger" size="xs" onClick={open}>
                  <Trash2 className="size-3" />
                  删除
                </Button>
              )}
            />
            <IconButton
              size="sm"
              aria-label="退出多选"
              onClick={() => {
                setSelectedUrls(new Set());
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

/* ==========================================================================
   Source Inspector (4 Sub-tabs: Quick Edit / Raw JSON / Live Test / Auth & Cookie)
   ========================================================================== */

function InspectorTabPanel({
  active,
  reduced,
  children,
}: {
  active: boolean;
  reduced: boolean;
  children: React.ReactNode;
}) {
  return (
    <motion.div
      role="tabpanel"
      aria-hidden={!active}
      initial={false}
      animate={active ? { opacity: 1, y: 0 } : { opacity: 0, y: 6 }}
      transition={reduced ? { duration: 0 } : { duration: 0.18, ease: EASE_PAPER }}
      className={active ? "block" : "hidden"}
    >
      {children}
    </motion.div>
  );
}

function SourceInspector({
  source,
  stat,
  invalid,
  cookieRow,
  isAdmin,
  inSheet = false,
  onClose,
  onDeleted,
  onMutated,
  onOpenWorkbench,
}: {
  source: BookSource;
  stat?: SourceStat;
  invalid?: InvalidBookSource;
  cookieRow?: SourceCookie;
  isAdmin: boolean;
  inSheet?: boolean;
  onClose: () => void;
  onDeleted: () => void;
  onMutated: () => void;
  onOpenWorkbench: () => void;
}) {
  const navigate = useNavigate();
  const reducedMotion = Boolean(useReducedMotion());
  const searchTimeout = usePrefsStore((s) => s.searchTimeout);
  const restoredTest = cachedSourceTestByUrl.get(source.bookSourceUrl) ?? null;

  const [tab, setTab] = React.useState<InspectorTab>(() => restoredTest?.tab ?? "quick");
  const scrollBodyRef = React.useRef<HTMLDivElement>(null);
  const scrollTopByTabRef = React.useRef<Record<InspectorTab, number>>({
    quick: 0,
    json: 0,
    test: 0,
    auth: 0,
  });

  const handleSelectTab = (next: InspectorTab) => {
    if (next === tab) return;
    if (scrollBodyRef.current) {
      scrollTopByTabRef.current[tab] = scrollBodyRef.current.scrollTop;
    }
    setTab(next);
  };

  React.useLayoutEffect(() => {
    if (scrollBodyRef.current) {
      scrollBodyRef.current.scrollTop = scrollTopByTabRef.current[tab] ?? 0;
    }
  }, [tab]);

  // Quick edit draft
  const [name, setName] = React.useState(source.bookSourceName);
  const [group, setGroup] = React.useState(source.bookSourceGroup ?? "");
  const [url, setUrl] = React.useState(source.bookSourceUrl);
  const [sourceType, setSourceType] = React.useState(source.bookSourceType ?? 0);
  const [weight, setWeight] = React.useState(String(source.weight ?? 0));
  const [customOrder, setCustomOrder] = React.useState(String(source.customOrder ?? 0));
  const [comment, setComment] = React.useState(source.bookSourceComment ?? "");
  const [enabled, setEnabled] = React.useState(source.enabled);
  const [enabledExplore, setEnabledExplore] = React.useState(source.enabledExplore);

  // Raw JSON draft
  const [rawJson, setRawJson] = React.useState(() => JSON.stringify(source, null, 2));

  // Test search state
  const [testKey, setTestKey] = React.useState(() => restoredTest?.testKey ?? "诡秘之主");
  const [testLoading, setTestLoading] = React.useState(false);
  const [testElapsed, setTestElapsed] = React.useState<number | null>(
    () => restoredTest?.testElapsed ?? null,
  );
  const [testResults, setTestResults] = React.useState<SearchBook[] | null>(
    () => restoredTest?.testResults ?? null,
  );
  const [testError, setTestError] = React.useState<string | null>(
    () => restoredTest?.testError ?? null,
  );
  const testAbortRef = React.useRef<AbortController | null>(null);

  React.useEffect(() => {
    cachedSourceTestByUrl.set(source.bookSourceUrl, {
      tab,
      testKey,
      testElapsed,
      testResults,
      testError,
    });
  }, [source.bookSourceUrl, tab, testKey, testElapsed, testResults, testError]);

  // Login & Cookie state
  const [loginMode, setLoginMode] = React.useState<"http" | "browser">("http");
  const [loginUser, setLoginUser] = React.useState("");
  const [loginPass, setLoginPass] = React.useState("");
  const [captchaCode, setCaptchaCode] = React.useState("");
  const [loginResp, setLoginResp] = React.useState<SourceLoginResult | null>(null);
  const [cookieText, setCookieText] = React.useState(cookieRow?.cookie ?? "");

  React.useEffect(() => {
    setCookieText(cookieRow?.cookie ?? "");
  }, [cookieRow?.cookie]);

  React.useEffect(() => {
    setEnabled(source.enabled);
  }, [source.enabled]);

  React.useEffect(
    () => () => {
      testAbortRef.current?.abort();
    },
    [],
  );

  const jsonValidation = React.useMemo(() => {
    try {
      const parsed: unknown = JSON.parse(rawJson);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return { valid: false, error: "根节点必须是 JSON 对象", data: null };
      }
      const obj = parsed as Record<string, unknown>;
      if (!String(obj.bookSourceUrl ?? "").trim()) {
        return { valid: false, error: "缺少必填字段 bookSourceUrl", data: null };
      }
      return { valid: true, error: null, data: obj as Partial<BookSource> & { bookSourceUrl: string } };
    } catch (err) {
      return {
        valid: false,
        error: err instanceof Error ? err.message : "JSON 语法错误",
        data: null,
      };
    }
  }, [rawJson]);

  const saveQuickMutation = useMutation({
    mutationFn: () => {
      const cleanUrl = url.trim();
      if (!cleanUrl) return Promise.reject(new Error("书源 URL 不能为空"));
      return sourcesApi.save({
        ...source,
        bookSourceName: name.trim() || safeHost(cleanUrl),
        bookSourceGroup: group.trim() || null,
        bookSourceUrl: cleanUrl,
        bookSourceType: sourceType,
        weight: Number(weight) || 0,
        customOrder: Number(customOrder) || 0,
        bookSourceComment: comment.trim() || null,
        enabled,
        enabledExplore,
        lastUpdateTime: Date.now(),
      });
    },
    onSuccess: () => {
      notify.success("书源信息已保存");
      onMutated();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const saveJsonMutation = useMutation({
    mutationFn: () => {
      if (!jsonValidation.valid || !jsonValidation.data) {
        return Promise.reject(new Error(jsonValidation.error || "JSON 无效"));
      }
      return sourcesApi.save(jsonValidation.data);
    },
    onSuccess: () => {
      notify.success("已保存原始 JSON 配置");
      onMutated();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const publishMutation = useMutation({
    mutationFn: () =>
      sourcesApi.publishDefault([source.bookSourceUrl], source.published ? "remove" : "publish"),
    onSuccess: () => {
      notify.success(source.published ? "已从公共书源库下架" : "已发布至公共书源库");
      onMutated();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const deleteMutation = useMutation({
    mutationFn: () => sourcesApi.remove(source.bookSourceUrl),
    onSuccess: () => {
      notify.success(`已删除书源「${source.bookSourceName}」`);
      onDeleted();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const loginMutation = useMutation({
    mutationFn: () =>
      sourcesApi.login({
        bookSource: source.bookSourceUrl,
        username: loginUser.trim() || undefined,
        password: loginPass || undefined,
        captcha: captchaCode.trim() || undefined,
        mode: loginMode,
      }),
    onSuccess: (res) => {
      setLoginResp(res);
      if (res.success) {
        setCookieText(res.cookie);
        notify.success("书源登录成功，Cookie 已保存");
        onMutated();
      } else if ("needCaptcha" in res && res.needCaptcha) {
        notify.info(res.message || "请输入图形验证码后再次提交");
      } else if ("needManualCaptcha" in res && res.needManualCaptcha) {
        notify.info(res.message || "请手动获取并粘贴 Cookie");
      } else {
        notify.error(res.message || "登录失败");
      }
    },
    onError: (err) => notify.error(errText(err)),
  });

  const saveCookieMutation = useMutation({
    mutationFn: (cookie: string) => sourcesApi.setCookie(source.bookSourceUrl, cookie),
    onSuccess: (res) => {
      notify.success(res.cleared ? "已清空该书源 Cookie" : "Cookie 已保存");
      if (res.cleared) setCookieText("");
      onMutated();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const runTestSearch = async () => {
    const q = testKey.trim();
    if (!q) {
      notify.info("请输入测试书名或作者");
      return;
    }
    testAbortRef.current?.abort();
    const ctrl = new AbortController();
    testAbortRef.current = ctrl;
    setTestLoading(true);
    setTestError(null);
    const start = performance.now();
    try {
      const list = await discoverApi.searchSingle(
        { key: q, bookSource: source.bookSourceUrl, page: 1, timeout: searchTimeout },
        ctrl.signal,
      );
      setTestElapsed(Math.round(performance.now() - start));
      setTestResults(list ?? []);
    } catch (err) {
      if (!ctrl.signal.aborted) {
        setTestElapsed(Math.round(performance.now() - start));
        setTestError(errText(err));
        setTestResults(null);
      }
    } finally {
      if (!ctrl.signal.aborted) setTestLoading(false);
    }
  };

  const telemetryHealth =
    stat && (stat.confidence !== null || stat.attempts > 0) ? getHealthMeta(stat) : null;
  const telemetryLatency =
    stat && stat.avgLatencyMs > 0 ? LATENCY_META[getLatencyGrade(stat.avgLatencyMs)] : null;
  const testLatencyMeta =
    testElapsed !== null ? LATENCY_META[getLatencyGrade(testElapsed)] : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {/* Header */}
      <header className="flex shrink-0 flex-col gap-3 border-b border-line px-5 pt-4 pb-3">
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-1.5">
              <h2 className="truncate font-serif text-base font-semibold text-ink">
                {source.bookSourceName || "未命名书源"}
              </h2>
              {source.published ? <Badge tone="accent">公共</Badge> : null}
              <SourceHealthBadges stat={stat} invalid={invalid} compact />
            </div>
            <p className="mt-0.5 truncate font-mono text-[11px] text-ink-muted">
              {source.bookSourceUrl}
            </p>
          </div>
          <IconButton size="sm" aria-label="关闭检视器" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        </div>

        <Segmented<InspectorTab>
          layoutId={inSheet ? "v2-src-inspector-sheet-tab" : "v2-src-inspector-tab"}
          size="sm"
          fullWidth
          ariaLabel="书源检视面板"
          value={tab}
          onChange={handleSelectTab}
          options={[
            { value: "quick", label: "快捷编辑" },
            {
              value: "json",
              label: "原始 JSON",
              badge: !jsonValidation.valid ? (
                <span className="size-1.5 rounded-full bg-danger" aria-label="JSON 语法有误" />
              ) : null,
            },
            {
              value: "test",
              label: "试搜验证",
              badge: testError ? (
                <span className="size-1.5 rounded-full bg-danger" aria-label="试搜出错" />
              ) : testResults !== null ? (
                <span className="text-[10px] opacity-70 tabular-nums">{testResults.length}</span>
              ) : null,
            },
            {
              value: "auth",
              label: "登录与 Cookie",
              badge: cookieRow?.cookie ? (
                <span className="size-1.5 rounded-full bg-success" aria-label="已保存 Cookie" />
              ) : null,
            },
          ]}
        />
      </header>

      {/* Scrollable Body (all 4 panels stay mounted to preserve state & scroll) */}
      <div ref={scrollBodyRef} className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <InspectorTabPanel active={tab === "quick"} reduced={reducedMotion}>
          <div className="flex flex-col gap-4">
            {/* Invalid alert */}
            {invalid ? (
              <div className="flex items-start gap-2.5 rounded-xl border border-danger/30 bg-danger-soft/50 px-3.5 py-2.5 text-xs text-danger">
                <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                <div className="min-w-0">
                  <p className="font-medium">最近一次探测已标记失效</p>
                  <p className="mt-0.5 break-all opacity-90">
                    {invalid.error || invalid.errorMsg || "连接异常"}
                  </p>
                </div>
              </div>
            ) : null}

            {/* Telemetry strip */}
            {stat ? (
              <div className="grid grid-cols-3 gap-2 rounded-xl border border-line bg-subtle/45 p-3 text-center tabular-nums">
                <div>
                  <p className="text-[10px] text-ink-muted">
                    {stat.confidence !== null ? "综合置信度" : "成功率"}
                  </p>
                  <p
                    className={cn(
                      "mt-0.5 text-sm font-semibold",
                      telemetryHealth ? telemetryHealth.textClass : "text-ink",
                    )}
                  >
                    {telemetryHealth
                      ? telemetryHealth.failed
                        ? "失败"
                        : `${telemetryHealth.pct}%`
                      : "—"}
                  </p>
                </div>
                <div>
                  <p className="text-[10px] text-ink-muted">平均延迟</p>
                  <p
                    className={cn(
                      "mt-0.5 text-sm font-semibold",
                      telemetryLatency ? telemetryLatency.textClass : "text-ink",
                    )}
                  >
                    {stat.avgLatencyMs > 0 && telemetryLatency
                      ? `${telemetryLatency.label} · ${formatLatency(stat.avgLatencyMs)}`
                      : "—"}
                  </p>
                </div>
                <div>
                  <p className="text-[10px] text-ink-muted">有效样本</p>
                  <p className="mt-0.5 text-sm font-semibold text-ink">
                    {stat.effectiveSamples || stat.attempts}
                    {stat.consecFailures > 0 ? (
                      <span className="ml-1 text-[11px] font-normal text-danger">
                        (连败 {stat.consecFailures})
                      </span>
                    ) : null}
                  </p>
                </div>
              </div>
            ) : null}

            <Input
              label="书源名称"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            <Input
              label="分组（逗号分隔支持多分组）"
              placeholder="如：玄幻, 精选"
              value={group}
              onChange={(e) => setGroup(e.target.value)}
            />
            <Input
              label="书源唯一 URL"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />

            <div className="grid grid-cols-3 gap-2.5">
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-ink-secondary">类型</label>
                <select
                  value={sourceType}
                  onChange={(e) => setSourceType(Number(e.target.value))}
                  className="h-9.5 rounded-xl border border-line-strong/80 bg-elevated px-2.5 text-[13px] text-ink outline-none focus:border-accent/60"
                >
                  {Object.entries(SOURCE_TYPE_LABELS).map(([k, label]) => (
                    <option key={k} value={k}>
                      {label}
                    </option>
                  ))}
                </select>
              </div>
              <Input
                label="权重"
                type="number"
                value={weight}
                onChange={(e) => setWeight(e.target.value)}
              />
              <Input
                label="排序"
                type="number"
                value={customOrder}
                onChange={(e) => setCustomOrder(e.target.value)}
              />
            </div>

            <Textarea
              label="书源注释"
              rows={3}
              placeholder="可填写维护记录、特点或注意事项…"
              value={comment}
              onChange={(e) => setComment(e.target.value)}
            />

            <div className="flex flex-col gap-2 rounded-xl border border-line bg-subtle/40 px-3.5 py-2.5">
              <label className="flex items-center justify-between text-xs text-ink">
                <span>启用该书源</span>
                <Switch checked={enabled} onChange={setEnabled} />
              </label>
              <label className="flex items-center justify-between text-xs text-ink">
                <span>启用书海发现</span>
                <Switch checked={enabledExplore} onChange={setEnabledExplore} />
              </label>
            </div>

            <Button
              variant="primary"
              size="sm"
              loading={saveQuickMutation.isPending}
              onClick={() => saveQuickMutation.mutate()}
              className="w-full"
            >
              保存快捷修改
            </Button>
          </div>
        </InspectorTabPanel>

        <InspectorTabPanel active={tab === "json"} reduced={reducedMotion}>
          <div className="flex flex-col gap-3">
            <div className="flex items-center justify-between gap-2">
              <Badge tone={jsonValidation.valid ? "success" : "danger"}>
                {jsonValidation.valid ? "JSON 校验通过" : "语法有误"}
              </Badge>
              <Button
                variant="ghost"
                size="xs"
                disabled={!jsonValidation.valid}
                onClick={() => {
                  if (jsonValidation.data) {
                    setRawJson(JSON.stringify(jsonValidation.data, null, 2));
                  }
                }}
              >
                <Code2 className="size-3" />
                格式化
              </Button>
            </div>
            {jsonValidation.error ? (
              <p className="rounded-xl border border-danger/30 bg-danger-soft/50 px-3 py-2 font-mono text-[11px] text-danger">
                {jsonValidation.error}
              </p>
            ) : null}
            <Textarea
              mono
              rows={18}
              value={rawJson}
              onChange={(e) => setRawJson(e.target.value)}
              className="font-mono"
            />
            <Button
              variant="primary"
              size="sm"
              disabled={!jsonValidation.valid}
              loading={saveJsonMutation.isPending}
              onClick={() => saveJsonMutation.mutate()}
              className="w-full"
            >
              保存 JSON 配置
            </Button>
          </div>
        </InspectorTabPanel>

        <InspectorTabPanel active={tab === "test"} reduced={reducedMotion}>
          <div className="flex flex-col gap-3.5">
            <div className="flex items-center gap-2">
              <Input
                size="sm"
                value={testKey}
                onChange={(e) => setTestKey(e.target.value)}
                placeholder="输入书名或作者实时试搜…"
                className="flex-1"
                onKeyDown={(e) => {
                  if (e.key === "Enter") void runTestSearch();
                }}
              />
              <Button
                variant="primary"
                size="sm"
                loading={testLoading}
                onClick={() => void runTestSearch()}
              >
                <Play className="size-3" />
                试搜
              </Button>
            </div>

            {testElapsed !== null ? (
              <div className="flex items-center justify-between text-xs text-ink-muted tabular-nums">
                <span>
                  {testError
                    ? "试搜请求失败"
                    : testResults
                      ? `命中 ${testResults.length} 本`
                      : "试搜结束"}
                </span>
                {testError ? (
                  <Badge tone="danger">失败 · {formatLatency(testElapsed)}</Badge>
                ) : testLatencyMeta ? (
                  <Badge tone={testLatencyMeta.tone}>
                    {testLatencyMeta.label} · {formatLatency(testElapsed)}
                  </Badge>
                ) : (
                  <span>耗时 {testElapsed} ms</span>
                )}
              </div>
            ) : null}

            {testError ? (
              <div className="rounded-xl border border-danger/30 bg-danger-soft/50 px-3.5 py-2.5 text-xs text-danger">
                {testError}
              </div>
            ) : null}

            {testLoading ? (
              <div className="flex justify-center py-10">
                <Spinner label="正在单源试搜…" />
              </div>
            ) : testResults !== null ? (
              testResults.length === 0 ? (
                <EmptyView
                  compact
                  icon={<Search className="size-4" />}
                  title="未搜到匹配书籍"
                  description="可尝试更换常见书名，或点击底部「前往调试台」排查规则"
                />
              ) : (
                <div className="divide-y divide-line overflow-hidden rounded-xl border border-line bg-elevated">
                  {testResults.slice(0, 20).map((book, idx) => (
                    <div
                      key={`${book.bookUrl}-${idx}`}
                      className="flex items-start gap-3 p-3 transition-colors hover:bg-subtle/40"
                    >
                      <BookCover
                        src={book.coverUrl}
                        referer={source.bookSourceUrl}
                        title={book.name}
                        author={book.author}
                        interactive={false}
                        pageEdges={false}
                        className="w-11 shrink-0"
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center justify-between gap-2">
                          <p className="truncate text-[13px] font-medium text-ink">{book.name}</p>
                          <button
                            type="button"
                            onClick={() =>
                              navigate(buildReaderUrl(book.bookUrl, 0, source.bookSourceUrl))
                            }
                            className="inline-flex shrink-0 cursor-pointer items-center gap-0.5 text-[11px] text-accent hover:underline"
                          >
                            试读
                            <ExternalLink className="size-3" />
                          </button>
                        </div>
                        <p className="mt-0.5 truncate text-[11px] text-ink-secondary">
                          {book.author || "未知作者"}
                          {book.kind ? ` · ${book.kind}` : ""}
                        </p>
                        {book.latestChapterTitle ? (
                          <p className="mt-1 truncate text-[11px] text-ink-muted">
                            最新：{book.latestChapterTitle}
                          </p>
                        ) : null}
                      </div>
                    </div>
                  ))}
                </div>
              )
            ) : (
              <EmptyView
                compact
                icon={<Search className="size-4" />}
                title="原地单源试搜"
                description="无需离开页面，即时验证该书源当前搜索规则是否可用"
              />
            )}
          </div>
        </InspectorTabPanel>

        <InspectorTabPanel active={tab === "auth"} reduced={reducedMotion}>
          <div className="flex flex-col gap-5">
            {/* Login form */}
            <div className="flex flex-col gap-3 rounded-2xl border border-line bg-subtle/35 p-4">
              <div className="flex items-center justify-between gap-2">
                <span className="flex items-center gap-1.5 text-xs font-semibold text-ink">
                  <KeyRound className="size-3.5 text-accent" />
                  自动登录获取 Cookie
                </span>
                <Segmented<"http" | "browser">
                  layoutId="v2-src-login-mode"
                  size="sm"
                  value={loginMode}
                  onChange={setLoginMode}
                  options={[
                    { value: "http", label: "HTTP" },
                    { value: "browser", label: "浏览器" },
                  ]}
                />
              </div>

              <Input
                size="sm"
                label="账号"
                placeholder="书源站点用户名 / 手机号"
                value={loginUser}
                onChange={(e) => setLoginUser(e.target.value)}
              />
              <Input
                size="sm"
                type="password"
                label="密码"
                placeholder="站点登录密码"
                value={loginPass}
                onChange={(e) => setLoginPass(e.target.value)}
              />

              {/* Captcha block when backend returns needCaptcha */}
              {loginResp && "needCaptcha" in loginResp && loginResp.needCaptcha ? (
                <div className="flex flex-col gap-2 rounded-xl border border-warn/30 bg-elevated p-3">
                  <p className="text-[11px] text-warn">{loginResp.message || "需要图形验证码"}</p>
                  {loginResp.captchaUrl ? (
                    <img
                      src={resolveImageUrl(loginResp.captchaUrl, source.bookSourceUrl) ?? loginResp.captchaUrl}
                      alt="登录验证码"
                      className="h-12 w-fit rounded-lg border border-line bg-white object-contain"
                    />
                  ) : null}
                  <Input
                    size="sm"
                    placeholder="输入上方图形验证码"
                    value={captchaCode}
                    onChange={(e) => setCaptchaCode(e.target.value)}
                  />
                </div>
              ) : null}

              {loginResp && "needManualCaptcha" in loginResp && loginResp.needManualCaptcha ? (
                <p className="rounded-xl border border-warn/30 bg-warn-soft/50 px-3 py-2 text-xs text-warn">
                  {loginResp.message || "该站点需人机验证，请于浏览器登录后在下方粘贴 Cookie。"}
                </p>
              ) : null}

              {loginResp &&
              !loginResp.success &&
              !("needCaptcha" in loginResp && loginResp.needCaptcha) &&
              !("needManualCaptcha" in loginResp && loginResp.needManualCaptcha) ? (
                <p className="rounded-xl border border-danger/30 bg-danger-soft/50 px-3 py-2 text-xs text-danger">
                  {loginResp.message || "登录失败"}
                </p>
              ) : null}

              <Button
                variant="primary"
                size="sm"
                loading={loginMutation.isPending}
                onClick={() => loginMutation.mutate()}
              >
                执行登录
              </Button>
            </div>

            {/* Manual Cookie Management */}
            <div className="flex flex-col gap-3">
              <div className="flex items-center justify-between gap-2">
                <span className="flex items-center gap-1.5 text-xs font-semibold text-ink">
                  <Cookie className="size-3.5 text-accent" />
                  手动管理 Cookie
                </span>
                {cookieRow?.updatedAt ? (
                  <span className="text-[11px] text-ink-muted tabular-nums">
                    更新于 {formatRelativeTime(cookieRow.updatedAt)}
                  </span>
                ) : (
                  <span className="text-[11px] text-ink-muted">尚未保存</span>
                )}
              </div>

              <Textarea
                mono
                rows={4}
                placeholder="粘贴 key1=value1; key2=value2 格式的 Cookie…"
                value={cookieText}
                onChange={(e) => setCookieText(e.target.value)}
              />

              <div className="flex items-center gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  loading={saveCookieMutation.isPending}
                  onClick={() => saveCookieMutation.mutate(cookieText)}
                  className="flex-1"
                >
                  保存 Cookie
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={!cookieRow && !cookieText}
                  loading={saveCookieMutation.isPending}
                  onClick={() => saveCookieMutation.mutate("")}
                >
                  清空
                </Button>
              </div>
            </div>
          </div>
        </InspectorTabPanel>
      </div>

      {/* Footer Actions */}
      <footer className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-line bg-subtle/30 px-5 py-3">
        <div className="flex items-center gap-1.5">
          <Button variant="subtle" size="xs" onClick={onOpenWorkbench}>
            <Terminal className="size-3" />
            前往调试台
          </Button>
          {isAdmin ? (
            <Button
              variant="ghost"
              size="xs"
              loading={publishMutation.isPending}
              onClick={() => publishMutation.mutate()}
            >
              <Shield className="size-3" />
              {source.published ? "下架公共" : "发布公共"}
            </Button>
          ) : null}
        </div>
        <InlineConfirm
          confirmLabel="确认删除"
          busy={deleteMutation.isPending}
          onConfirm={() => deleteMutation.mutateAsync().then(() => undefined)}
          trigger={(open) => (
            <Button variant="danger" size="xs" onClick={open}>
              <Trash2 className="size-3" />
              删除
            </Button>
          )}
        />
      </footer>
    </div>
  );
}

/* ==========================================================================
   Non-blocking Import Sources Sheet (Paste JSON / Drop File / Remote URL)
   ========================================================================== */

function ImportSourcesSheet({
  open,
  existingUrls,
  onClose,
  onImported,
}: {
  open: boolean;
  existingUrls: Set<string>;
  onClose: () => void;
  onImported: () => void;
}) {
  const [mode, setMode] = React.useState<ImportMode>("paste");
  const [pasteText, setPasteText] = React.useState("");
  const [remoteUrl, setRemoteUrl] = React.useState("");
  const [dragging, setDragging] = React.useState(false);
  const fileRef = React.useRef<HTMLInputElement>(null);

  const [candidates, setCandidates] = React.useState<BookSource[]>([]);
  const [remoteExisting, setRemoteExisting] = React.useState<Set<string>>(() => new Set());
  const [pickedUrls, setPickedUrls] = React.useState<Set<string>>(() => new Set());
  const [parseError, setParseError] = React.useState<string | null>(null);

  const combinedExisting = React.useMemo(() => {
    const merged = new Set(existingUrls);
    for (const u of remoteExisting) merged.add(u);
    return merged;
  }, [existingUrls, remoteExisting]);

  const applyCandidates = (list: BookSource[], extraExisting?: string[]) => {
    const existSet = new Set(existingUrls);
    for (const u of extraExisting ?? []) existSet.add(u);
    setRemoteExisting(new Set(extraExisting ?? []));
    setCandidates(list);
    // Default pick: prefer new ones if any exist, else pick all
    const newOnly = list.filter((s) => !existSet.has(s.bookSourceUrl));
    setPickedUrls(
      new Set((newOnly.length > 0 ? newOnly : list).map((s) => s.bookSourceUrl)),
    );
  };

  const handleParsePaste = (raw: string) => {
    setPasteText(raw);
    const trimmed = raw.trim();
    if (!trimmed) {
      setCandidates([]);
      setPickedUrls(new Set());
      setParseError(null);
      return;
    }
    try {
      const list = parseSourcesPayload(trimmed);
      if (list.length === 0) {
        setParseError("未解析到包含 bookSourceUrl 的有效书源");
        setCandidates([]);
        setPickedUrls(new Set());
      } else {
        setParseError(null);
        applyCandidates(list);
      }
    } catch (err) {
      setParseError(err instanceof Error ? err.message : "JSON 解析失败");
      setCandidates([]);
      setPickedUrls(new Set());
    }
  };

  const handleFileRead = async (file: File) => {
    try {
      const text = await file.text();
      const list = parseSourcesPayload(text);
      if (list.length === 0) {
        setParseError("文件中未包含有效书源数据");
        return;
      }
      setParseError(null);
      applyCandidates(list);
      notify.info(`已从「${file.name}」解析 ${list.length} 个书源`);
    } catch (err) {
      setParseError(err instanceof Error ? err.message : "读取 JSON 文件失败");
    }
  };

  const remotePreviewMutation = useMutation({
    mutationFn: (url: string) => sourcesApi.previewRemote(url.trim()),
    onSuccess: (res) => {
      setParseError(null);
      applyCandidates(res.sources ?? [], res.existing ?? []);
      notify.success(`远程解析成功，共 ${res.sources?.length ?? 0} 个书源`);
    },
    onError: (err) => {
      setParseError(errText(err));
      notify.error(errText(err));
    },
  });

  const saveManyMutation = useMutation({
    mutationFn: async () => {
      const selected = candidates.filter((s) => pickedUrls.has(s.bookSourceUrl));
      if (selected.length === 0) throw new Error("请至少勾选一个书源");
      const CHUNK_SIZE = 250;
      let totalCount = 0;
      for (let i = 0; i < selected.length; i += CHUNK_SIZE) {
        const chunk = selected.slice(i, i + CHUNK_SIZE);
        const res = await sourcesApi.saveMany(chunk);
        totalCount += res.count ?? chunk.length;
      }
      return { count: totalCount };
    },
    onSuccess: (res) => {
      notify.success(`成功导入 ${res.count} 个书源`);
      setPasteText("");
      setRemoteUrl("");
      setCandidates([]);
      setPickedUrls(new Set());
      onImported();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const newCount = candidates.filter((c) => !combinedExisting.has(c.bookSourceUrl)).length;
  const existCount = candidates.length - newCount;

  return (
    <Sheet
      open={open}
      onClose={onClose}
      size="lg"
      title="导入书源"
      subtitle="支持粘贴 JSON、拖拽本地文件或远程订阅 URL，预览去重后一键入库"
      footer={
        <div className="flex items-center justify-between gap-2">
          <span className="text-xs text-ink-muted tabular-nums">
            {candidates.length > 0
              ? `共 ${candidates.length} 条 · 新增 ${newCount} · 已存在 ${existCount}`
              : "等待解析书源"}
          </span>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="sm" onClick={onClose}>
              取消
            </Button>
            <Button
              variant="primary"
              size="sm"
              disabled={pickedUrls.size === 0}
              loading={saveManyMutation.isPending}
              onClick={() => saveManyMutation.mutate()}
            >
              导入选中 ({pickedUrls.size})
            </Button>
          </div>
        </div>
      }
    >
      <div className="flex flex-col gap-4">
        <Segmented<ImportMode>
          layoutId="v2-src-import-mode"
          size="sm"
          value={mode}
          onChange={(m) => {
            setMode(m);
            setParseError(null);
          }}
          options={[
            { value: "paste", label: "粘贴 JSON", icon: <FileJson className="size-3.5" /> },
            { value: "file", label: "本地文件", icon: <Upload className="size-3.5" /> },
            { value: "remote", label: "远程订阅", icon: <Link2 className="size-3.5" /> },
          ]}
        />

        {mode === "paste" ? (
          <Textarea
            mono
            rows={6}
            placeholder="粘贴单个书源 JSON 对象或 Legado 书源 JSON 数组…"
            value={pasteText}
            onChange={(e) => handleParsePaste(e.target.value)}
          />
        ) : null}

        {mode === "file" ? (
          <div
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(e) => {
              e.preventDefault();
              setDragging(false);
              const file = e.dataTransfer.files[0];
              if (file) void handleFileRead(file);
            }}
            onClick={() => fileRef.current?.click()}
            className={cn(
              "flex cursor-pointer flex-col items-center justify-center gap-2 rounded-2xl border border-dashed px-6 py-8 text-center transition-colors",
              dragging
                ? "border-accent bg-accent-soft/45"
                : "border-line-strong bg-subtle/35 hover:border-accent/50 hover:bg-subtle/60",
            )}
          >
            <FileJson className="size-7 text-ink-muted" />
            <p className="text-xs font-medium text-ink">拖拽 .json 书源文件到此，或点击选择</p>
            <p className="text-[11px] text-ink-muted">兼容数组、bookSourceList 包装与单源 JSON</p>
            <input
              ref={fileRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (file) void handleFileRead(file);
                e.target.value = "";
              }}
            />
          </div>
        ) : null}

        {mode === "remote" ? (
          <div className="flex items-center gap-2">
            <Input
              size="sm"
              leftIcon={<Globe className="size-3.5" />}
              placeholder="输入远程书源订阅地址（https://.../sources.json）"
              value={remoteUrl}
              onChange={(e) => setRemoteUrl(e.target.value)}
              className="flex-1"
              onKeyDown={(e) => {
                if (e.key === "Enter" && remoteUrl.trim()) {
                  remotePreviewMutation.mutate(remoteUrl);
                }
              }}
            />
            <Button
              variant="secondary"
              size="sm"
              disabled={!remoteUrl.trim()}
              loading={remotePreviewMutation.isPending}
              onClick={() => remotePreviewMutation.mutate(remoteUrl)}
            >
              获取预览
            </Button>
          </div>
        ) : null}

        {parseError ? (
          <div className="flex items-center gap-2 rounded-xl border border-danger/30 bg-danger-soft/50 px-3 py-2 text-xs text-danger">
            <AlertTriangle className="size-3.5 shrink-0" />
            <span>{parseError}</span>
          </div>
        ) : null}

        {/* Preview list with select all / new only / dedup badges */}
        {candidates.length > 0 ? (
          <div className="flex flex-col gap-2.5">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-xs font-medium text-ink-secondary">
                解析预览（已自动按 URL 去重）
              </span>
              <div className="flex items-center gap-1.5">
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => setPickedUrls(new Set(candidates.map((c) => c.bookSourceUrl)))}
                >
                  全选
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() =>
                    setPickedUrls(
                      new Set(
                        candidates
                          .filter((c) => !combinedExisting.has(c.bookSourceUrl))
                          .map((c) => c.bookSourceUrl),
                      ),
                    )
                  }
                >
                  仅选新增 ({newCount})
                </Button>
                <Button variant="ghost" size="xs" onClick={() => setPickedUrls(new Set())}>
                  清空
                </Button>
              </div>
            </div>

            <div className="max-h-[340px] divide-y divide-line overflow-y-auto rounded-2xl border border-line bg-elevated">
              {candidates.map((src) => {
                const isExisting = combinedExisting.has(src.bookSourceUrl);
                const checked = pickedUrls.has(src.bookSourceUrl);
                return (
                  <label
                    key={src.bookSourceUrl}
                    className={cn(
                      "flex cursor-pointer items-center gap-3 px-3.5 py-2.5 transition-colors",
                      checked ? "bg-accent-soft/25" : "hover:bg-subtle/40",
                    )}
                  >
                    <span
                      onClick={(e) => {
                        e.preventDefault();
                        setPickedUrls((prev) => {
                          const next = new Set(prev);
                          if (next.has(src.bookSourceUrl)) next.delete(src.bookSourceUrl);
                          else next.add(src.bookSourceUrl);
                          return next;
                        });
                      }}
                      className={cn(
                        "flex size-4 shrink-0 items-center justify-center rounded border transition-colors",
                        checked
                          ? "border-accent bg-accent text-accent-ink"
                          : "border-line-strong bg-elevated",
                      )}
                    >
                      {checked ? <Check className="size-3" /> : null}
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-1.5">
                        <span className="truncate text-xs font-medium text-ink">
                          {src.bookSourceName}
                        </span>
                        {src.bookSourceGroup ? (
                          <Badge>{splitSourceGroups(src.bookSourceGroup)[0]}</Badge>
                        ) : null}
                      </div>
                      <p className="truncate font-mono text-[10px] text-ink-muted">
                        {src.bookSourceUrl}
                      </p>
                    </div>
                    <Badge tone={isExisting ? "warn" : "success"}>
                      {isExisting ? "覆盖" : "新增"}
                    </Badge>
                  </label>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </Sheet>
  );
}
