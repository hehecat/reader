import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowRight,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Compass,
  Globe,
  Grid2x2,
  List,
  Loader2,
  RefreshCw,
  ScanSearch,
  Search,
  SkipForward,
  Square as StopIcon,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { booksApi, discoverApi, sourcesApi } from "@/api/endpoints";
import type { Book, BookSource, InvalidBookSource, SearchBook, SourceStat } from "@/api/types";
import { BookInspectorSheet } from "@/features/book/BookInspectorSheet";
import { cn } from "@/lib/cn";
import {
  cleanChapterTitle,
  parseExploreUrl,
  type ExploreCategory,
} from "@/lib/format";
import { BookCover } from "@/ui/BookCover";
import { Button } from "@/ui/Button";
import { Badge, EmptyView } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import {
  AnimatePresence,
  motion,
  PageEnter,
  SPRING_SILK,
  SPRING_SNAPPY,
  useReducedMotion,
} from "@/ui/motion";
import { Popover } from "@/ui/Popover";

/** 分类胶囊折叠态：约 2 行高度（胶囊 28px ×2 + 行距 6px + 内边距 4px） */
const CATEGORY_COLLAPSED_HEIGHT = 66;
const EASE_OUT_QUINT = [0.22, 1, 0.36, 1] as const;
const LAST_EXPLORE_SOURCE_KEY = "reader.explore.sourceUrl";
const EXPLORE_LAYOUT_KEY = "reader.explore.layout.v1";
const EXPLORE_HEALTH_STORAGE_KEY = "reader.explore.health.v1";
const EXPLORE_HEALTH_TTL_MS = 6 * 60 * 60 * 1000;

type ExploreViewMode = "grid" | "list";

