import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  BookMarked,
  BookOpen,
  Check,
  ChevronDown,
  EyeOff,
  Filter,
  History,
  Layers,
  Plus,
  Search,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { booksApi, discoverApi, sourcesApi } from "@/api/endpoints";
import type { Book, SearchBook } from "@/api/types";
import { BookInspectorSheet } from "@/features/book/BookInspectorSheet";
import { cn } from "@/lib/cn";
import {
  buildReaderUrl,
  cleanChapterTitle,
  cleanIntro,
  splitSourceGroups,
} from "@/lib/format";
import { useAuthStore } from "@/stores/auth";
import { usePrefsStore } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button } from "@/ui/Button";
import { Badge, EmptyView, Switch } from "@/ui/Controls";
import {
  AnimatePresence,
  motion,
  PageEnter,
  SPRING_POP,
  SPRING_SILK,
  useReducedMotion,
} from "@/ui/motion";
import { Popover, PopoverItem } from "@/ui/Popover";

const SEARCH_BATCH_SIZE = 50;
const EASE_OUT_QUINT = [0.22, 1, 0.36, 1] as const;

interface AggregatedBook {
  key: string;
  primary: SearchBook;
  sources: SearchBook[];
}

interface DiscoverSessionSnapshot {
  userKey: string;
  activeQuery: string;
  exactMatch: boolean;
  selectedGroup: string;
  hideNoLatest: boolean;
  hasSearched: boolean;
  searchedCount: number;
  batchCount: number;
  lastIndex: number;
  isEnd: boolean;
  resultMap: Map<string, AggregatedBook>;
}

let cachedDiscoverSession: DiscoverSessionSnapshot | null = null;

function normalizeBookKey(name?: string, author?: string): string {
  const n = (name ?? "").trim().replace(/\s+/g, "").toLowerCase();
  const a = (author ?? "").trim().replace(/\s+/g, "").toLowerCase();
  return `${n}::${a}`;
}

function scoreCandidate(b: SearchBook): number {
  let score = 0;
  if (b.coverUrl?.trim()) score += 4;
  if (cleanIntro(b.intro).length > 15) score += 3;
  if (cleanChapterTitle(b.latestChapterTitle)) score += 3;
  if (b.wordCount?.trim()) score += 1;
  if (b.kind?.trim()) score += 1;
  return score;
}

function mergeSearchBooks(
  prev: Map<string, AggregatedBook>,
  incoming: SearchBook[],
): Map<string, AggregatedBook> {
  if (!incoming || incoming.length === 0) return prev;
  const next = new Map(prev);
  for (const item of incoming) {
    if (!item.bookUrl || !item.name?.trim()) continue;
    const key = normalizeBookKey(item.name, item.author);
    const existing = next.get(key);
    if (!existing) {
      next.set(key, { key, primary: item, sources: [item] });
      continue;
    }
    const originId = `${item.origin || item.bookSourceUrl || ""}::${item.bookUrl}`;
    const alreadyHas = existing.sources.some(
      (s) => `${s.origin || s.bookSourceUrl || ""}::${s.bookUrl}` === originId,
    );
    const updatedSources = alreadyHas ? existing.sources : [...existing.sources, item];
    const betterPrimary =
      scoreCandidate(item) > scoreCandidate(existing.primary)
        ? {
            ...item,
            coverUrl: item.coverUrl || existing.primary.coverUrl,
            intro: cleanIntro(item.intro) ? item.intro : existing.primary.intro,
            latestChapterTitle:
              cleanChapterTitle(item.latestChapterTitle) || existing.primary.latestChapterTitle,
          }
        : {
            ...existing.primary,
            coverUrl: existing.primary.coverUrl || item.coverUrl,
            intro: cleanIntro(existing.primary.intro) ? existing.primary.intro : item.intro,
            latestChapterTitle:
              cleanChapterTitle(existing.primary.latestChapterTitle) || item.latestChapterTitle,
          };
    next.set(key, {
      key,
      primary: betterPrimary,
      sources: updatedSources,
    });
  }
  return next;
}

function splitKinds(kind?: string | null): string[] {
  if (!kind) return [];
  return kind
    .split(/[,，、/|]+/)
    .map((s) => s.trim())
    .filter(Boolean)
    .slice(0, 2);
}