function cleanExploreIntro(raw?: string | null): string {
  if (!raw) return "";
  return raw
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/gi, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function splitExploreKinds(kind?: string | null): string[] {
  if (!kind) return [];
  return kind
    .split(/[,，、/|]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && s.length <= 14)
    .slice(0, 2);
}

interface ExploreHealthEntry {
  ok: boolean;
  at: number;
}

type ExploreHealthMap = Record<string, ExploreHealthEntry>;

interface ExploreUiSnapshot {
  selectedSourceUrl: string;
  selectedCategoryIdx: number;
  page: number;
  categoriesExpanded: boolean;
}

let cachedExploreUi: ExploreUiSnapshot | null = null;

interface ProbeProgressState {
  done: number;
  total: number;
  okCount: number;
}

// 模块级后台探活单例：切去书架 / 搜索 / 阅读页后继续在后台扫描，回到书海自动接续实时进度
let bgHealthMap: ExploreHealthMap | null = null;
let bgProbing: ProbeProgressState | null = null;
let bgProbingActive = false;
let bgPrewarmStarted = false;
const bgListeners = new Set<() => void>();

function notifyExploreBg(): void {
  for (const fn of bgListeners) fn();
}

function getBgHealthMap(): ExploreHealthMap {
  if (!bgHealthMap) {
    bgHealthMap = readHealthMap();
  }
  return bgHealthMap;
}

function markBgSourceHealth(sourceUrl: string, ok: boolean): void {
  if (!sourceUrl) return;
  const prev = getBgHealthMap();
  const prevEntry = prev[sourceUrl];
  const now = Date.now();
  if (prevEntry && prevEntry.ok === ok && now - prevEntry.at < 30_000) {
    return;
  }
  bgHealthMap = { ...prev, [sourceUrl]: { ok, at: now } };
  writeHealthMap(bgHealthMap);
  notifyExploreBg();
}

function stopBgProbe(): void {
  bgProbingActive = false;
  bgProbing = null;
  notifyExploreBg();
}

function startBgProbeBatch(
  targets: ExploreSourceEntry[],
  queryClient: ReturnType<typeof useQueryClient>,
  onFirstOk?: (sourceUrl: string) => void,
  stopAfterOkCount?: number,
): void {
  if (targets.length === 0 || bgProbingActive) return;
  bgProbingActive = true;
  let cursor = 0;
  let done = 0;
  let okCount = 0;
  bgProbing = { done: 0, total: targets.length, okCount: 0 };
  notifyExploreBg();

  const concurrency = Math.min(8, targets.length);
  const probeOne = async (entry: ExploreSourceEntry): Promise<boolean> => {
    for (const cat of entry.categories.slice(0, 2)) {
      if (!bgProbingActive) return false;
      try {
        const hits = await discoverApi.explore({
          url: cat.url,
          bookSource: entry.source.bookSourceUrl,
          page: 1,
          timeout: 4,
        });
        if (Array.isArray(hits) && hits.length > 0) {
          queryClient.setQueryData(
            ["explore", entry.source.bookSourceUrl, cat.url, 1],
            hits,
          );
          markBgSourceHealth(entry.source.bookSourceUrl, true);
          return true;
        }
      } catch {
        // try next category
      }
    }
    markBgSourceHealth(entry.source.bookSourceUrl, false);
    return false;
  };

  const worker = async () => {
    while (bgProbingActive) {
      if (stopAfterOkCount && okCount >= stopAfterOkCount) {
        return;
      }
      const idx = cursor;
      cursor += 1;
      if (idx >= targets.length) return;
      const item = targets[idx];
      if (!item) return;

      const ok = await probeOne(item);
      if (!bgProbingActive) return;
      done += 1;
      if (ok) {
        okCount += 1;
        if (okCount === 1) {
          onFirstOk?.(item.source.bookSourceUrl);
        }
      }
      bgProbing = { done, total: targets.length, okCount };
      notifyExploreBg();
    }
  };

  void Promise.all(Array.from({ length: concurrency }, worker)).finally(() => {
    bgProbingActive = false;
    bgProbing = null;
    notifyExploreBg();
  });
}

function readHealthMap(): ExploreHealthMap {
  try {
    const raw = localStorage.getItem(EXPLORE_HEALTH_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as ExploreHealthMap;
    }
  } catch {
    // ignore storage errors
  }
  return {};
}

function writeHealthMap(map: ExploreHealthMap): void {
  try {
    localStorage.setItem(EXPLORE_HEALTH_STORAGE_KEY, JSON.stringify(map));
  } catch {
    // ignore storage quota
  }
}

interface ExploreSourceEntry {
  source: BookSource;
  categories: ExploreCategory[];
  healthScore: number;
  verified: boolean;
}

function computeExploreRank(
  src: BookSource,
  stat: SourceStat | undefined,
  invalid: InvalidBookSource | undefined,
  verified: boolean,
): number {
  const weightBonus = Math.min(20, Math.max(-20, (src.weight ?? 0) / 20));
  const verifiedBonus = verified ? 1000 : 0;
  if (invalid) return -1000;
  if (stat) {
    if (stat.confidence !== null) {
      if (stat.confidence < 0.25) return verifiedBonus - 500 + stat.confidence * 100;
      const speedBonus = stat.avgLatencyMs > 0 ? Math.max(0, 30 - stat.avgLatencyMs / 200) : 0;
      return verifiedBonus + 200 + stat.confidence * 100 + speedBonus + weightBonus;
    }
    if (stat.attempts > 0) {
      return stat.successRate > 0
        ? verifiedBonus + 100 + stat.successRate * 80 + weightBonus
        : verifiedBonus - 300;
    }
  }
  return verifiedBonus + weightBonus;
}

export default function ExplorePage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const reduced = useReducedMotion();

  /* ---------------- Local Explore Health Store (6h TTL, Background-Persistent) ---------------- */
  const [, forceBgSync] = React.useReducer((c: number) => c + 1, 0);
  React.useEffect(() => {
    bgListeners.add(forceBgSync);
    return () => {
      bgListeners.delete(forceBgSync);
    };
  }, []);
  const healthMap = getBgHealthMap();
  const probing = bgProbing;
  const [onlyVerified, setOnlyVerified] = React.useState<boolean>(false);

  const markSourceHealth = React.useCallback((sourceUrl: string, ok: boolean) => {
    markBgSourceHealth(sourceUrl, ok);
  }, []);

  const isSourceHidden = React.useCallback(
    (sourceUrl: string) => {
      const entry = healthMap[sourceUrl];
      if (!entry) return false;
      if (Date.now() - entry.at > EXPLORE_HEALTH_TTL_MS) return false;
      return !entry.ok;
    },
    [healthMap],
  );

  const isSourceVerified = React.useCallback(
    (sourceUrl: string) => {
      const entry = healthMap[sourceUrl];
      if (!entry) return false;
      if (Date.now() - entry.at > EXPLORE_HEALTH_TTL_MS) return false;
      return entry.ok;
    },
    [healthMap],
  );

  const {
    data: allSources = [],
    isLoading: sourcesLoading,
    error: sourcesError,
    refetch: refetchSources,
  } = useQuery({
    queryKey: ["sources"],
    queryFn: () => sourcesApi.list(),
    staleTime: 60_000,
  });

  const { data: statsList = [] } = useQuery({
    queryKey: ["studio", "sourceStats"],
    queryFn: () => sourcesApi.stats(),
    staleTime: 60_000,
  });

  const { data: invalidsList = [] } = useQuery({
    queryKey: ["studio", "invalidSources"],
    queryFn: () => sourcesApi.invalids(),
    staleTime: 30_000,
  });

  const { data: shelf = [] } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(),
    staleTime: 30_000,
  });

  const shelfLookup = React.useMemo(() => {
    const urls = new Set<string>();
    const titles = new Set<string>();
    for (const b of shelf) {
      urls.add(b.bookUrl);
      titles.add(`${b.name.trim().toLowerCase()}::${(b.author || "").trim().toLowerCase()}`);
    }
    return { urls, titles };
  }, [shelf]);

  const allCandidateSources = React.useMemo<ExploreSourceEntry[]>(() => {
    const statMap = new Map<string, SourceStat>();
    for (const st of statsList) statMap.set(st.sourceUrl, st);
    const invMap = new Map<string, InvalidBookSource>();
    for (const inv of invalidsList) {
      invMap.set(inv.sourceUrl || inv.bookSourceUrl || "", inv);
    }

    const entries: ExploreSourceEntry[] = [];
    for (const s of allSources) {
      if (!s.enabled || s.enabledExplore === false) continue;
      const categories = parseExploreUrl(s.exploreUrl);
      if (categories.length > 0) {
        const verified = isSourceVerified(s.bookSourceUrl);
        entries.push({
          source: s,
          categories,
          verified,
          healthScore: computeExploreRank(
            s,
            statMap.get(s.bookSourceUrl),
            invMap.get(s.bookSourceUrl),
            verified,
          ),
        });
      }
    }
    entries.sort((a, b) => b.healthScore - a.healthScore);
    return entries;
  }, [allSources, statsList, invalidsList, isSourceVerified]);

  const unhiddenSources = React.useMemo(
    () => allCandidateSources.filter((e) => !isSourceHidden(e.source.bookSourceUrl)),
    [allCandidateSources, isSourceHidden],
  );

  const verifiedSources = React.useMemo(
    () => unhiddenSources.filter((e) => e.verified),
    [unhiddenSources],
  );

  const untestedSources = React.useMemo(
    () => unhiddenSources.filter((e) => !e.verified),
    [unhiddenSources],
  );

  const hiddenByHealthCount = allCandidateSources.length - unhiddenSources.length;

  // 已验证有书源排在最前（默认首屏即命中真实内容源），已确认空源/死源自动隐藏，待测源保留在列表下方；用户也可切换「仅看已验」
  const exploreSources = React.useMemo(() => {
    if (onlyVerified && verifiedSources.length > 0) {
      return verifiedSources;
    }
    return unhiddenSources;
  }, [onlyVerified, verifiedSources, unhiddenSources]);

  /* ---------------- Channel / Category / Page State ---------------- */
  const [selectedSourceUrl, setSelectedSourceUrl] = React.useState<string>(() => {
    if (cachedExploreUi?.selectedSourceUrl) return cachedExploreUi.selectedSourceUrl;
    try {
      return localStorage.getItem(LAST_EXPLORE_SOURCE_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [selectedCategoryIdx, setSelectedCategoryIdx] = React.useState<number>(
    () => cachedExploreUi?.selectedCategoryIdx ?? 0,
  );
  const [page, setPage] = React.useState<number>(() => cachedExploreUi?.page ?? 1);
  const [categoriesExpanded, setCategoriesExpanded] = React.useState(
    () => cachedExploreUi?.categoriesExpanded ?? false,
  );
  const [sourceSearch, setSourceSearch] = React.useState("");
  const [manualRefreshing, setManualRefreshing] = React.useState(false);
  const [viewMode, setViewMode] = React.useState<ExploreViewMode>(() => {
    try {
      return localStorage.getItem(EXPLORE_LAYOUT_KEY) === "list" ? "list" : "grid";
    } catch {
      return "grid";
    }
  });

  const changeViewMode = (mode: ExploreViewMode) => {
    setViewMode(mode);
    try {
      localStorage.setItem(EXPLORE_LAYOUT_KEY, mode);
    } catch {
      // ignore storage quota
    }
  };

  React.useEffect(() => {
    cachedExploreUi = {
      selectedSourceUrl,
      selectedCategoryIdx,
      page,
      categoriesExpanded,
    };
  }, [selectedSourceUrl, selectedCategoryIdx, page, categoriesExpanded]);

  const [inspectorBook, setInspectorBook] = React.useState<
    (Partial<Book> & { bookUrl: string; name?: string }) | null
  >(null);

  const activeEntry = React.useMemo(() => {
    if (exploreSources.length === 0) return null;
    if (selectedSourceUrl) {
      const found = exploreSources.find((e) => e.source.bookSourceUrl === selectedSourceUrl);
      if (found) return found;
    }
    return exploreSources[0] ?? null;
  }, [exploreSources, selectedSourceUrl]);

  const activeSource = activeEntry?.source ?? null;
  const categories = React.useMemo(() => activeEntry?.categories ?? [], [activeEntry]);
  const activeCategory = categories[selectedCategoryIdx] ?? categories[0] ?? null;

  const persistSourceChoice = React.useCallback((url: string) => {
    setSelectedSourceUrl(url);
    setSelectedCategoryIdx(0);
    setPage(1);
    setCategoriesExpanded(false);
    try {
      localStorage.setItem(LAST_EXPLORE_SOURCE_KEY, url);
    } catch {
      // ignore storage quota
    }
  }, []);

  const handleSelectSource = (url: string, closePopover: () => void) => {
    persistSourceChoice(url);
    setSourceSearch("");
    closePopover();
  };

  const switchToNextSource = React.useCallback(() => {
    if (!activeSource || exploreSources.length <= 1) return;
    const curIdx = exploreSources.findIndex(
      (e) => e.source.bookSourceUrl === activeSource.bookSourceUrl,
    );
    const nextEntry = exploreSources[(curIdx + 1) % exploreSources.length];
    if (nextEntry) {
      persistSourceChoice(nextEntry.source.bookSourceUrl);
    }
  }, [activeSource, exploreSources, persistSourceChoice]);

  /* ---------------- Batch & Cold-Start Explore Health Prober (Background-Persistent) ---------------- */
  // 冷启动自动预检：若当前还没有任何已验证有内容的书海源，自动并发快探前 36 个候选源，攒够 8 个真实有书源即停
  React.useEffect(() => {
    if (
      bgPrewarmStarted ||
      bgProbingActive ||
      sourcesLoading ||
      verifiedSources.length > 0 ||
      unhiddenSources.length === 0
    ) {
      return;
    }
    bgPrewarmStarted = true;
    startBgProbeBatch(
      unhiddenSources.slice(0, 36),
      queryClient,
      (firstUrl) => persistSourceChoice(firstUrl),
      8,
    );
  }, [sourcesLoading, verifiedSources.length, unhiddenSources, queryClient, persistSourceChoice]);

  const handleToggleProbeAll = () => {
    if (bgProbingActive) {
      stopBgProbe();
      return;
    }
    if (allCandidateSources.length === 0) return;
    // 若仍有未测源，优先把剩余未测源全部测完；若全部已测过，则重新全量检测
    const targets = untestedSources.length > 0 ? untestedSources : allCandidateSources;
    startBgProbeBatch(targets, queryClient, (firstUrl) => {
      if (!selectedSourceUrl) persistSourceChoice(firstUrl);
    });
  };

  const handleSelectCategory = (idx: number) => {
    setSelectedCategoryIdx(idx);
    setPage(1);
  };

  const filteredSources = React.useMemo(() => {
    const q = sourceSearch.trim().toLowerCase();
    if (!q) return exploreSources;
    return exploreSources.filter(
      (e) =>
        e.source.bookSourceName.toLowerCase().includes(q) ||
        (e.source.bookSourceGroup || "").toLowerCase().includes(q),
    );
  }, [exploreSources, sourceSearch]);

  /* 分类胶囊按真实排版高度折叠：默认展示约 2 行，溢出时出现「展开全部 N」 */
  const categoryInnerRef = React.useRef<HTMLDivElement | null>(null);
  const [categoryOverflow, setCategoryOverflow] = React.useState(false);
  const [categoryFullHeight, setCategoryFullHeight] = React.useState(CATEGORY_COLLAPSED_HEIGHT);
  const [activeCategoryHidden, setActiveCategoryHidden] = React.useState(false);

  React.useLayoutEffect(() => {
    const el = categoryInnerRef.current;
    if (!el) return;
    const measure = () => {
      const fullHeight = el.scrollHeight;
      const overflow = fullHeight > CATEGORY_COLLAPSED_HEIGHT + 4;
      setCategoryFullHeight(fullHeight);
      setCategoryOverflow(overflow);
      const activeEl = el.querySelector<HTMLElement>('[data-active="true"]');
      setActiveCategoryHidden(
        Boolean(
          overflow &&
            activeEl &&
            activeEl.offsetTop + activeEl.offsetHeight > CATEGORY_COLLAPSED_HEIGHT + 2,
        ),
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [categories, selectedCategoryIdx, exploreSources.length]);

  const shouldCollapse = categoryOverflow;

  /* ---------------- Explore Query ---------------- */
  const activeSourceUrl = activeSource?.bookSourceUrl ?? "";
  const activeCategoryUrl = activeCategory?.url ?? "";

  const queryKey = React.useMemo(
    () => ["explore", activeSourceUrl, activeCategoryUrl, page] as const,
    [activeSourceUrl, activeCategoryUrl, page],
  );

  const {
    data: rawBooks = [],
    isLoading: booksLoading,
    isFetching: booksFetching,
    isSuccess: booksSuccess,
    isPlaceholderData: booksPlaceholder,
    error: booksError,
    refetch: refetchBooks,
  } = useQuery({
    queryKey,
    queryFn: ({ signal }) =>
      discoverApi.explore(
        {
          url: activeCategoryUrl,
          bookSource: activeSourceUrl,
          page,
        },
        signal,
      ),
    enabled: Boolean(activeSourceUrl && activeCategoryUrl),
    staleTime: 5 * 60_000,
    retry: false,
    // 同源同分类翻页时保留旧页内容做半透明过渡；换源/换分类不沿用，避免张冠李戴
    placeholderData: (prev, prevQuery) => {
      const k = prevQuery?.queryKey;
      return k && k[1] === activeSourceUrl && k[2] === activeCategoryUrl ? prev : undefined;
    },
  });

  // 按 bookUrl|origin 去重（对齐原版 ExplorePage）
  const books = React.useMemo(() => {
    const seen = new Set<string>();
    const out: SearchBook[] = [];
    for (const item of rawBooks) {
      const k = `${item.bookUrl}|${item.origin}`;
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(item);
    }
    return out;
  }, [rawBooks]);

  // 实时探活写回：第 1 页返回有书 → 标记可用；首分类第 1 页报错或为空 → 自动标记隐藏（无缝切到下一个有书源）
  React.useEffect(() => {
    if (!activeSourceUrl || page !== 1 || booksPlaceholder) return;
    if (booksSuccess) {
      if (rawBooks.length > 0) {
        markSourceHealth(activeSourceUrl, true);
      } else if (selectedCategoryIdx === 0) {
        markSourceHealth(activeSourceUrl, false);
      }
    } else if (booksError && selectedCategoryIdx === 0) {
      markSourceHealth(activeSourceUrl, false);
    }
  }, [
    activeSourceUrl,
    page,
    selectedCategoryIdx,
    booksSuccess,
    booksPlaceholder,
    booksError,
    rawBooks.length,
    markSourceHealth,
  ]);

  const handleManualRefresh = async () => {
    if (!activeSource || !activeCategory || manualRefreshing) return;
    setManualRefreshing(true);
    try {
      const fresh = await discoverApi.explore({
        url: activeCategory.url,
        bookSource: activeSource.bookSourceUrl,
        page,
        refresh: 1,
      });
      queryClient.setQueryData(queryKey, fresh);
      if (page === 1) {
        markSourceHealth(activeSource.bookSourceUrl, fresh.length > 0);
      }
    } catch {
      void refetchBooks();
    } finally {
      setManualRefreshing(false);
    }
  };

  const handlePageChange = (nextPage: number) => {
    if (nextPage < 1) return;
    setPage(nextPage);
    window.scrollTo({ top: 0, behavior: "smooth" });
  };

  const openBook = (item: SearchBook) => {
    setInspectorBook({
      ...item,
      tocUrl: item.tocUrl ?? undefined,
      origin: item.origin || activeSource?.bookSourceUrl || "",
      originName: item.originName || activeSource?.bookSourceName || "",
    });
  };

  return (
    <PageEnter className="mx-auto w-full max-w-7xl px-4 pt-4 pb-24 sm:px-6 lg:px-8">
      {sourcesLoading ? (
        <div className="space-y-6">
          <div className="space-y-4 border-b border-line/70 pb-4 md:rounded-3xl md:border md:border-line md:bg-elevated/75 md:p-5">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="h-10 w-56 animate-pulse rounded-full bg-subtle" />
              <div className="flex items-center gap-2">
                <div className="h-5 w-44 animate-pulse rounded-full bg-subtle/80" />
                <div className="h-7 w-24 animate-pulse rounded-full bg-subtle" />
              </div>
            </div>
            <div className="flex flex-wrap gap-2 border-t border-line pt-3.5">
              {Array.from({ length: 8 }).map((_, i) => (
                <div
                  key={i}
                  className="h-7 animate-pulse rounded-full bg-subtle/80"
                  style={{ width: `${56 + (i % 3) * 18}px` }}
                />
              ))}
            </div>
          </div>
          <ExploreGridSkeleton reduced={Boolean(reduced)} />
        </div>
      ) : sourcesError ? (
        <EmptyView
          icon={<Compass className="size-5" />}
          title="加载书源列表失败"
          description={sourcesError instanceof Error ? sourcesError.message : "请稍后重试"}
          action={
            <Button variant="primary" size="sm" onClick={() => void refetchSources()}>
              重新加载
            </Button>
          }
        />
      ) : allCandidateSources.length === 0 ? (
        <EmptyView
          icon={<Compass className="size-5" />}
          title="书海画廊尚未开辟"
          description="当前还没有支持发现频道（exploreUrl）的启用书源。前往书源工坊导入优质书源后，即可漫游各大书城榜单与分类。"
          action={
            <Button
              variant="primary"
              size="md"
              onClick={() => navigate("/studio/sources")}
            >
              前往导入书源
              <ArrowRight className="size-4" />
            </Button>
          }
        />
      ) : exploreSources.length === 0 || !activeSource ? (
        <EmptyView
          icon={<ScanSearch className="size-5" />}
          title="暂未检出有内容的书海频道"
          description={`共 ${allCandidateSources.length} 个发现书源，此前检测已自动隐藏 ${hiddenByHealthCount} 个空源或失效源。可点击下方重新全量检测。`}
          action={
            <Button variant="primary" size="sm" onClick={handleToggleProbeAll}>
              <ScanSearch className="size-3.5" />
              重新检测书海源
            </Button>
          }
        />
      ) : (
        <div className="space-y-4 md:space-y-7">
          {/* Integrated Bookstore Channel & Category Bar (calm 2-row editorial header on mobile, elevated bar on desktop) */}
          <section className="border-b border-line/70 pb-2 md:rounded-3xl md:border md:border-line md:bg-elevated/90 md:p-5 md:shadow-[var(--shadow-sm)]">
            {/* Tier 1: Bookstore Showcase Identity & Health Controls */}
            <div className="flex items-center justify-between gap-2 md:flex-wrap md:gap-x-4 md:gap-y-3">
              {/* Source Channel Switcher Popover */}
              <div className="flex min-w-0 items-center gap-2.5">
                <Popover
                  align="start"
                  className="w-[min(20rem,calc(100vw-16px))] p-2.5 sm:w-96"
                  onOpenChange={(open) => {
                    if (!open) setSourceSearch("");
                  }}
                  trigger={
                    <button
                      type="button"
                      className="group/src inline-flex max-w-full cursor-pointer items-center gap-1.5 py-1 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/50 md:gap-2.5 md:rounded-full md:border md:border-line-strong md:bg-bg md:px-3.5 md:py-2 md:shadow-[var(--shadow-sm)] md:transition-[border-color,background-color,box-shadow] md:hover:border-accent/55 md:hover:bg-elevated"
                    >
                      <span className="hidden size-6 shrink-0 items-center justify-center rounded-full bg-accent-soft text-accent transition-transform duration-200 group-hover/src:scale-105 md:flex">
                        <Compass className="size-3.5" />
                      </span>
                      <span className="max-w-[180px] truncate font-serif text-[17px] font-semibold tracking-tight text-ink md:max-w-[230px] md:text-[15px]">
                        {activeSource.bookSourceName}
                      </span>
                      {activeEntry?.verified ? (
                        <CheckCircle2
                          aria-label="已验证有内容"
                          className="size-3.5 shrink-0 text-success"
                        />
                      ) : null}
                      <span className="hidden md:inline-flex">
                        <Badge>{categories.length} 频道</Badge>
                      </span>
                      <ChevronDown className="size-4 shrink-0 text-ink-muted transition-colors group-hover/src:text-ink md:size-3.5" />
                    </button>
                  }
                >
                  {(close) => (
                    <div className="space-y-2">
                      <div className="flex items-center gap-1.5">
                        <Input
                          size="sm"
                          value={sourceSearch}
                          onChange={(e) => setSourceSearch(e.target.value)}
                          placeholder="搜索书海书源…"
                          leftIcon={<Search className="size-3.5" />}
                          clearable
                          onClear={() => setSourceSearch("")}
                          className="flex-1"
                        />
                        {verifiedSources.length > 0 &&
                        unhiddenSources.length > verifiedSources.length ? (
                          <button
                            type="button"
                            onClick={() => setOnlyVerified((v) => !v)}
                            className={cn(
                              "shrink-0 cursor-pointer rounded-lg border px-2 py-1.5 text-[11px] font-medium transition-colors tabular-nums",
                              onlyVerified
                                ? "border-accent bg-accent-soft text-accent"
                                : "border-line bg-bg text-ink-muted hover:text-ink",
                            )}
                            title="切换仅显示已验证有真实书籍内容的书源，或包含尚未检测的候选源"
                          >
                            {onlyVerified
                              ? `仅看有书 (${verifiedSources.length})`
                              : `含待测 (${unhiddenSources.length})`}
                          </button>
                        ) : null}
                      </div>

                      <p className="px-1 text-[11px] text-ink-muted tabular-nums">
                        共 {unhiddenSources.length} 个源 · 已验有书 {verifiedSources.length} · 待测{" "}
                        {untestedSources.length}
                        {hiddenByHealthCount > 0 ? ` · 已滤 ${hiddenByHealthCount} 空源` : ""}
                      </p>

                      <div className="max-h-72 space-y-0.5 overflow-y-auto pr-0.5">
                        {filteredSources.length === 0 ? (
                          <p className="py-6 text-center text-xs text-ink-muted">
                            未找到匹配的书海源
                          </p>
                        ) : (
                          filteredSources.map((entry) => {
                            const selected =
                              entry.source.bookSourceUrl === activeSource.bookSourceUrl;
                            return (
                              <button
                                key={entry.source.bookSourceUrl}
                                type="button"
                                onClick={() =>
                                  handleSelectSource(entry.source.bookSourceUrl, close)
                                }
                                className={cn(
                                  "flex w-full cursor-pointer items-center justify-between gap-2.5 rounded-xl px-3 py-2 text-left transition-colors",
                                  selected
                                    ? "bg-accent-soft/80 text-accent"
                                    : "text-ink hover:bg-subtle",
                                )}
                              >
                                <div className="min-w-0 flex-1">
                                  <div className="flex items-center gap-1.5">
                                    <p className="truncate text-[13px] font-medium">
                                      {entry.source.bookSourceName}
                                    </p>
                                    {entry.verified ? (
                                      <CheckCircle2
                                        aria-label="已验证有内容"
                                        className="size-3.5 shrink-0 text-success"
                                      />
                                    ) : null}
                                  </div>
                                  {entry.source.bookSourceGroup ? (
                                    <p className="truncate text-[11px] text-ink-muted">
                                      {entry.source.bookSourceGroup}
                                    </p>
                                  ) : null}
                                </div>
                                <div className="flex shrink-0 items-center gap-1.5">
                                  <span className="text-[11px] text-ink-muted tabular-nums">
                                    {entry.categories.length}
                                  </span>
                                  {selected ? <Check className="size-3.5" /> : null}
                                </div>
                              </button>
                            );
                          })
                        )}
                      </div>
                    </div>
                  )}
                </Popover>

                {activeSource.bookSourceGroup ? (
                  <span className="hidden rounded-full bg-subtle px-2.5 py-1 text-[11px] text-ink-muted md:inline-flex">
                    {activeSource.bookSourceGroup}
                  </span>
                ) : null}
              </div>

              {/* Transparent Health Stats (desktop) & Quiet Inline Actions (mobile + desktop) */}
              <div className="flex shrink-0 items-center gap-1 text-xs text-ink-muted md:flex-wrap md:gap-x-2.5 md:gap-y-2">
                <span className="hidden tabular-nums md:inline">
                  {onlyVerified && verifiedSources.length > 0
                    ? `有内容源 ${verifiedSources.length} 个（待测 ${untestedSources.length}${hiddenByHealthCount > 0 ? ` · 已滤 ${hiddenByHealthCount} 空源` : ""}）`
                    : untestedSources.length > 0
                      ? `频道 ${unhiddenSources.length} 个（已验有书 ${verifiedSources.length} · 待测 ${untestedSources.length}${hiddenByHealthCount > 0 ? ` · 已滤 ${hiddenByHealthCount} 空源` : ""}）`
                      : `有内容源 ${verifiedSources.length} 个${hiddenByHealthCount > 0 ? `（已滤 ${hiddenByHealthCount} 空源）` : ""}`}
                </span>

                <div className="flex items-center gap-0.5 md:gap-1.5">
                  <button
                    type="button"
                    onClick={handleToggleProbeAll}
                    title={
                      probing
                        ? "停止后台频道检测"
                        : `并发检测书海源（已验 ${verifiedSources.length} · 待测 ${untestedSources.length}）`
                    }
                    className={cn(
                      "inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium transition-colors md:h-7 md:rounded-full md:border md:px-2.5 md:text-[11.5px]",
                      probing
                        ? "bg-accent-soft text-accent md:border-accent/30"
                        : "text-ink-secondary hover:bg-subtle hover:text-ink md:border-line-strong md:bg-elevated",
                    )}
                  >
                    {probing ? (
                      <>
                        <StopIcon className="size-3" />
                        <span>停止</span>
                        <span className="hidden md:inline">检测</span>
                      </>
                    ) : untestedSources.length > 0 ? (
                      <>
                        <ScanSearch className="size-3.5" />
                        <span className="md:hidden">检测</span>
                        <span className="hidden md:inline">检测剩余 {untestedSources.length} 源</span>
                      </>
                    ) : (
                      <>
                        <ScanSearch className="size-3.5" />
                        <span className="md:hidden">重检</span>
                        <span className="hidden md:inline">重检全部 ({allCandidateSources.length})</span>
                      </>
                    )}
                  </button>

                  <button
                    type="button"
                    title="跳过缓存，重新从源站拉取当前分类书单"
                    disabled={booksLoading || manualRefreshing}
                    onClick={() => void handleManualRefresh()}
                    className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:opacity-45 md:h-7 md:rounded-full md:px-2.5 md:text-[11.5px]"
                  >
                    <RefreshCw className={cn("size-3.5", manualRefreshing && "animate-spin")} />
                    <span>刷新</span>
                  </button>
                </div>
              </div>
            </div>

            {/* Integrated Probing Progress Strip */}
            <AnimatePresence initial={false}>
              {probing ? (
                <motion.div
                  key="probing-strip"
                  initial={reduced ? false : { opacity: 0, height: 0, marginTop: 0 }}
                  animate={{ opacity: 1, height: "auto", marginTop: 12 }}
                  exit={reduced ? { opacity: 0 } : { opacity: 0, height: 0, marginTop: 0 }}
                  transition={{ duration: 0.22, ease: EASE_OUT_QUINT }}
                  className="overflow-hidden"
                >
                  <div className="space-y-1.5 rounded-2xl border border-accent/20 bg-accent-soft/45 px-3.5 py-2.5">
                    <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-accent tabular-nums">
                      <span className="inline-flex items-center gap-2 font-medium">
                        <span className="relative flex size-2 shrink-0">
                          <span className="absolute inline-flex size-full animate-ping rounded-full bg-accent opacity-65" />
                          <span className="relative inline-flex size-2 rounded-full bg-accent" />
                        </span>
                        检测频道 {probing.done}/{probing.total} · 有书 {probing.okCount}
                        {hiddenByHealthCount > 0 ? ` · 隐藏 ${hiddenByHealthCount} 空源` : ""}
                      </span>
                      <span className="font-mono text-[11px] opacity-80">
                        {Math.round((probing.done / Math.max(1, probing.total)) * 100)}%
                      </span>
                    </div>
                    <div className="h-1 w-full overflow-hidden rounded-full bg-accent/15">
                      <motion.div
                        className="h-full rounded-full bg-accent"
                        initial={{ width: 0 }}
                        animate={{
                          width: `${Math.max(
                            4,
                            Math.round((probing.done / Math.max(1, probing.total)) * 100),
                          )}%`,
                        }}
                        transition={reduced ? { duration: 0 } : SPRING_SILK}
                      />
                    </div>
                  </div>
                </motion.div>
              ) : null}
            </AnimatePresence>

            {/* Tier 2: Category Tabs (clean editorial underline on mobile, soft spring pill on desktop) */}
            <div className="mt-2 md:mt-4 md:border-t md:border-line md:pt-3.5">
              <motion.div
                initial={false}
                animate={{
                  maxHeight: !shouldCollapse
                    ? "none"
                    : categoriesExpanded
                      ? Math.max(categoryFullHeight + 8, CATEGORY_COLLAPSED_HEIGHT)
                      : CATEGORY_COLLAPSED_HEIGHT,
                }}
                transition={reduced ? { duration: 0 } : SPRING_SILK}
                className="relative overflow-hidden"
              >
                <div
                  ref={categoryInnerRef}
                  className="flex flex-wrap items-center gap-x-4 gap-y-1 py-0.5 md:gap-1.5"
                >
                  {categories.map((cat, idx) => {
                    const active = idx === selectedCategoryIdx;
                    return (
                      <button
                        key={`${cat.title}-${cat.url}-${idx}`}
                        type="button"
                        data-active={active ? "true" : undefined}
                        aria-pressed={active}
                        onClick={() => handleSelectCategory(idx)}
                        className={cn(
                          "relative z-10 cursor-pointer py-1.5 text-[13px] whitespace-nowrap transition-colors outline-none focus-visible:ring-2 focus-visible:ring-accent/60 md:rounded-full md:px-3 md:text-xs md:font-medium",
                          active
                            ? "font-semibold text-accent md:text-accent-ink"
                            : "text-ink-secondary hover:text-ink md:hover:bg-subtle",
                        )}
                      >
                        {active ? (
                          <motion.span
                            layoutId="explore-category-pill"
                            transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                            className="pointer-events-none absolute inset-x-0 bottom-0 h-[2px] rounded-full bg-accent md:inset-0 md:-z-10 md:h-auto md:rounded-full md:shadow-[0_2px_8px_-2px_rgba(178,58,39,0.45)]"
                          />
                        ) : null}
                        {cat.title}
                      </button>
                    );
                  })}
                </div>

                {shouldCollapse && !categoriesExpanded ? (
                  <div
                    aria-hidden
                    className="pointer-events-none absolute inset-x-0 bottom-0 h-5 bg-gradient-to-t from-bg/95 to-transparent md:from-elevated/95"
                  />
                ) : null}
              </motion.div>

              {shouldCollapse ? (
                <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2">
                  {!categoriesExpanded && activeCategoryHidden && activeCategory ? (
                    <span className="inline-flex items-center gap-1.5 text-[11.5px] text-ink-muted">
                      当前选中
                      <span className="rounded-full bg-accent-soft px-2 py-0.5 font-medium text-accent">
                        {activeCategory.title}
                      </span>
                    </span>
                  ) : (
                    <span />
                  )}

                  <button
                    type="button"
                    aria-expanded={categoriesExpanded}
                    onClick={() => setCategoriesExpanded((v) => !v)}
                    className="inline-flex cursor-pointer items-center gap-1 rounded-full border border-dashed border-line-strong bg-bg/60 px-3 py-1 text-xs font-medium text-ink-muted transition-colors hover:border-accent/45 hover:bg-subtle hover:text-ink"
                  >
                    {categoriesExpanded ? (
                      <>
                        收起分类
                        <ChevronUp className="size-3.5" />
                      </>
                    ) : (
                      <>
                        展开全部 {categories.length}
                        <ChevronDown className="size-3.5" />
                      </>
                    )}
                  </button>
                </div>
              ) : null}
            </div>
          </section>

          {/* Gallery Sub-Header: Active Channel Context, Loading Indicator & View Switcher */}
          {activeCategory ? (
            <div className="flex min-h-7 flex-wrap items-center justify-between gap-2 px-0.5">
              <div className="flex min-w-0 items-baseline gap-2.5">
                <h2 className="truncate font-serif text-base font-semibold tracking-tight text-ink">
                  {activeCategory.title}
                </h2>
                <span className="shrink-0 text-xs text-ink-muted tabular-nums">
                  第 {page} 页
                  {!booksLoading && books.length > 0 ? ` · ${books.length} 部` : ""}
                </span>
              </div>

              <div className="flex items-center gap-2">
                <AnimatePresence mode="wait" initial={false}>
                  {booksFetching || manualRefreshing ? (
                    <motion.span
                      key={booksPlaceholder ? "paging" : manualRefreshing ? "refreshing" : "loading"}
                      initial={reduced ? false : { opacity: 0, y: -3 }}
                      animate={{ opacity: 1, y: 0 }}
                      exit={{ opacity: 0, y: -3 }}
                      transition={{ duration: 0.16 }}
                      className="inline-flex items-center gap-1.5 text-[11.5px] text-ink-secondary"
                    >
                      <Loader2 className="size-3 animate-spin text-accent" />
                      <span className="hidden sm:inline">
                        {booksPlaceholder
                          ? `正在翻至第 ${page} 页…`
                          : manualRefreshing
                            ? "正在刷新书单…"
                            : "正在载入频道书单…"}
                      </span>
                    </motion.span>
                  ) : null}
                </AnimatePresence>

                <div
                  role="group"
                  aria-label="书海视图切换"
                  className="inline-flex shrink-0 items-center rounded-xl border border-line bg-elevated p-0.5"
                >
                  <button
                    type="button"
                    onClick={() => changeViewMode("grid")}
                    aria-label="网格视图"
                    aria-pressed={viewMode === "grid"}
                    title="网格视图"
                    className={cn(
                      "flex size-7 cursor-pointer items-center justify-center rounded-lg outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60",
                      viewMode === "grid"
                        ? "bg-accent-soft text-accent shadow-[var(--shadow-sm)]"
                        : "text-ink-muted hover:text-ink",
                    )}
                  >
                    <Grid2x2 className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    onClick={() => changeViewMode("list")}
                    aria-label="列表视图（含简介）"
                    aria-pressed={viewMode === "list"}
                    title="列表视图（含简介）"
                    className={cn(
                      "flex size-7 cursor-pointer items-center justify-center rounded-lg outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60",
                      viewMode === "list"
                        ? "bg-accent-soft text-accent shadow-[var(--shadow-sm)]"
                        : "text-ink-muted hover:text-ink",
                    )}
                  >
                    <List className="size-3.5" />
                  </button>
                </div>
              </div>
            </div>
          ) : null}

          {/* Gallery Book Grid (Stable Height to Prevent Layout Shift) */}
          <div className="relative min-h-[420px]">
            {/* Restrained hairline progress indicator while paginating / refreshing existing content */}
            <AnimatePresence>
              {books.length > 0 && (booksPlaceholder || manualRefreshing) ? (
                <motion.div
                  key="grid-loading-rail"
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  transition={{ duration: 0.18 }}
                  className="pointer-events-none absolute inset-x-0 -top-3 h-0.5 overflow-hidden rounded-full bg-line/70"
                >
                  {reduced ? (
                    <div className="h-full w-1/2 rounded-full bg-accent/70" />
                  ) : (
                    <motion.div
                      className="h-full w-1/3 rounded-full bg-accent/80"
                      initial={{ x: "-100%" }}
                      animate={{ x: "340%" }}
                      transition={{ duration: 1.25, ease: "easeInOut", repeat: Infinity }}
                    />
                  )}
                </motion.div>
              ) : null}
            </AnimatePresence>

            {booksLoading || (booksError && probing !== null) ? (
              <ExploreGridSkeleton reduced={Boolean(reduced)} viewMode={viewMode} />
            ) : booksError ? (
              <motion.div
                initial={reduced ? false : { opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.25, ease: EASE_OUT_QUINT }}
              >
                <EmptyView
                  icon={<Globe className="size-5" />}
                  title="该频道暂时无法加载"
                  description={
                    booksError instanceof Error
                      ? booksError.message
                      : "目标书源响应异常，可尝试重试或切换其他频道。"
                  }
                  action={
                    <div className="flex flex-wrap items-center justify-center gap-2">
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => void handleManualRefresh()}
                      >
                        <RefreshCw className="size-3.5" />
                        重试当前源
                      </Button>
                      {exploreSources.length > 1 ? (
                        <Button variant="primary" size="sm" onClick={switchToNextSource}>
                          <SkipForward className="size-3.5" />
                          换下一个频道
                        </Button>
                      ) : null}
                    </div>
                  }
                />
              </motion.div>
            ) : books.length === 0 ? (
              <motion.div
                initial={reduced ? false : { opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.25, ease: EASE_OUT_QUINT }}
              >
                <EmptyView
                  compact
                  icon={<Compass className="size-5" />}
                  title={page > 1 ? "本页已无更多作品" : "该分类暂无书目"}
                  description={
                    page > 1
                      ? "可返回上一页或切换其他分类继续漫游。"
                      : "不妨切换其他分类频道或更换书海书源。"
                  }
                  action={
                    page > 1 ? (
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => handlePageChange(page - 1)}
                      >
                        返回上一页
                      </Button>
                    ) : undefined
                  }
                />
              </motion.div>
            ) : viewMode === "list" ? (
              <div
                aria-busy={booksFetching || manualRefreshing}
                className={cn(
                  "divide-y divide-line/70 transition-opacity duration-300 md:grid md:grid-cols-2 md:gap-4 md:divide-y-0",
                  booksPlaceholder
                    ? "pointer-events-none opacity-45 select-none"
                    : booksFetching || manualRefreshing
                      ? "opacity-70"
                      : "opacity-100",
                )}
              >
                {books.map((book, idx) => {
                  const key = `${book.bookUrl}-${idx}`;
                  const onShelf =
                    shelfLookup.urls.has(book.bookUrl) ||
                    shelfLookup.titles.has(
                      `${book.name.trim().toLowerCase()}::${(book.author || "").trim().toLowerCase()}`,
                    );
                  const latest = cleanChapterTitle(book.latestChapterTitle);
                  const intro = cleanExploreIntro(book.intro);
                  const kinds = splitExploreKinds(book.kind);

                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => openBook(book)}
                      className="group flex w-full cursor-pointer items-start gap-3.5 py-4 text-left outline-none transition-colors active:bg-subtle/35 focus-visible:ring-2 focus-visible:ring-accent/55 md:rounded-2xl md:border md:border-line md:bg-elevated/85 md:p-4 md:shadow-[var(--shadow-sm)] md:hover:border-line-strong md:hover:bg-elevated"
                    >
                      <BookCover
                        src={book.coverUrl}
                        referer={book.origin || activeSource.bookSourceUrl}
                        title={book.name}
                        author={book.author}
                        pageEdges={false}
                        interactive={false}
                        className="w-[70px] shrink-0 sm:w-[78px]"
                      />

                      <div className="min-w-0 flex-1 space-y-1.5">
                        <div className="flex items-start justify-between gap-2">
                          <h3 className="min-w-0 truncate font-serif text-[15px] leading-snug font-semibold text-ink transition-colors group-hover:text-accent">
                            {book.name}
                          </h3>
                          {onShelf ? (
                            <span className="shrink-0 text-[11px] font-medium text-accent">
                              在书房
                            </span>
                          ) : null}
                        </div>

                        <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-xs text-ink-secondary">
                          <span className="truncate">{book.author || "佚名"}</span>
                          {kinds.map((k) => (
                            <React.Fragment key={k}>
                              <span aria-hidden className="text-ink-muted/50">·</span>
                              <span className="truncate text-[11.5px] text-ink-muted">{k}</span>
                            </React.Fragment>
                          ))}
                          {book.wordCount ? (
                            <>
                              <span aria-hidden className="text-ink-muted/50">·</span>
                              <span className="shrink-0 text-[11.5px] text-ink-muted tabular-nums">
                                {book.wordCount}
                              </span>
                            </>
                          ) : null}
                        </div>

                        {latest ? (
                          <p className="flex min-w-0 items-center gap-1.5 text-[11.5px] text-ink-secondary">
                            <span className="shrink-0 text-[10.5px] font-medium text-accent">
                              最新
                            </span>
                            <span className="truncate">{latest}</span>
                          </p>
                        ) : null}

                        {intro ? (
                          <p className="line-clamp-2 text-xs leading-relaxed text-ink-muted">
                            {intro}
                          </p>
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            ) : (
              <div
                aria-busy={booksFetching || manualRefreshing}
                className={cn(
                  "grid grid-cols-2 gap-x-5 gap-y-7 transition-opacity duration-300 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6",
                  booksPlaceholder
                    ? "pointer-events-none opacity-45 select-none"
                    : booksFetching || manualRefreshing
                      ? "opacity-70"
                      : "opacity-100",
                )}
              >
                {books.map((book, idx) => {
                  const key = `${book.bookUrl}-${idx}`;
                  const onShelf =
                    shelfLookup.urls.has(book.bookUrl) ||
                    shelfLookup.titles.has(
                      `${book.name.trim().toLowerCase()}::${(book.author || "").trim().toLowerCase()}`,
                    );
                  const latest = cleanChapterTitle(book.latestChapterTitle);

                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => openBook(book)}
                      className="group relative flex cursor-pointer flex-col rounded-xl text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
                    >
                      <div className="relative">
                        <BookCover
                          src={book.coverUrl}
                          referer={book.origin || activeSource.bookSourceUrl}
                          title={book.name}
                          author={book.author}
                          pageEdges
                          interactive
                        />
                        {onShelf ? (
                          <span className="pointer-events-none absolute top-2 left-2 z-20 rounded-full border border-accent/20 bg-elevated/92 px-2 py-0.5 text-[10px] font-medium text-accent shadow-sm backdrop-blur-xs">
                            在书房
                          </span>
                        ) : null}
                      </div>

                      <div className="mt-2.5 space-y-0.5 px-0.5">
                        <p className="truncate font-serif text-[14px] font-medium text-ink transition-colors group-hover:text-accent">
                          {book.name}
                        </p>
                        <p className="truncate text-[11.5px] text-ink-muted">
                          {book.author || "佚名"}
                        </p>
                        {latest ? (
                          <p className="truncate text-[10.5px] text-ink-muted/80">{latest}</p>
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {/* Pagination Controls */}
          {books.length > 0 || page > 1 ? (
            <div className="flex items-center justify-center gap-3 pt-2">
              <Button
                size="sm"
                variant="secondary"
                disabled={page <= 1 || booksFetching}
                onClick={() => handlePageChange(page - 1)}
              >
                <ChevronLeft className="size-4" />
                上一页
              </Button>
              <span className="inline-flex min-w-[84px] items-center justify-center gap-1.5 rounded-full border border-line bg-elevated/70 px-3 py-1 font-mono text-xs text-ink-secondary tabular-nums">
                {booksPlaceholder ? <Loader2 className="size-3 animate-spin text-accent" /> : null}
                第 {page} 页
              </span>
              <Button
                size="sm"
                variant="secondary"
                disabled={books.length === 0 || booksFetching}
                onClick={() => handlePageChange(page + 1)}
              >
                下一页
                <ChevronRight className="size-4" />
              </Button>
            </div>
          ) : null}
        </div>
      )}

      {/* Book Inspector Sheet */}
      <BookInspectorSheet
        open={Boolean(inspectorBook)}
        onClose={() => setInspectorBook(null)}
        book={inspectorBook}
        onSourceSwitched={(updated) => {
          setInspectorBook({
            ...updated,
            tocUrl: updated.tocUrl ?? undefined,
          });
        }}
      />
    </PageEnter>
  );
}

/* ---------------- Sub-components ---------------- */

function ExploreGridSkeleton({
  reduced,
  viewMode = "grid",
}: {
  reduced: boolean;
  viewMode?: ExploreViewMode;
}) {
  if (viewMode === "list") {
    return (
      <div
        aria-busy="true"
        aria-label="正在加载频道书单"
        className="divide-y divide-line/70 md:grid md:grid-cols-2 md:gap-4 md:divide-y-0"
      >
        {Array.from({ length: 8 }).map((_, i) => (
          <motion.div
            key={i}
            initial={reduced ? false : { opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{
              duration: 0.24,
              delay: reduced ? 0 : Math.min(i, 7) * 0.03,
              ease: EASE_OUT_QUINT,
            }}
            className="flex gap-3.5 py-4 md:rounded-2xl md:border md:border-line md:bg-elevated/65 md:p-4"
          >
            <div className="aspect-3/4 w-[70px] shrink-0 animate-pulse rounded bg-subtle sm:w-[78px]" />
            <div className="flex-1 space-y-2 py-0.5">
              <div className="h-4 w-2/5 animate-pulse rounded bg-subtle" />
              <div className="h-3 w-1/2 animate-pulse rounded bg-subtle/80" />
              <div className="h-3 w-full animate-pulse rounded bg-subtle/70" />
              <div className="h-3 w-4/5 animate-pulse rounded bg-subtle/70" />
            </div>
          </motion.div>
        ))}
      </div>
    );
  }

  return (
    <div
      aria-busy="true"
      aria-label="正在加载频道书单"
      className="grid grid-cols-2 gap-x-5 gap-y-7 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
    >
      {Array.from({ length: 12 }).map((_, i) => (
        <motion.div
          key={i}
          initial={reduced ? false : { opacity: 0, y: 6 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{
            duration: 0.24,
            delay: reduced ? 0 : Math.min(i, 11) * 0.022,
            ease: EASE_OUT_QUINT,
          }}
          className="space-y-2.5"
        >
          <div className="aspect-3/4 w-full animate-pulse rounded-r-[7px] rounded-l-[2px] bg-subtle" />
          <div className="space-y-1.5 px-0.5">
            <div className="h-3.5 w-4/5 animate-pulse rounded bg-subtle" />
            <div className="h-3 w-1/2 animate-pulse rounded bg-subtle/80" />
            <div className="h-2.5 w-2/3 animate-pulse rounded bg-subtle/60" />
          </div>
        </motion.div>
      ))}
    </div>
  );
}