export default function DiscoverPage() {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const queryClient = useQueryClient();

  const searchTimeout = usePrefsStore((s) => s.searchTimeout);
  const searchHistory = usePrefsStore((s) => s.searchHistory);
  const addSearchHistory = usePrefsStore((s) => s.addSearchHistory);
  const removeSearchHistory = usePrefsStore((s) => s.removeSearchHistory);
  const clearSearchHistory = usePrefsStore((s) => s.clearSearchHistory);

  // Share shelf cache so we know which books are already in library
  const { data: shelf = [] } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(),
    staleTime: 30_000,
  });

  // Load source list lite for source group filtering
  const { data: sourcesLite = [] } = useQuery({
    queryKey: ["sources-lite"],
    queryFn: () => sourcesApi.listLite(),
    staleTime: 60_000,
  });

  const sourceGroups = React.useMemo(() => {
    const set = new Set<string>();
    for (const s of sourcesLite) {
      if (!s.enabled) continue;
      for (const g of splitSourceGroups(s.bookSourceGroup)) {
        set.add(g);
      }
    }
    return Array.from(set).sort((a, b) => a.localeCompare(b, "zh-Hans-CN"));
  }, [sourcesLite]);

  const shelfLookup = React.useMemo(() => {
    const urls = new Set<string>();
    const keys = new Set<string>();
    for (const b of shelf) {
      urls.add(b.bookUrl);
      keys.add(normalizeBookKey(b.name, b.author));
    }
    return { urls, keys };
  }, [shelf]);

  /* ---------------- Search State ---------------- */
  const currentUserKey = useAuthStore((s) => s.user?.username ?? "__local__");
  const initialUrlQuery = (searchParams.get("q") ?? "").trim();
  const restoredSession =
    cachedDiscoverSession &&
    cachedDiscoverSession.userKey === currentUserKey &&
    cachedDiscoverSession.hasSearched &&
    (!initialUrlQuery || cachedDiscoverSession.activeQuery === initialUrlQuery)
      ? cachedDiscoverSession
      : null;

  const [inputValue, setInputValue] = React.useState(
    () => initialUrlQuery || restoredSession?.activeQuery || "",
  );
  const [activeQuery, setActiveQuery] = React.useState(() => restoredSession?.activeQuery ?? "");
  const [exactMatch, setExactMatch] = React.useState(() => restoredSession?.exactMatch ?? false);
  const [selectedGroup, setSelectedGroup] = React.useState(
    () => restoredSession?.selectedGroup ?? "",
  );
  const [hideNoLatest, setHideNoLatest] = React.useState(
    () => restoredSession?.hideNoLatest ?? false,
  );

  const [hasSearched, setHasSearched] = React.useState(
    () => restoredSession?.hasSearched ?? false,
  );
  const [streaming, setStreaming] = React.useState(false);
  const [searchedCount, setSearchedCount] = React.useState(
    () => restoredSession?.searchedCount ?? 0,
  );
  const [batchCount, setBatchCount] = React.useState(() => restoredSession?.batchCount ?? 0);
  const [lastIndex, setLastIndex] = React.useState(() => restoredSession?.lastIndex ?? -1);
  const [isEnd, setIsEnd] = React.useState(() => restoredSession?.isEnd ?? false);
  const [streamError, setStreamError] = React.useState<string | null>(null);
  const [resultMap, setResultMap] = React.useState<Map<string, AggregatedBook>>(
    () => restoredSession?.resultMap ?? new Map(),
  );

  // Inspector & saving state
  const [inspectorBook, setInspectorBook] = React.useState<
    (Partial<Book> & { bookUrl: string; name?: string }) | null
  >(null);
  const [savingBookKey, setSavingBookKey] = React.useState<string | null>(null);

  const stopStreamRef = React.useRef<(() => void) | null>(null);
  const lastUrlQueryRef = React.useRef<string>(restoredSession?.activeQuery ?? "");

  React.useEffect(() => {
    if (!hasSearched || !activeQuery) return;
    cachedDiscoverSession = {
      userKey: currentUserKey,
      activeQuery,
      exactMatch,
      selectedGroup,
      hideNoLatest,
      hasSearched,
      searchedCount,
      batchCount,
      lastIndex,
      isEnd,
      resultMap,
    };
  }, [
    currentUserKey,
    activeQuery,
    exactMatch,
    selectedGroup,
    hideNoLatest,
    hasSearched,
    searchedCount,
    batchCount,
    lastIndex,
    isEnd,
    resultMap,
  ]);

  const stopStream = React.useCallback(() => {
    stopStreamRef.current?.();
    stopStreamRef.current = null;
    setStreaming(false);
  }, []);

  React.useEffect(() => () => stopStream(), [stopStream]);

  const runSearch = React.useCallback(
    (
      keyword: string,
      opts?: {
        append?: boolean;
        fromIndex?: number;
        exact?: boolean;
        group?: string;
        syncUrl?: boolean;
      },
    ) => {
      const trimmed = keyword.trim();
      if (!trimmed) return;

      stopStream();

      const append = opts?.append ?? false;
      const fromIndex = opts?.fromIndex ?? -1;
      const useExact = opts?.exact ?? exactMatch;
      const useGroup = opts?.group ?? selectedGroup;

      setHasSearched(true);
      setActiveQuery(trimmed);
      setInputValue(trimmed);
      setStreamError(null);
      setStreaming(true);
      setBatchCount(0);

      if (!append) {
        setResultMap(new Map());
        setSearchedCount(0);
        setLastIndex(-1);
        setIsEnd(false);
        addSearchHistory(trimmed);
      }

      if (opts?.syncUrl !== false) {
        lastUrlQueryRef.current = trimmed;
        setSearchParams({ q: trimmed }, { replace: true });
      }

      stopStreamRef.current = discoverApi.searchMultiStream(
        {
          key: trimmed,
          exact: useExact,
          bookSourceGroup: useGroup || undefined,
          lastIndex: fromIndex,
          searchSize: SEARCH_BATCH_SIZE,
          timeout: searchTimeout,
        },
        {
          onData: (frame) => {
            setSearchedCount((c) => c + 1);
            setBatchCount((c) => c + 1);
            if (typeof frame.lastIndex === "number") {
              setLastIndex(frame.lastIndex);
            }
            if (frame.data && frame.data.length > 0) {
              setResultMap((prev) => mergeSearchBooks(prev, frame.data));
            }
          },
          onEnd: (end) => {
            if (typeof end?.lastIndex === "number") {
              setLastIndex(end.lastIndex);
            }
            setIsEnd(Boolean(end?.isEnd));
            setStreaming(false);
          },
          onError: (err) => {
            setStreaming(false);
            if (err.message === "没有更多了") {
              setIsEnd(true);
            } else {
              setStreamError(err.message || "流式搜索中断");
              notify.error(err.message || "流式搜索失败");
            }
          },
          onClose: () => {
            setStreaming(false);
          },
        },
      );
    },
    [stopStream, exactMatch, selectedGroup, addSearchHistory, setSearchParams, searchTimeout],
  );

  // Trigger automatically from URL ?q=...
  React.useEffect(() => {
    const q = (searchParams.get("q") ?? "").trim();
    if (q && q !== lastUrlQueryRef.current) {
      lastUrlQueryRef.current = q;
      runSearch(q, { syncUrl: false });
    }
  }, [searchParams, runSearch]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    runSearch(inputValue);
  };

  const handleAddToShelf = async (e: React.MouseEvent, item: AggregatedBook) => {
    e.stopPropagation();
    const book = item.primary;
    setSavingBookKey(item.key);
    try {
      await booksApi.saveBook({
        ...book,
        bookUrl: book.bookUrl,
        tocUrl: book.tocUrl || book.bookUrl,
        origin: book.origin || book.bookSourceUrl || "",
        isInShelf: true,
      });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已将《${book.name}》收入书房`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "收入书房失败");
    } finally {
      setSavingBookKey(null);
    }
  };

  /* ---------------- Sorted & Filtered Results ---------------- */
  const displayedResults = React.useMemo(() => {
    const qNorm = activeQuery.trim().toLowerCase();
    const list: AggregatedBook[] = [];

    for (const item of resultMap.values()) {
      if (hideNoLatest) {
        const validSources = item.sources.filter((s) =>
          Boolean(cleanChapterTitle(s.latestChapterTitle)),
        );
        if (validSources.length === 0) continue;
        const primary = validSources.some((s) => s.bookUrl === item.primary.bookUrl)
          ? item.primary
          : validSources[0]!;
        list.push({ key: item.key, primary, sources: validSources });
      } else {
        list.push(item);
      }
    }

    return list.sort((a, b) => {
      const aExact = a.primary.name.trim().toLowerCase() === qNorm ? 1 : 0;
      const bExact = b.primary.name.trim().toLowerCase() === qNorm ? 1 : 0;
      if (aExact !== bExact) return bExact - aExact;
      if (a.sources.length !== b.sources.length) return b.sources.length - a.sources.length;
      return scoreCandidate(b.primary) - scoreCandidate(a.primary);
    });
  }, [resultMap, hideNoLatest, activeQuery]);

  const progressPct = React.useMemo(() => {
    if (!streaming && hasSearched) return 100;
    return Math.min(96, Math.round((batchCount / SEARCH_BATCH_SIZE) * 100));
  }, [streaming, hasSearched, batchCount]);

  const reduced = useReducedMotion();
  const totalSourceHits = React.useMemo(
    () => displayedResults.reduce((sum, item) => sum + item.sources.length, 0),
    [displayedResults],
  );
  const loadNextBatch = () => runSearch(activeQuery, { append: true, fromIndex: lastIndex });

  return (
    <PageEnter className="mx-auto w-full max-w-6xl px-4 pt-4 pb-24 sm:px-6 lg:px-8">
      <AnimatePresence mode="wait" initial={false}>
      {!hasSearched ? (
        /* ---------------- Calm Editorial Hero State ---------------- */
        <motion.div
          key="discover-hero"
          initial={reduced ? false : { opacity: 0, y: 12 }}
          animate={{ opacity: 1, y: 0 }}
          exit={reduced ? { opacity: 0 } : { opacity: 0, y: -16, scale: 0.985 }}
          transition={{ duration: 0.32, ease: EASE_OUT_QUINT }}
          className="mx-auto flex min-h-[68vh] max-w-2xl flex-col items-center justify-center py-10"
        >
          <div className="mb-8 text-center">
            <span className="inline-flex items-center gap-1.5 rounded-full border border-line bg-elevated/80 px-3 py-1 text-[11px] font-medium tracking-[0.14em] text-accent">
              <Sparkles className="size-3" />
              多源流式检索引擎
            </span>
            <h1 className="mt-4 font-serif text-3xl font-semibold tracking-tight text-ink sm:text-4xl">
              全网搜索
            </h1>
            <p className="mt-2.5 text-sm text-ink-muted">
              输入书名或作者，实时聚合多书源结果并一键加入书架
            </p>
          </div>

          <form onSubmit={handleSubmit} className="w-full space-y-4">
            <motion.div
              layoutId={reduced ? undefined : "discover-search-shell"}
              transition={SPRING_SILK}
              className="surface-glass flex items-center gap-2 rounded-full p-2 pl-5 transition-shadow focus-within:ring-2 focus-within:ring-accent/35"
            >
              <Search className="size-5 shrink-0 text-ink-muted" />
              <input
                type="text"
                autoFocus
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                placeholder="输入书名、作者或关键词…"
                className="min-w-0 flex-1 bg-transparent text-[15px] text-ink outline-none placeholder:text-ink-muted/75"
              />
              <AnimatePresence initial={false}>
                {inputValue ? (
                  <motion.button
                    key="clear"
                    type="button"
                    initial={{ opacity: 0, scale: 0.6 }}
                    animate={{ opacity: 1, scale: 1 }}
                    exit={{ opacity: 0, scale: 0.6 }}
                    transition={SPRING_POP}
                    onClick={() => setInputValue("")}
                    aria-label="清空搜索词"
                    className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-ink-muted hover:bg-subtle hover:text-ink"
                  >
                    <X className="size-3.5" />
                  </motion.button>
                ) : null}
              </AnimatePresence>
              <Button
                type="submit"
                variant="primary"
                size="md"
                disabled={!inputValue.trim()}
              >
                寻书
              </Button>
            </motion.div>

            {/* Subtle Search Options */}
            <div className="flex flex-wrap items-center justify-center gap-4 text-xs text-ink-secondary">
              <label className="inline-flex cursor-pointer items-center gap-2">
                <Switch
                  checked={exactMatch}
                  onChange={setExactMatch}
                  ariaLabel="精准匹配书名"
                />
                <span>精准书名</span>
              </label>

              <SourceGroupPicker
                groups={sourceGroups}
                value={selectedGroup}
                onChange={setSelectedGroup}
              />
            </div>
          </form>

          {/* Search History Capsules */}
          {searchHistory.length > 0 ? (
            <div className="mt-10 w-full space-y-3">
              <div className="flex items-center justify-between px-1">
                <span className="inline-flex items-center gap-1.5 text-xs font-medium text-ink-muted">
                  <History className="size-3.5" />
                  最近检索
                </span>
                <button
                  type="button"
                  onClick={clearSearchHistory}
                  className="cursor-pointer text-xs text-ink-muted transition-colors hover:text-danger"
                >
                  清空历史
                </button>
              </div>
              <motion.div layout className="flex flex-wrap gap-2">
                <AnimatePresence initial={false}>
                  {searchHistory.map((kw, i) => (
                    <motion.div
                      key={kw}
                      layout
                      initial={reduced ? false : { opacity: 0, y: 6 }}
                      animate={{
                        opacity: 1,
                        y: 0,
                        transition: { delay: reduced ? 0 : Math.min(i, 10) * 0.025 },
                      }}
                      exit={{ opacity: 0, scale: 0.85 }}
                      transition={SPRING_POP}
                      className="group inline-flex items-center rounded-full border border-line bg-elevated/80 transition-colors hover:border-accent/40 hover:bg-elevated"
                    >
                      <button
                        type="button"
                        onClick={() => runSearch(kw)}
                        className="cursor-pointer py-1.5 pr-1.5 pl-3.5 text-xs text-ink-secondary transition-colors group-hover:text-ink"
                      >
                        {kw}
                      </button>
                      <button
                        type="button"
                        onClick={() => removeSearchHistory(kw)}
                        aria-label={`删除历史记录 ${kw}`}
                        className="mr-1.5 flex size-5 cursor-pointer items-center justify-center rounded-full text-ink-muted/70 transition-colors hover:bg-subtle hover:text-ink"
                      >
                        <X className="size-3" />
                      </button>
                    </motion.div>
                  ))}
                </AnimatePresence>
              </motion.div>
            </div>
          ) : null}
        </motion.div>
      ) : (
        /* ---------------- Streaming & Results State ---------------- */
        <motion.div
          key="discover-results"
          initial={reduced ? false : { opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.3, ease: EASE_OUT_QUINT }}
          className="space-y-5"
        >
          {/* Compact Top Search Bar (unified rounded-xl on mobile, pill on desktop) */}
          <form
            onSubmit={handleSubmit}
            className="flex flex-col gap-2.5 sm:flex-row sm:items-center"
          >
            <motion.div
              layoutId={reduced ? undefined : "discover-search-shell"}
              transition={SPRING_SILK}
              className="flex flex-1 items-center gap-2 rounded-xl border border-line-strong bg-elevated px-3 py-1.5 transition-[border-color,box-shadow] focus-within:border-accent/60 focus-within:ring-2 focus-within:ring-accent/15 md:rounded-full md:px-4 md:shadow-[var(--shadow-sm)]"
            >
              <Search className="size-4 shrink-0 text-ink-muted" />
              <input
                type="text"
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                placeholder="输入书名或作者…"
                className="min-w-0 flex-1 bg-transparent text-[14px] text-ink outline-none placeholder:text-ink-muted/75"
              />
              {inputValue ? (
                <button
                  type="button"
                  onClick={() => setInputValue("")}
                  aria-label="清空输入"
                  className="flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-full text-ink-muted hover:bg-subtle hover:text-ink"
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
              <button
                type="submit"
                disabled={!inputValue.trim()}
                className="shrink-0 cursor-pointer rounded-lg px-2.5 py-1 text-xs font-semibold text-accent transition-colors hover:bg-accent-soft disabled:opacity-40 md:rounded-full md:bg-accent md:px-3.5 md:font-medium md:text-accent-ink md:hover:brightness-110"
              >
                搜索
              </button>
            </motion.div>

            <div className="flex flex-wrap items-center justify-between gap-2.5 px-0.5 sm:justify-start sm:px-0">
              <label className="inline-flex cursor-pointer items-center gap-1.5 text-xs text-ink-secondary">
                <Switch
                  size="sm"
                  checked={exactMatch}
                  onChange={(v) => {
                    setExactMatch(v);
                    if (activeQuery) runSearch(activeQuery, { exact: v });
                  }}
                  ariaLabel="精准书名"
                />
                <span>精准匹配</span>
              </label>

              <SourceGroupPicker
                groups={sourceGroups}
                value={selectedGroup}
                onChange={(g) => {
                  setSelectedGroup(g);
                  if (activeQuery) runSearch(activeQuery, { group: g });
                }}
              />
            </div>
          </form>

          {/* Streaming Sonar Progress Bar (flat hairline strip on mobile, frosted card on desktop) */}
          <section
            aria-live="polite"
            className="space-y-2 border-b border-line/70 pb-3 md:space-y-2.5 md:rounded-2xl md:border md:border-line md:bg-elevated/85 md:px-4 md:py-3 md:shadow-[var(--shadow-sm)] md:backdrop-blur-md"
          >
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5">
              <div className="flex min-w-0 items-center gap-2 text-xs">
                <AnimatePresence mode="wait" initial={false}>
                  {streaming ? (
                    <motion.span
                      key="live"
                      initial={{ opacity: 0, scale: 0.6 }}
                      animate={{ opacity: 1, scale: 1 }}
                      exit={{ opacity: 0, scale: 0.6 }}
                      className="relative flex size-2 shrink-0"
                    >
                      <span className="absolute inline-flex size-full animate-ping rounded-full bg-accent opacity-60" />
                      <span className="relative inline-flex size-2 rounded-full bg-accent" />
                    </motion.span>
                  ) : (
                    <motion.span
                      key="idle"
                      initial={{ opacity: 0, scale: 0.6 }}
                      animate={{ opacity: 1, scale: 1 }}
                      exit={{ opacity: 0, scale: 0.6 }}
                      transition={SPRING_POP}
                      className={cn(
                        "size-2 shrink-0 rounded-full",
                        streamError ? "bg-danger" : "bg-success",
                      )}
                    />
                  )}
                </AnimatePresence>
                <span className="min-w-0 truncate text-ink-secondary tabular-nums">
                  {streaming ? (
                    <span className="mr-1 text-ink-muted">聚合中 ·</span>
                  ) : null}
                  已搜 <strong className="font-semibold text-ink">{searchedCount}</strong> 源 ·{" "}
                  <strong className="font-semibold text-ink">
                    {displayedResults.length}
                  </strong>{" "}
                  部
                  {totalSourceHits > displayedResults.length ? (
                    <span className="ml-1 hidden text-ink-muted sm:inline">
                      （合并 {totalSourceHits} 条）
                    </span>
                  ) : null}
                  {isEnd && !streaming ? (
                    <span className="ml-1 text-[11px] text-ink-muted">· 已搜遍</span>
                  ) : null}
                </span>
                {activeQuery ? (
                  <span className="hidden max-w-[180px] truncate rounded-full bg-subtle px-2 py-0.5 text-[11px] text-ink-muted sm:inline-block">
                    「{activeQuery}」
                  </span>
                ) : null}
                {streamError ? <Badge tone="danger">{streamError}</Badge> : null}
              </div>

              <div className="flex shrink-0 items-center gap-2">
                <button
                  type="button"
                  aria-pressed={hideNoLatest}
                  onClick={() => setHideNoLatest((v) => !v)}
                  className={cn(
                    "inline-flex cursor-pointer items-center gap-1 rounded-lg px-1.5 py-1 text-xs font-medium transition-colors md:rounded-full md:border md:px-2.5 md:text-[11.5px]",
                    hideNoLatest
                      ? "bg-accent-soft text-accent md:border-accent/40"
                      : "text-ink-muted hover:bg-subtle hover:text-ink md:border-line md:bg-elevated/70",
                  )}
                >
                  <EyeOff className="size-3" />
                  隐藏缺章源
                </button>

                <AnimatePresence mode="wait" initial={false}>
                  {streaming ? (
                    <motion.span
                      key="stop"
                      initial={{ opacity: 0, x: 6 }}
                      animate={{ opacity: 1, x: 0 }}
                      exit={{ opacity: 0, x: -6 }}
                      transition={{ duration: 0.16 }}
                      className="inline-flex"
                    >
                      <button
                        type="button"
                        onClick={stopStream}
                        className="inline-flex cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-semibold text-danger transition-colors hover:bg-danger-soft md:h-7 md:rounded-full md:bg-danger-soft md:px-2.5 md:text-[11.5px]"
                      >
                        <Square className="size-2.5 fill-current" />
                        停止
                      </button>
                    </motion.span>
                  ) : !isEnd ? (
                    <motion.span
                      key="more"
                      initial={{ opacity: 0, x: 6 }}
                      animate={{ opacity: 1, x: 0 }}
                      exit={{ opacity: 0, x: -6 }}
                      transition={{ duration: 0.16 }}
                      className="inline-flex"
                    >
                      <button
                        type="button"
                        onClick={loadNextBatch}
                        className="inline-flex cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-accent transition-colors hover:bg-accent-soft md:h-7 md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-2.5 md:text-[11.5px] md:text-ink"
                      >
                        <Plus className="size-3.5" />
                        加载更多
                      </button>
                    </motion.span>
                  ) : null}
                </AnimatePresence>
              </div>
            </div>

            {/* Spring Progress Rail + restrained indeterminate sheen while streaming */}
            <div className="relative h-1 w-full overflow-hidden rounded-full bg-line">
              <motion.div
                className={cn(
                  "h-full rounded-full transition-colors duration-300",
                  streaming ? "bg-accent" : streamError ? "bg-danger" : "bg-success",
                )}
                initial={{ width: 0 }}
                animate={{ width: `${Math.max(streaming ? 6 : 100, progressPct)}%` }}
                transition={SPRING_SILK}
              />
              {streaming && !reduced ? (
                <motion.div
                  aria-hidden
                  className="pointer-events-none absolute inset-y-0 left-0 w-1/4 bg-gradient-to-r from-transparent via-white/45 to-transparent"
                  initial={{ x: "-100%" }}
                  animate={{ x: "400%" }}
                  transition={{ duration: 1.6, ease: "easeInOut", repeat: Infinity }}
                />
              ) : null}
            </div>
          </section>

          {/* Aggregated Results Grid */}
          {displayedResults.length === 0 ? (
            streaming ? (
              <div className="divide-y divide-line/70 md:grid md:grid-cols-1 md:gap-4 md:divide-y-0 lg:grid-cols-2">
                {Array.from({ length: 6 }).map((_, i) => (
                  <motion.div
                    key={i}
                    initial={reduced ? false : { opacity: 0, y: 8 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ delay: reduced ? 0 : i * 0.05, duration: 0.3, ease: EASE_OUT_QUINT }}
                    className="flex gap-3.5 py-4 md:gap-4 md:rounded-2xl md:border md:border-line md:bg-elevated/65 md:p-4"
                  >
                    <div className="aspect-3/4 w-20 shrink-0 animate-pulse rounded bg-subtle sm:w-22" />
                    <div className="flex-1 space-y-2.5 py-1">
                      <div className="flex items-center justify-between gap-3">
                        <div className="h-4 w-2/5 animate-pulse rounded bg-subtle" />
                        <div className="h-4 w-12 animate-pulse rounded-full bg-subtle/80" />
                      </div>
                      <div className="h-3 w-1/3 animate-pulse rounded bg-subtle/80" />
                      <div className="h-3 w-full animate-pulse rounded bg-subtle/70" />
                      <div className="h-3 w-4/5 animate-pulse rounded bg-subtle/70" />
                      <div className="flex gap-2 pt-1.5">
                        <div className="h-7 w-16 animate-pulse rounded-full bg-subtle/80" />
                        <div className="h-7 w-20 animate-pulse rounded-full bg-subtle/60" />
                      </div>
                    </div>
                  </motion.div>
                ))}
              </div>
            ) : (
              <motion.div
                initial={reduced ? false : { opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.3, ease: EASE_OUT_QUINT }}
              >
                <EmptyView
                  icon={<Search className="size-5" />}
                  title={`未找到与「${activeQuery}」匹配的作品`}
                  description={
                    !isEnd
                      ? "当前批次书源尚未命中，可尝试加载下一批书源或关闭精准匹配。"
                      : "建议换个关键词、关闭精准书名或前往书源工坊扩充书源。"
                  }
                  action={
                    !isEnd ? (
                      <Button variant="primary" size="sm" onClick={loadNextBatch}>
                        加载下一批书源
                      </Button>
                    ) : (
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => {
                          setExactMatch(false);
                          setSelectedGroup("");
                          setHideNoLatest(false);
                        }}
                      >
                        放宽筛选条件
                      </Button>
                    )
                  }
                />
              </motion.div>
            )
          ) : (
            <div className="divide-y divide-line/70 md:grid md:grid-cols-1 md:gap-4 md:divide-y-0 lg:grid-cols-2">
              <AnimatePresence initial={false}>
                {displayedResults.map((item) => (
                  <DiscoverResultCard
                    key={item.key}
                    item={item}
                    reduced={Boolean(reduced)}
                    onShelf={
                      shelfLookup.urls.has(item.primary.bookUrl) || shelfLookup.keys.has(item.key)
                    }
                    saving={savingBookKey === item.key}
                    onOpen={() =>
                      setInspectorBook({
                        ...item.primary,
                        tocUrl: item.primary.tocUrl ?? undefined,
                      })
                    }
                    onRead={(onShelf) =>
                      navigate(
                        buildReaderUrl(
                          item.primary.bookUrl,
                          0,
                          onShelf ? undefined : item.primary.origin || item.primary.bookSourceUrl,
                        ),
                      )
                    }
                    onAdd={(e) => void handleAddToShelf(e, item)}
                  />
                ))}
              </AnimatePresence>
            </div>
          )}

          {/* Streaming tail hint: calm, non-blocking signal that more is arriving */}
          <AnimatePresence>
            {streaming && displayedResults.length > 0 ? (
              <motion.div
                key="tail"
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: 6 }}
                transition={{ duration: 0.2 }}
                className="flex items-center justify-center gap-2 py-2 text-[11.5px] text-ink-muted"
              >
                <span className="size-3.5 animate-spin rounded-full border-2 border-line-strong border-t-accent" />
                更多书源结果正在汇入…
              </motion.div>
            ) : null}
          </AnimatePresence>
        </motion.div>
      )}
      </AnimatePresence>

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

function DiscoverResultCard({
  item,
  reduced,
  onShelf,
  saving,
  onOpen,
  onRead,
  onAdd,
}: {
  item: AggregatedBook;
  reduced: boolean;
  onShelf: boolean;
  saving: boolean;
  onOpen: () => void;
  onRead: (onShelf: boolean) => void;
  onAdd: (e: React.MouseEvent) => void;
}) {
  const book = item.primary;
  const latest = cleanChapterTitle(book.latestChapterTitle);
  const intro = cleanIntro(book.intro);
  const kinds = splitKinds(book.kind);
  const sourceCount = item.sources.length;
  const primaryOrigin = book.originName?.trim() || "未知书源";
  const sourceNames = React.useMemo(
    () =>
      Array.from(
        new Set(item.sources.map((s) => s.originName?.trim()).filter(Boolean) as string[]),
      ),
    [item.sources],
  );
  const shelfState: "shelf" | "saving" | "idle" = onShelf ? "shelf" : saving ? "saving" : "idle";

  return (
    <motion.article
      layout={reduced ? false : "position"}
      initial={reduced ? false : { opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      exit={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.97 }}
      whileHover={reduced ? undefined : { y: -2 }}
      transition={SPRING_SILK}
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="group flex cursor-pointer gap-3.5 py-4 outline-none transition-colors duration-200 active:bg-subtle/35 focus-visible:ring-2 focus-visible:ring-accent/50 md:gap-4 md:rounded-2xl md:border md:border-line md:bg-elevated/85 md:p-4 md:shadow-[var(--shadow-sm)] md:transition-[border-color,background-color,box-shadow] md:hover:border-line-strong md:hover:bg-elevated md:hover:shadow-[var(--shadow-float)]"
    >
      <div className="relative w-20 shrink-0 self-start sm:w-22">
        {sourceCount > 1 ? (
          /* stacked-pages hint for multi-source aggregation */
          <span
            aria-hidden
            className="absolute inset-0 translate-x-1 translate-y-1 rounded-r-[6px] rounded-l-[2px] border border-line bg-subtle"
          />
        ) : null}
        <BookCover
          src={book.coverUrl}
          referer={book.origin || book.bookSourceUrl}
          title={book.name}
          author={book.author}
          pageEdges={false}
          interactive={false}
          className="relative w-full"
        />
      </div>

      <div className="flex min-w-0 flex-1 flex-col justify-between gap-2.5">
        <div className="space-y-1.5">
          <div className="flex items-start justify-between gap-2">
            <h3 className="min-w-0 truncate font-serif text-[16px] leading-snug font-semibold text-ink transition-colors group-hover:text-accent">
              {book.name}
            </h3>
            <AnimatePresence mode="wait" initial={false}>
              <motion.span
                key={sourceCount > 1 ? `multi-${sourceCount}` : "single"}
                initial={reduced ? false : { opacity: 0, scale: 0.7 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.7 }}
                transition={SPRING_POP}
                className="inline-flex shrink-0"
                title={sourceNames.length > 0 ? `可用书源：${sourceNames.join("、")}` : undefined}
              >
                {sourceCount > 1 ? (
                  <span className="inline-flex items-center gap-1 text-[11.5px] font-medium text-accent tabular-nums md:rounded-full md:bg-accent-soft md:px-2 md:py-0.5 md:text-[11px]">
                    <Layers className="size-2.5" />
                    {sourceCount} 源
                  </span>
                ) : (
                  <span className="text-[11px] text-ink-muted md:rounded-full md:bg-subtle md:px-2 md:py-0.5">
                    单源
                  </span>
                )}
              </motion.span>
            </AnimatePresence>
          </div>

          <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-xs text-ink-secondary">
            <span className="truncate">{book.author || "佚名"}</span>
            <span aria-hidden className="text-ink-muted/50">·</span>
            <span className="truncate text-ink-muted">{primaryOrigin}</span>
            {kinds.slice(0, 2).map((k) => (
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
            <p className="line-clamp-2 text-xs leading-relaxed text-ink-muted">{intro}</p>
          ) : null}
        </div>

        <div className="flex items-center gap-3 pt-0.5 md:gap-2">
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onRead(onShelf);
            }}
            className="inline-flex cursor-pointer items-center gap-1 text-xs font-medium text-ink-secondary transition-colors hover:text-accent md:h-7 md:rounded-full md:bg-subtle md:px-2.5 md:text-[11.5px] md:hover:bg-hover md:hover:text-ink"
          >
            <BookOpen className="size-3" />
            {onShelf ? "继续阅读" : "试读"}
          </button>

          <span aria-hidden className="h-3 w-px bg-line md:hidden" />

          <AnimatePresence mode="wait" initial={false}>
            {shelfState === "shelf" ? (
              <motion.span
                key="shelf"
                initial={reduced ? false : { opacity: 0, scale: 0.8 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.8 }}
                transition={SPRING_POP}
                className="inline-flex items-center gap-1 text-xs font-medium text-accent md:h-7 md:rounded-full md:bg-accent-soft md:px-2.5 md:text-[11px]"
              >
                <motion.span
                  initial={reduced ? false : { scale: 0, rotate: -30 }}
                  animate={{ scale: 1, rotate: 0 }}
                  transition={{ ...SPRING_POP, delay: reduced ? 0 : 0.06 }}
                  className="inline-flex"
                >
                  <Check className="size-3" strokeWidth={2.75} />
                </motion.span>
                已在书房
              </motion.span>
            ) : (
              <motion.span
                key="add"
                initial={reduced ? false : { opacity: 0, scale: 0.9 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.9 }}
                transition={{ duration: 0.15 }}
                className="inline-flex"
              >
                <button
                  type="button"
                  disabled={shelfState === "saving"}
                  aria-busy={shelfState === "saving"}
                  onClick={(e) => {
                    e.stopPropagation();
                    onAdd(e);
                  }}
                  className="inline-flex cursor-pointer items-center gap-1 text-xs font-medium text-accent transition-opacity hover:opacity-80 disabled:opacity-50 md:h-7 md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-2.5 md:text-[11.5px] md:text-ink md:hover:border-accent/40 md:hover:text-accent"
                >
                  <BookMarked className="size-3" />
                  {shelfState === "saving" ? "收入中…" : "收入书房"}
                </button>
              </motion.span>
            )}
          </AnimatePresence>
        </div>
      </div>
    </motion.article>
  );
}

function SourceGroupPicker({
  groups,
  value,
  onChange,
}: {
  groups: string[];
  value: string;
  onChange: (group: string) => void;
}) {
  if (groups.length === 0) return null;

  return (
    <Popover
      align="end"
      trigger={
        <button
          type="button"
          className="inline-flex cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink md:h-7 md:rounded-full md:border md:border-line-strong md:bg-elevated md:px-2.5 md:text-[11.5px]"
        >
          <Filter className="size-3 text-ink-muted" />
          <span className="max-w-[120px] truncate">{value || "全部书源分组"}</span>
          <ChevronDown className="size-3 text-ink-muted" />
        </button>
      }
    >
      {(close) => (
        <div className="max-h-64 overflow-y-auto">
          <PopoverItem
            icon={value === "" ? <Check className="size-3.5 text-accent" /> : <span className="size-3.5" />}
            onClick={() => {
              onChange("");
              close();
            }}
          >
            全部分组
          </PopoverItem>
          {groups.map((g) => (
            <PopoverItem
              key={g}
              icon={value === g ? <Check className="size-3.5 text-accent" /> : <span className="size-3.5" />}
              onClick={() => {
                onChange(g);
                close();
              }}
            >
              {g}
            </PopoverItem>
          ))}
        </div>
      )}
    </Popover>
  );
}
