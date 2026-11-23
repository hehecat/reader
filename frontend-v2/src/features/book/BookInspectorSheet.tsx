import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDown,
  ArrowDownNarrowWide,
  ArrowUp,
  ArrowUpNarrowWide,
  BookMarked,
  BookOpen,
  Check,
  ChevronDown,
  Crosshair,
  Download,
  Folder,
  Globe,
  HardDrive,
  ListOrdered,
  Loader2,
  RefreshCw,
  Search,
  Sparkles,
  Square,
  Timer,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { booksApi, discoverApi, featuresApi, sourcesApi } from "@/api/endpoints";
import type { Book, BookChapter, CacheProgress, SearchBook } from "@/api/types";
import { getCachedToc, setCachedToc } from "@/lib/cache";
import { cn } from "@/lib/cn";
import {
  buildReaderUrl,
  cleanChapterTitle,
  cleanIntro,
  formatBytes,
  formatRelativeTime,
} from "@/lib/format";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button, IconButton } from "@/ui/Button";
import { Badge, EmptyView, InlineConfirm, Segmented, Spinner } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import { AnimatePresence, motion, SPRING_SILK, SPRING_SNAPPY } from "@/ui/motion";
import { Popover, PopoverItem } from "@/ui/Popover";
import { Sheet } from "@/ui/Sheet";

export interface BookInspectorSheetProps {
  open: boolean;
  onClose: () => void;
  book: (Partial<Book> & { bookUrl: string; name?: string }) | null;
  initialTab?: "overview" | "toc" | "sources";
  onSourceSwitched?: (updated: Book | SearchBook) => void;
}

type InspectorTab = "overview" | "toc" | "sources";

function isLocalBook(book?: { bookUrl?: string; origin?: string } | null): boolean {
  if (!book) return false;
  return (
    book.origin === "local" ||
    book.origin === "loc_book" ||
    Boolean(book.bookUrl?.startsWith("local://"))
  );
}

function splitKinds(kind?: string | null): string[] {
  if (!kind) return [];
  return kind
    .split(/[,，、/|]+/)
    .map((s) => s.trim())
    .filter(Boolean)
    .slice(0, 3);
}

/* ---------- Chapter number parsing (for "latest chapter" comparison across sources) ---------- */

const CN_DIGITS: Record<string, number> = {
  零: 0,
  〇: 0,
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
const CN_UNITS: Record<string, number> = { 十: 10, 百: 100, 千: 1000 };

function parseCnNumber(raw: string): number {
  if (/^\d+$/.test(raw)) return Number(raw);
  let total = 0;
  let section = 0;
  let num = 0;
  for (const ch of raw) {
    if (ch in CN_DIGITS) {
      num = CN_DIGITS[ch] ?? 0;
    } else if (ch === "万") {
      total += (section + num) * 10_000;
      section = 0;
      num = 0;
    } else if (ch in CN_UNITS) {
      section += (num || 1) * (CN_UNITS[ch] ?? 1);
      num = 0;
    }
  }
  return total + section + num;
}

/** 从章节标题中提取章节序号（第一百二十章 / 第120章 / 120. xxx），失败返回 0 */
function extractChapterNo(title?: string | null): number {
  if (!title) return 0;
  const m = title.match(/第\s*([0-9零〇一二两三四五六七八九十百千万]+)\s*[章节回话集]/);
  if (m?.[1]) return parseCnNumber(m[1]);
  const d = title.match(/(\d{1,6})/);
  return d?.[1] ? Number(d[1]) : 0;
}

/* ---------- Source latency tiers ---------- */

type LatencyTier = "fast" | "mid" | "slow";

function latencyTier(ms: number): LatencyTier {
  if (ms < 1500) return "fast";
  if (ms < 4000) return "mid";
  return "slow";
}

const LATENCY_STYLE: Record<LatencyTier, { label: string; chip: string; dot: string }> = {
  fast: { label: "快", chip: "border-success/25 bg-success-soft text-success", dot: "bg-success" },
  mid: { label: "中", chip: "border-warn/25 bg-warn-soft text-warn", dot: "bg-warn" },
  slow: { label: "慢", chip: "border-danger/25 bg-danger-soft text-danger", dot: "bg-danger" },
};

function formatLatency(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
}

function candidateKey(item: { origin?: string; bookSourceUrl?: string; bookUrl: string }): string {
  return `${item.origin || item.bookSourceUrl || ""}::${item.bookUrl}`;
}

type SourceSort = "smart" | "latest" | "fast";

/** 关键词高亮（大小写不敏感） */
function HighlightText({ text, keyword }: { text: string; keyword: string }) {
  const q = keyword.trim();
  if (!q) return <>{text}</>;
  const lower = text.toLowerCase();
  const lq = q.toLowerCase();
  const parts: React.ReactNode[] = [];
  let i = 0;
  let k = 0;
  while (i < text.length) {
    const at = lower.indexOf(lq, i);
    if (at < 0) {
      parts.push(text.slice(i));
      break;
    }
    if (at > i) parts.push(text.slice(i, at));
    parts.push(
      <mark key={k++} className="rounded-[3px] bg-warn-soft px-0.5 text-warn">
        {text.slice(at, at + q.length)}
      </mark>,
    );
    i = at + q.length;
  }
  return <>{parts}</>;
}

/** 区块标题 */
function SectionTitle({ children, extra }: { children: React.ReactNode; extra?: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <h4 className="flex items-center gap-2 font-serif text-sm font-semibold tracking-wide text-ink">
        <span className="h-3.5 w-[3px] rounded-full bg-accent/70" aria-hidden />
        {children}
      </h4>
      {extra}
    </div>
  );
}

export function BookInspectorSheet({
  open,
  onClose,
  book,
  initialTab = "overview",
  onSourceSwitched,
}: BookInspectorSheetProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const [tab, setTab] = React.useState<InspectorTab>(initialTab);
  const [activeBook, setActiveBook] = React.useState<
    (Partial<Book> & { bookUrl: string; name?: string }) | null
  >(book);

  const bookRef = React.useRef(book);
  bookRef.current = book;

  // Sync incoming book & initialTab when sheet opens or target changes
  React.useEffect(() => {
    if (open && bookRef.current) {
      setActiveBook(bookRef.current);
      setTab(initialTab);
    }
  }, [open, book?.bookUrl, initialTab]);

  // Share shelf & groups cache with LibraryPage / CommandPalette
  const { data: shelf = [] } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(),
    enabled: open,
    staleTime: 30_000,
  });

  const { data: groups = [] } = useQuery({
    queryKey: ["groups"],
    queryFn: () => booksApi.getGroups(),
    enabled: open,
    staleTime: 60_000,
  });

  const shelfMatch = React.useMemo(() => {
    if (!activeBook) return undefined;
    const byUrl = shelf.find((b) => b.bookUrl === activeBook.bookUrl);
    if (byUrl) return byUrl;
    if (activeBook.name) {
      return shelf.find(
        (b) =>
          b.name === activeBook.name &&
          (!activeBook.author || !b.author || b.author === activeBook.author),
      );
    }
    return undefined;
  }, [shelf, activeBook]);

  const onShelf = Boolean(shelfMatch);
  const effectiveBookUrl = shelfMatch?.bookUrl ?? activeBook?.bookUrl ?? "";
  const effectiveOrigin = shelfMatch?.origin ?? activeBook?.origin ?? "";
  const local = isLocalBook({ bookUrl: effectiveBookUrl, origin: effectiveOrigin });

  // Fetch detailed BookInfo for network books to enrich synopsis, wordCount, relatedBooks, etc.
  const { data: bookInfo } = useQuery({
    queryKey: ["book-info", effectiveBookUrl, effectiveOrigin],
    queryFn: ({ signal }) =>
      booksApi.getBookInfo(effectiveBookUrl, effectiveOrigin || undefined, signal),
    enabled: open && Boolean(effectiveBookUrl) && !local,
    staleTime: 5 * 60_000,
    retry: false,
  });

  const displayName =
    shelfMatch?.name || activeBook?.name || bookInfo?.name || "未命名作品";
  const displayAuthor =
    shelfMatch?.author || activeBook?.author || bookInfo?.author || "佚名";
  const displayOriginName =
    shelfMatch?.originName ||
    activeBook?.originName ||
    bookInfo?.originName ||
    (local ? "本地书" : "网络书源");
  const displayCover =
    shelfMatch?.customCoverUrl ||
    shelfMatch?.displayCover ||
    shelfMatch?.coverUrl ||
    activeBook?.coverUrl ||
    bookInfo?.coverUrl;
  const displayKind = shelfMatch?.kind || activeBook?.kind || bookInfo?.kind;
  const displayWordCount =
    shelfMatch?.wordCount || activeBook?.wordCount || bookInfo?.wordCount;
  const displayLatest = cleanChapterTitle(
    shelfMatch?.latestChapterTitle ||
      bookInfo?.latestChapterTitle ||
      activeBook?.latestChapterTitle,
  );
  const displayTocUrl =
    shelfMatch?.tocUrl || bookInfo?.tocUrl || activeBook?.tocUrl || undefined;
  const displayIntro = cleanIntro(
    shelfMatch?.customIntro ||
      shelfMatch?.displayIntro ||
      bookInfo?.intro ||
      shelfMatch?.intro ||
      activeBook?.intro,
  );
  const kinds = React.useMemo(() => splitKinds(displayKind), [displayKind]);

  /* ---------------- Shelf & Group actions ---------------- */
  const [savingShelf, setSavingShelf] = React.useState(false);
  const [removingShelf, setRemovingShelf] = React.useState(false);
  const [changingGroup, setChangingGroup] = React.useState(false);

  const handleAddToShelf = async () => {
    if (!effectiveBookUrl) return;
    setSavingShelf(true);
    try {
      await booksApi.saveBook({
        ...activeBook,
        ...bookInfo,
        bookUrl: effectiveBookUrl,
        tocUrl: displayTocUrl ?? effectiveBookUrl,
        origin: effectiveOrigin,
        originName: displayOriginName,
        name: displayName,
        author: displayAuthor,
        coverUrl: displayCover ?? undefined,
        intro: displayIntro || undefined,
        isInShelf: true,
      });
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已将《${displayName}》收入书房`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "收入书房失败");
    } finally {
      setSavingShelf(false);
    }
  };

  const handleRemoveFromShelf = async () => {
    if (!shelfMatch) return;
    setRemovingShelf(true);
    try {
      await booksApi.deleteBook(shelfMatch.bookUrl);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已从书房移出《${displayName}》`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "移出书房失败");
    } finally {
      setRemovingShelf(false);
    }
  };

  const currentGroupId = React.useMemo(() => {
    if (!shelfMatch) return 0;
    if (shelfMatch.groupIds && shelfMatch.groupIds.length > 0) {
      return shelfMatch.groupIds[0] ?? 0;
    }
    return shelfMatch.group ?? 0;
  }, [shelfMatch]);

  const currentGroupName = React.useMemo(() => {
    if (!currentGroupId) return "未分组";
    return groups.find((g) => g.id === currentGroupId)?.name ?? "未分组";
  }, [currentGroupId, groups]);

  const handleSetGroup = async (groupId: number, closePopover: () => void) => {
    if (!shelfMatch) return;
    setChangingGroup(true);
    try {
      await booksApi.setBookGroup(shelfMatch.bookUrl, groupId);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      const targetName =
        groupId === 0
          ? "未分组"
          : (groups.find((g) => g.id === groupId)?.name ?? "分组");
      notify.success(`已移至「${targetName}」`);
      closePopover();
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "修改分组失败");
    } finally {
      setChangingGroup(false);
    }
  };

  const handleStartReading = (chapterIndex?: number) => {
    if (!effectiveBookUrl) return;
    const idx =
      chapterIndex !== undefined
        ? chapterIndex
        : onShelf
          ? shelfMatch?.durChapterIndex ?? 0
          : 0;
    navigate(
      buildReaderUrl(
        effectiveBookUrl,
        idx,
        !onShelf && effectiveOrigin ? effectiveOrigin : undefined,
      ),
    );
    onClose();
  };

  /* ---------------- Offline Cache (Overview Tab) ---------------- */
  const {
    data: cacheInfo,
    isLoading: cacheInfoLoading,
    refetch: refetchCacheInfo,
  } = useQuery({
    queryKey: ["book-cache-info", shelfMatch?.bookUrl],
    queryFn: () => featuresApi.getBookCacheInfo(shelfMatch!.bookUrl),
    enabled: open && onShelf && !local && Boolean(shelfMatch?.bookUrl),
    staleTime: 15_000,
  });

  const [cacheProgress, setCacheProgress] = React.useState<CacheProgress | null>(null);
  const [caching, setCaching] = React.useState(false);
  const [clearingCache, setClearingCache] = React.useState(false);
  const cacheStreamStopRef = React.useRef<(() => void) | null>(null);

  const stopCacheStream = React.useCallback(() => {
    cacheStreamStopRef.current?.();
    cacheStreamStopRef.current = null;
  }, []);

  React.useEffect(() => {
    if (!open) {
      stopCacheStream();
      setCaching(false);
      setCacheProgress(null);
    }
  }, [open, effectiveBookUrl, stopCacheStream]);

  React.useEffect(() => () => stopCacheStream(), [stopCacheStream]);

  const handleStartCache = () => {
    if (!shelfMatch) return;
    stopCacheStream();
    setCaching(true);
    setCacheProgress({
      cached: cacheInfo?.cacheChapterCount ?? 0,
      total: shelfMatch.totalChapterNum || 0,
      title: "正在准备整书缓存…",
      finished: false,
      cancelled: false,
      error: null,
    });
    cacheStreamStopRef.current = featuresApi.cacheBookStream(
      { url: shelfMatch.bookUrl },
      {
        onData: (progress) => {
          setCacheProgress(progress);
          if (progress.finished || progress.cancelled) {
            setCaching(false);
            void refetchCacheInfo();
            if (progress.error) {
              notify.error(progress.error);
            } else if (progress.cancelled) {
              notify.info("已取消整书缓存");
            } else {
              notify.success(`《${displayName}》离线缓存完成`);
            }
          }
        },
        onError: (err) => {
          setCaching(false);
          notify.error(err.message || "缓存连接中断");
          void refetchCacheInfo();
        },
        onClose: () => {
          setCaching(false);
          void refetchCacheInfo();
        },
      },
    );
  };

  const handleCancelCache = async () => {
    if (!shelfMatch) return;
    try {
      await featuresApi.cancelCacheBook({ url: shelfMatch.bookUrl });
    } catch {
      // ignore if already finished
    } finally {
      stopCacheStream();
      setCaching(false);
      void refetchCacheInfo();
    }
  };

  const handleDeleteCache = async () => {
    if (!shelfMatch) return;
    setClearingCache(true);
    try {
      await featuresApi.deleteBookCache(shelfMatch.bookUrl);
      setCacheProgress(null);
      await refetchCacheInfo();
      notify.success("已清理本书离线缓存");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "清理缓存失败");
    } finally {
      setClearingCache(false);
    }
  };

  /* ---------------- TOC Tab ---------------- */
  const [tocKeyword, setTocKeyword] = React.useState("");
  const [tocReversed, setTocReversed] = React.useState(false);
  const [tocRefreshing, setTocRefreshing] = React.useState(false);
  const virtuosoRef = React.useRef<VirtuosoHandle>(null);

  React.useEffect(() => {
    setTocKeyword("");
  }, [effectiveBookUrl]);

  const {
    data: chapters = [],
    isLoading: tocLoading,
    error: tocError,
    refetch: refetchToc,
  } = useQuery({
    queryKey: ["chapters", effectiveBookUrl, effectiveOrigin],
    queryFn: async ({ signal }) => {
      const list = await booksApi.getChapters(
        {
          url: effectiveBookUrl,
          tocUrl: displayTocUrl,
          bookSource: effectiveOrigin || undefined,
        },
        signal,
      );
      if (list.length > 0) {
        void setCachedToc(effectiveBookUrl, list);
      }
      return list;
    },
    enabled: open && tab === "toc" && Boolean(effectiveBookUrl),
    staleTime: 5 * 60_000,
    retry: false,
  });

  // Warm placeholder from IndexedDB if query hasn't populated yet
  const [idbChapters, setIdbChapters] = React.useState<BookChapter[] | null>(null);
  React.useEffect(() => {
    if (!open || tab !== "toc" || !effectiveBookUrl || chapters.length > 0) return;
    let cancelled = false;
    void getCachedToc(effectiveBookUrl).then((cached) => {
      if (!cancelled && cached && cached.length > 0) {
        setIdbChapters(cached);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [open, tab, effectiveBookUrl, chapters.length]);

  const effectiveChapters = React.useMemo(
    () => (chapters.length > 0 ? chapters : (idbChapters ?? [])),
    [chapters, idbChapters],
  );

  const handleRefreshToc = async () => {
    if (!effectiveBookUrl) return;
    setTocRefreshing(true);
    try {
      const fresh = await booksApi.getChapters({
        url: effectiveBookUrl,
        tocUrl: displayTocUrl,
        bookSource: effectiveOrigin || undefined,
        refresh: 1,
      });
      queryClient.setQueryData(["chapters", effectiveBookUrl, effectiveOrigin], fresh);
      if (fresh.length > 0) void setCachedToc(effectiveBookUrl, fresh);
      notify.success(`目录已刷新，共 ${fresh.length} 章`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "刷新目录失败");
    } finally {
      setTocRefreshing(false);
    }
  };

  const filteredChapters = React.useMemo(() => {
    const q = tocKeyword.trim().toLowerCase();
    const base = q
      ? effectiveChapters.filter((ch) => ch.title.toLowerCase().includes(q))
      : effectiveChapters;
    return tocReversed ? [...base].reverse() : base;
  }, [effectiveChapters, tocKeyword, tocReversed]);

  const tocQuery = tocKeyword.trim();
  const tocHitCount = React.useMemo(
    () => (tocQuery ? filteredChapters.filter((c) => !c.isVolume).length : 0),
    [filteredChapters, tocQuery],
  );
  const totalPlainChapters = React.useMemo(
    () => effectiveChapters.filter((c) => !c.isVolume).length,
    [effectiveChapters],
  );

  const currentChapterIndex = onShelf && shelfMatch ? shelfMatch.durChapterIndex : -1;
  const currentPosInList = React.useMemo(
    () =>
      currentChapterIndex < 0
        ? -1
        : filteredChapters.findIndex((c) => c.index === currentChapterIndex),
    [filteredChapters, currentChapterIndex],
  );
  const currentChapterInBook = React.useMemo(
    () =>
      currentChapterIndex < 0
        ? undefined
        : effectiveChapters.find((c) => c.index === currentChapterIndex),
    [effectiveChapters, currentChapterIndex],
  );

  const [tocRange, setTocRange] = React.useState<{ startIndex: number; endIndex: number }>({
    startIndex: 0,
    endIndex: 0,
  });
  const currentDirection: "up" | "down" | null =
    currentPosInList < 0 || tocRange.endIndex === 0
      ? null
      : currentPosInList < tocRange.startIndex
        ? "up"
        : currentPosInList > tocRange.endIndex
          ? "down"
          : null;

  const pendingLocateRef = React.useRef(false);

  const scrollToCurrentChapter = () => {
    if (currentChapterIndex < 0) return;
    if (currentPosInList < 0) {
      // 当前章被关键词过滤掉：清空筛选后再定位
      if (tocQuery) {
        pendingLocateRef.current = true;
        setTocKeyword("");
      }
      return;
    }
    virtuosoRef.current?.scrollToIndex({
      index: currentPosInList,
      align: "center",
      behavior: "smooth",
    });
  };

  React.useEffect(() => {
    if (!pendingLocateRef.current || currentPosInList < 0) return;
    pendingLocateRef.current = false;
    const raf = requestAnimationFrame(() => {
      virtuosoRef.current?.scrollToIndex({ index: currentPosInList, align: "center" });
    });
    return () => cancelAnimationFrame(raf);
  }, [currentPosInList]);

  // 关键词变化时回到列表顶部，命中结果一目了然
  React.useEffect(() => {
    if (!tocQuery) return;
    virtuosoRef.current?.scrollToIndex({ index: 0, align: "start" });
  }, [tocQuery]);

  const tocReadPct =
    currentChapterIndex >= 0 && totalPlainChapters > 0
      ? Math.min(100, Math.round(((currentChapterIndex + 1) / effectiveChapters.length) * 100))
      : 0;

  /* ---------------- Sources Tab ---------------- */
  const [streamCandidates, setStreamCandidates] = React.useState<SearchBook[]>([]);
  const [searchingSources, setSearchingSources] = React.useState(false);
  const [searchedSourceCount, setSearchedSourceCount] = React.useState(0);
  const [switchingUrl, setSwitchingUrl] = React.useState<string | null>(null);
  const [sourceTimings, setSourceTimings] = React.useState<Record<string, number>>({});
  const [sourceSort, setSourceSort] = React.useState<SourceSort>("smart");
  const streamStartRef = React.useRef(0);
  const sourceStreamStopRef = React.useRef<(() => void) | null>(null);

  const stopSourceStream = React.useCallback(() => {
    sourceStreamStopRef.current?.();
    sourceStreamStopRef.current = null;
    setSearchingSources(false);
  }, []);

  React.useEffect(() => {
    if (!open) {
      stopSourceStream();
      setStreamCandidates([]);
      setSearchedSourceCount(0);
      setSourceTimings({});
    }
  }, [open, stopSourceStream]);

  React.useEffect(() => {
    stopSourceStream();
    setStreamCandidates([]);
    setSearchedSourceCount(0);
    setSourceTimings({});
  }, [effectiveBookUrl, stopSourceStream]);

  React.useEffect(() => () => stopSourceStream(), [stopSourceStream]);

  const {
    data: cachedAlternatives = [],
    isLoading: loadingAlternatives,
  } = useQuery({
    queryKey: ["available-sources", shelfMatch?.bookUrl],
    queryFn: () => sourcesApi.availableForBook(shelfMatch!.bookUrl, 0),
    enabled: open && tab === "sources" && onShelf && !local && Boolean(shelfMatch?.bookUrl),
    staleTime: 60_000,
    retry: false,
  });

  const startAlternativeStream = React.useCallback(() => {
    if (!effectiveBookUrl || local) return;
    stopSourceStream();
    setSearchingSources(true);
    setSearchedSourceCount(0);
    streamStartRef.current = performance.now();

    const mergeIncoming = (items: SearchBook[]) => {
      if (!items || items.length === 0) return;
      const elapsed = performance.now() - streamStartRef.current;
      setSourceTimings((prev) => {
        let next: Record<string, number> | null = null;
        for (const item of items) {
          const key = candidateKey(item);
          if (prev[key] === undefined) {
            next ??= { ...prev };
            next[key] = elapsed;
          }
        }
        return next ?? prev;
      });
      setStreamCandidates((prev) => {
        const map = new Map<string, SearchBook>();
        for (const item of prev) {
          map.set(candidateKey(item), item);
        }
        for (const item of items) {
          const key = candidateKey(item);
          if (!map.has(key)) map.set(key, item);
        }
        return Array.from(map.values());
      });
    };

    if (effectiveOrigin) {
      sourceStreamStopRef.current = discoverApi.searchAlternativesStream(
        { url: effectiveBookUrl, bookSource: effectiveOrigin },
        {
          onData: (frame) => {
            setSearchedSourceCount((c) => c + 1);
            mergeIncoming(frame.data ?? []);
          },
          onEnd: () => {
            setSearchingSources(false);
          },
          onError: (err) => {
            // Fallback for preview books if bookSourceStream fails: search by exact title
            if (!onShelf && displayName && displayName !== "未命名作品") {
              sourceStreamStopRef.current = discoverApi.searchMultiStream(
                { key: displayName, exact: true, searchSize: 40 },
                {
                  onData: (frame) => {
                    setSearchedSourceCount((c) => c + 1);
                    const matching = (frame.data ?? []).filter(
                      (b) => b.name.trim() === displayName.trim(),
                    );
                    mergeIncoming(matching);
                  },
                  onEnd: () => setSearchingSources(false),
                  onError: () => setSearchingSources(false),
                  onClose: () => setSearchingSources(false),
                },
              );
            } else {
              setSearchingSources(false);
              notify.error(err.message || "全源重搜失败");
            }
          },
          onClose: () => {
            setSearchingSources(false);
          },
        },
      );
    } else if (displayName && displayName !== "未命名作品") {
      sourceStreamStopRef.current = discoverApi.searchMultiStream(
        { key: displayName, exact: true, searchSize: 40 },
        {
          onData: (frame) => {
            setSearchedSourceCount((c) => c + 1);
            const matching = (frame.data ?? []).filter(
              (b) => b.name.trim() === displayName.trim(),
            );
            mergeIncoming(matching);
          },
          onEnd: () => setSearchingSources(false),
          onError: (err) => {
            setSearchingSources(false);
            notify.error(err.message || "搜索候选书源失败");
          },
          onClose: () => setSearchingSources(false),
        },
      );
    } else {
      setSearchingSources(false);
    }
  }, [effectiveBookUrl, effectiveOrigin, local, onShelf, displayName, stopSourceStream]);

  // Auto-discover alternatives for preview books when user switches to sources tab
  const autoTriggeredRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (
      open &&
      tab === "sources" &&
      !onShelf &&
      !local &&
      effectiveBookUrl &&
      autoTriggeredRef.current !== effectiveBookUrl
    ) {
      autoTriggeredRef.current = effectiveBookUrl;
      startAlternativeStream();
    }
  }, [open, tab, onShelf, local, effectiveBookUrl, startAlternativeStream]);

  React.useEffect(() => {
    if (!open) autoTriggeredRef.current = null;
  }, [open]);

  const allCandidates = React.useMemo(() => {
    const map = new Map<string, SearchBook>();
    for (const item of [...cachedAlternatives, ...streamCandidates]) {
      const key = candidateKey(item);
      if (!map.has(key)) map.set(key, item);
    }
    return Array.from(map.values());
  }, [cachedAlternatives, streamCandidates]);

  // 历史测速（与书源工作台共享缓存），用于缓存候选源的耗时兜底
  const { data: sourceStats = [] } = useQuery({
    queryKey: ["studio", "sourceStats"],
    queryFn: () => sourcesApi.stats(),
    enabled: open && tab === "sources" && !local,
    staleTime: 60_000,
    retry: false,
  });
  const statLatencyMap = React.useMemo(() => {
    const map = new Map<string, number>();
    for (const s of sourceStats) {
      if (s.avgLatencyMs > 0) map.set(s.sourceUrl, s.avgLatencyMs);
    }
    return map;
  }, [sourceStats]);

  const currentLatestNo = React.useMemo(() => extractChapterNo(displayLatest), [displayLatest]);

  const decoratedCandidates = React.useMemo(() => {
    const rows = allCandidates.map((cand, order) => {
      const origin = cand.origin || cand.bookSourceUrl || "";
      const key = candidateKey(cand);
      const measured = sourceTimings[key];
      const historic = origin ? statLatencyMap.get(origin) : undefined;
      const latencyMs = measured ?? historic;
      const latest = cleanChapterTitle(cand.latestChapterTitle);
      return {
        cand,
        key,
        origin,
        order,
        latest,
        chapterNo: extractChapterNo(latest),
        latencyMs,
        latencySource: measured !== undefined ? ("live" as const) : historic ? ("history" as const) : null,
        isCurrent:
          cand.bookUrl === effectiveBookUrl || (Boolean(origin) && origin === effectiveOrigin),
      };
    });
    const maxNo = rows.reduce((m, r) => Math.max(m, r.chapterNo), 0);
    const sorted = [...rows].sort((a, b) => {
      if (a.isCurrent !== b.isCurrent) return a.isCurrent ? -1 : 1;
      if (sourceSort === "latest") {
        if (b.chapterNo !== a.chapterNo) return b.chapterNo - a.chapterNo;
      } else if (sourceSort === "fast") {
        const la = a.latencyMs ?? Number.POSITIVE_INFINITY;
        const lb = b.latencyMs ?? Number.POSITIVE_INFINITY;
        if (la !== lb) return la - lb;
      }
      return a.order - b.order;
    });
    return { rows: sorted, maxNo };
  }, [allCandidates, sourceTimings, statLatencyMap, effectiveBookUrl, effectiveOrigin, sourceSort]);

  const latencySummary = React.useMemo(() => {
    const out = { fast: 0, mid: 0, slow: 0 };
    for (const r of decoratedCandidates.rows) {
      if (r.latencyMs !== undefined) out[latencyTier(r.latencyMs)] += 1;
    }
    return out;
  }, [decoratedCandidates]);

  const handleSwitchSource = async (candidate: SearchBook, saveFirst = false) => {
    const targetOrigin = candidate.origin || candidate.bookSourceUrl || "";
    setSwitchingUrl(candidate.bookUrl);
    try {
      if (shelfMatch) {
        const updated = await booksApi.switchSource({
          bookUrl: shelfMatch.bookUrl,
          newUrl: candidate.bookUrl,
          bookSourceUrl: targetOrigin,
          name: candidate.name || displayName,
          author: candidate.author || displayAuthor,
        });
        setActiveBook(updated);
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: ["shelf"] }),
          queryClient.invalidateQueries({ queryKey: ["chapters"] }),
          queryClient.invalidateQueries({ queryKey: ["available-sources"] }),
        ]);
        notify.success(`已切换书源至「${candidate.originName || targetOrigin}」`);
        onSourceSwitched?.(updated);
      } else if (saveFirst) {
        await booksApi.saveBook({
          ...candidate,
          bookUrl: candidate.bookUrl,
          tocUrl: candidate.tocUrl || candidate.bookUrl,
          origin: targetOrigin,
          isInShelf: true,
        });
        setActiveBook({
          ...candidate,
          tocUrl: candidate.tocUrl || candidate.bookUrl,
          origin: targetOrigin,
        });
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: ["shelf"] }),
          queryClient.invalidateQueries({ queryKey: ["groups"] }),
        ]);
        notify.success(`已以「${candidate.originName || "所选书源"}」收入书房`);
        onSourceSwitched?.(candidate);
      } else {
        setActiveBook({
          ...candidate,
          tocUrl: candidate.tocUrl || candidate.bookUrl,
          origin: targetOrigin,
        });
        notify.success(`已切换预览源至「${candidate.originName || targetOrigin}」`);
        onSourceSwitched?.(candidate);
      }
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "换源失败");
    } finally {
      setSwitchingUrl(null);
    }
  };

  /* ---------------- Progress calculation ---------------- */
  const cachePct = React.useMemo(() => {
    if (cacheProgress && cacheProgress.total > 0) {
      return Math.min(100, Math.round((cacheProgress.cached / cacheProgress.total) * 100));
    }
    if (cacheInfo && shelfMatch?.totalChapterNum && shelfMatch.totalChapterNum > 0) {
      return Math.min(
        100,
        Math.round((cacheInfo.cacheChapterCount / shelfMatch.totalChapterNum) * 100),
      );
    }
    return 0;
  }, [cacheProgress, cacheInfo, shelfMatch?.totalChapterNum]);

  const hasStartedReading = Boolean(
    shelfMatch && (shelfMatch.durChapterIndex > 0 || shelfMatch.durChapterTime > 0),
  );

  const readPct =
    shelfMatch && shelfMatch.totalChapterNum > 0
      ? Math.min(
          100,
          Math.round(((shelfMatch.durChapterIndex + 1) / shelfMatch.totalChapterNum) * 100),
        )
      : 0;
  const remainingChapters =
    shelfMatch && shelfMatch.totalChapterNum > 0
      ? Math.max(0, shelfMatch.totalChapterNum - 1 - shelfMatch.durChapterIndex)
      : 0;
  const cachedCount = cacheProgress?.cached ?? cacheInfo?.cacheChapterCount ?? 0;
  const fullyCached =
    Boolean(shelfMatch?.totalChapterNum) && cachedCount >= (shelfMatch?.totalChapterNum ?? 0);

  const [introExpanded, setIntroExpanded] = React.useState(false);
  React.useEffect(() => setIntroExpanded(false), [effectiveBookUrl]);
  const introLong = displayIntro.length > 160 || displayIntro.split("\n").length > 4;

  const tabOptions = React.useMemo(
    () =>
      [
        { value: "overview" as const, label: "概览" },
        {
          value: "toc" as const,
          label: "目录",
          badge:
            effectiveChapters.length > 0 ? (
              <span className="text-[10px] text-ink-muted tabular-nums">
                {effectiveChapters.length}
              </span>
            ) : undefined,
        },
        {
          value: "sources" as const,
          label: "换源",
          badge:
            allCandidates.length > 0 ? (
              <span className="text-[10px] text-accent tabular-nums">
                {allCandidates.length}
              </span>
            ) : undefined,
        },
      ] as const,
    [effectiveChapters.length, allCandidates.length],
  );

  return (
    <Sheet
      open={open && Boolean(activeBook)}
      onClose={onClose}
      size="lg"
      title={displayName}
      subtitle={`${displayAuthor} · ${displayOriginName}`}
      bodyClassName="flex flex-col overflow-hidden p-0"
    >
      {/* Sub-tab Header (underline tabs on mobile, Segmented on desktop) */}
      <div className="flex shrink-0 items-center justify-between gap-3 border-b border-line px-5 sm:py-3">
        <div className="flex items-center gap-6 sm:hidden" role="tablist" aria-label="书籍详情页签">
          {tabOptions.map((opt) => {
            const active = tab === opt.value;
            return (
              <button
                key={opt.value}
                type="button"
                role="tab"
                aria-selected={active}
                onClick={() => setTab(opt.value)}
                className={cn(
                  "relative inline-flex cursor-pointer items-center gap-1.5 border-b-2 py-2.5 text-[13px] transition-colors",
                  active
                    ? "border-accent font-semibold text-accent"
                    : "border-transparent text-ink-muted hover:text-ink",
                )}
              >
                <span>{opt.label}</span>
                {"badge" in opt ? opt.badge : null}
              </button>
            );
          })}
        </div>
        <div className="hidden sm:block">
          <Segmented
            layoutId="book-inspector-tab"
            options={tabOptions}
            value={tab}
            onChange={setTab}
            size="sm"
          />
        </div>
        {onShelf && shelfMatch?.durChapterTime ? (
          <span className="hidden items-center gap-1.5 text-[11px] text-ink-muted sm:inline-flex">
            <span className="size-1.5 rounded-full bg-success/80" aria-hidden />
            上次阅读 {formatRelativeTime(shelfMatch.durChapterTime)}
          </span>
        ) : null}
      </div>

      {/* Tab 1: Overview */}
      {tab === "overview" ? (
        <div className="h-[66dvh] shrink-0 overflow-y-auto px-5 py-4 sm:h-auto sm:min-h-0 sm:flex-1 sm:py-5">
          <div className="space-y-5 sm:space-y-6">
            {/* Editorial Hero (borderless horizontal layout on mobile, gradient card on desktop) */}
            <div className="relative border-b border-line/70 pb-5 sm:overflow-hidden sm:rounded-3xl sm:border sm:border-line sm:bg-gradient-to-br sm:from-accent-soft/50 sm:via-elevated sm:to-elevated sm:p-5">
              <div
                aria-hidden
                className="pointer-events-none absolute -top-16 -right-16 hidden size-48 rounded-full bg-accent/10 blur-3xl sm:block"
              />
              <div className="relative flex items-start gap-4 sm:gap-5">
                <div className="shrink-0">
                  <BookCover
                    src={displayCover}
                    referer={effectiveOrigin}
                    title={displayName}
                    author={displayAuthor}
                    pageEdges
                    interactive={false}
                    className="w-[78px] sm:w-28"
                  />
                </div>

                <div className="min-w-0 flex-1 space-y-2 text-left sm:space-y-3">
                  <div>
                    <h3 className="font-serif text-lg leading-snug font-semibold tracking-tight text-balance text-ink sm:text-[22px]">
                      {displayName}
                    </h3>
                    <p className="mt-0.5 text-xs text-ink-secondary sm:mt-1 sm:text-[13px]">
                      {displayAuthor}
                      <span className="mx-1.5 text-ink-muted/60">·</span>
                      <span className="text-ink-muted">{displayOriginName}</span>
                    </p>
                  </div>

                  {/* Metadata (quiet middle-dot typography on mobile, badges on desktop) */}
                  <p className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-[11.5px] text-ink-muted sm:hidden">
                    <span className={cn("font-medium", local ? "text-success" : "text-accent")}>
                      {local ? "本地" : "网络"}
                    </span>
                    {kinds.map((k) => (
                      <React.Fragment key={k}>
                        <span className="opacity-50">·</span>
                        <span>{k}</span>
                      </React.Fragment>
                    ))}
                    {displayWordCount ? (
                      <>
                        <span className="opacity-50">·</span>
                        <span className="tabular-nums">{displayWordCount}</span>
                      </>
                    ) : null}
                    {shelfMatch && shelfMatch.totalChapterNum > 0 ? (
                      <>
                        <span className="opacity-50">·</span>
                        <span className="tabular-nums">共 {shelfMatch.totalChapterNum} 章</span>
                      </>
                    ) : null}
                  </p>
                  <div className="hidden flex-wrap items-center gap-1.5 sm:flex">
                    <Badge tone={local ? "success" : "accent"}>
                      {local ? <HardDrive className="size-3" /> : <Globe className="size-3" />}
                      {local ? "本地" : "网络"}
                    </Badge>
                    {kinds.map((k) => (
                      <Badge key={k}>{k}</Badge>
                    ))}
                    {displayWordCount ? <Badge>{displayWordCount}</Badge> : null}
                    {shelfMatch && shelfMatch.totalChapterNum > 0 ? (
                      <Badge>共 {shelfMatch.totalChapterNum} 章</Badge>
                    ) : null}
                  </div>

                  {displayLatest ? (
                    <p className="flex items-center gap-1.5 text-xs text-ink-muted">
                      <span className="shrink-0 text-[10.5px] font-medium text-success sm:rounded-md sm:bg-success-soft sm:px-1.5 sm:py-0.5 sm:text-[10px]">
                        最新
                      </span>
                      <span className="truncate text-ink-secondary">{displayLatest}</span>
                    </p>
                  ) : null}

                  {/* Primary Actions */}
                  <div className="flex flex-wrap items-center gap-2 pt-1">
                    <Button
                      variant="primary"
                      size="sm"
                      onClick={() => handleStartReading()}
                      title={
                        hasStartedReading && shelfMatch?.durChapterTitle
                          ? `继续：${cleanChapterTitle(shelfMatch.durChapterTitle)}`
                          : undefined
                      }
                    >
                      <BookOpen className="size-3.5" />
                      {hasStartedReading ? "继续阅读" : "开始阅读"}
                      {hasStartedReading && readPct > 0 ? (
                        <span className="rounded-full bg-white/20 px-1.5 py-px font-mono text-[10px] tabular-nums">
                          {readPct}%
                        </span>
                      ) : null}
                    </Button>

                    <AnimatePresence mode="wait" initial={false}>
                      {onShelf ? (
                        <motion.div
                          key="on-shelf"
                          initial={{ opacity: 0, scale: 0.92 }}
                          animate={{ opacity: 1, scale: 1 }}
                          exit={{ opacity: 0, scale: 0.92 }}
                          transition={SPRING_SNAPPY}
                          className="inline-flex"
                        >
                          <InlineConfirm
                            confirmLabel="移出书房"
                            busy={removingShelf}
                            onConfirm={handleRemoveFromShelf}
                            trigger={(arm) => (
                              <button
                                type="button"
                                onClick={arm}
                                title="点击可移出书房"
                                className="group/shelf inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink sm:rounded-full sm:bg-subtle sm:px-3"
                              >
                                <Check className="size-3.5 text-success group-hover/shelf:hidden" />
                                <X className="hidden size-3.5 text-danger group-hover/shelf:inline" />
                                <span className="group-hover/shelf:hidden">已在书房</span>
                                <span className="hidden text-danger group-hover/shelf:inline">
                                  移出书房
                                </span>
                              </button>
                            )}
                          />
                        </motion.div>
                      ) : (
                        <motion.div
                          key="off-shelf"
                          initial={{ opacity: 0, scale: 0.92 }}
                          animate={{ opacity: 1, scale: 1 }}
                          exit={{ opacity: 0, scale: 0.92 }}
                          transition={SPRING_SNAPPY}
                          className="inline-flex"
                        >
                          <Button
                            variant="secondary"
                            size="sm"
                            loading={savingShelf}
                            onClick={handleAddToShelf}
                          >
                            {savingShelf ? null : <BookMarked className="size-3.5" />}
                            {savingShelf ? "收入中…" : "收入书房"}
                          </Button>
                        </motion.div>
                      )}
                    </AnimatePresence>

                    {onShelf ? (
                      <Popover
                        align="start"
                        trigger={
                          <button
                            type="button"
                            disabled={changingGroup}
                            title="修改分组"
                            className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:opacity-45 sm:rounded-full sm:border sm:border-line sm:px-3"
                          >
                            {changingGroup ? (
                              <Loader2 className="size-3.5 animate-spin text-accent" />
                            ) : (
                              <Folder className="size-3.5 text-ink-muted" />
                            )}
                            <span className="max-w-[96px] truncate">{currentGroupName}</span>
                            <ChevronDown className="size-3 text-ink-muted" />
                          </button>
                        }
                      >
                        {(close) => (
                          <div className="max-h-60 overflow-y-auto">
                            <PopoverItem
                              icon={
                                currentGroupId === 0 ? (
                                  <Check className="size-3.5 text-accent" />
                                ) : (
                                  <Folder className="size-3.5" />
                                )
                              }
                              onClick={() => void handleSetGroup(0, close)}
                            >
                              未分组
                            </PopoverItem>
                            {groups
                              .filter((g) => g.id > 0)
                              .map((g) => (
                                <PopoverItem
                                  key={g.id}
                                  icon={
                                    currentGroupId === g.id ? (
                                      <Check className="size-3.5 text-accent" />
                                    ) : (
                                      <Folder className="size-3.5" />
                                    )
                                  }
                                  onClick={() => void handleSetGroup(g.id, close)}
                                >
                                  {g.name}
                                </PopoverItem>
                              ))}
                          </div>
                        )}
                      </Popover>
                    ) : null}
                  </div>
                </div>
              </div>
            </div>

            {/* Reading progress summary when on shelf (flat hairline section on mobile, card on desktop) */}
            {shelfMatch && hasStartedReading ? (
              <div className="border-b border-line/70 pb-5 sm:rounded-2xl sm:border sm:border-line sm:bg-elevated/80 sm:p-4 sm:shadow-[var(--shadow-sm)]">
                <div className="flex items-start justify-between gap-3">
                  <button
                    type="button"
                    onClick={() => handleStartReading()}
                    className="group min-w-0 flex-1 cursor-pointer text-left"
                    title="从这里继续阅读"
                  >
                    <span className="text-[11px] tracking-wide text-ink-muted">当前读至</span>
                    <p className="mt-0.5 truncate font-serif text-[15px] font-medium text-ink transition-colors group-hover:text-accent">
                      {cleanChapterTitle(shelfMatch.durChapterTitle) ||
                        shelfMatch.durChapterTitle ||
                        `第 ${shelfMatch.durChapterIndex + 1} 章`}
                    </p>
                  </button>
                  {shelfMatch.totalChapterNum > 0 ? (
                    <div className="shrink-0 text-right">
                      <p className="font-mono text-xl leading-none font-semibold text-accent tabular-nums">
                        {readPct}
                        <span className="ml-0.5 text-xs">%</span>
                      </p>
                      <p className="mt-1 text-[10.5px] text-ink-muted tabular-nums">
                        {remainingChapters > 0 ? `还剩 ${remainingChapters} 章` : "已读至最新"}
                      </p>
                    </div>
                  ) : null}
                </div>
                {shelfMatch.totalChapterNum > 0 ? (
                  <div className="mt-3 h-1.5 w-full overflow-hidden rounded-full bg-subtle">
                    <motion.div
                      className="h-full rounded-full bg-gradient-to-r from-accent/70 to-accent"
                      initial={{ width: 0 }}
                      animate={{ width: `${Math.max(2, readPct)}%` }}
                      transition={SPRING_SILK}
                    />
                  </div>
                ) : null}
                <div className="mt-3 flex items-center justify-between gap-2 text-[11px] text-ink-muted">
                  <span className="tabular-nums">
                    {Math.min(shelfMatch.durChapterIndex + 1, shelfMatch.totalChapterNum || Infinity)}
                    {shelfMatch.totalChapterNum > 0 ? ` / ${shelfMatch.totalChapterNum} 章` : " 章"}
                    {shelfMatch.durChapterTime
                      ? ` · ${formatRelativeTime(shelfMatch.durChapterTime)}`
                      : ""}
                  </span>
                  <button
                    type="button"
                    onClick={() => setTab("toc")}
                    className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2 py-0.5 text-ink-secondary transition-colors hover:bg-subtle hover:text-accent"
                  >
                    <ListOrdered className="size-3" />
                    在目录中查看
                  </button>
                </div>
              </div>
            ) : null}

            {/* Synopsis */}
            <section className="space-y-2.5 border-b border-line/70 pb-5 sm:border-b-0 sm:pb-0">
              <SectionTitle
                extra={
                  bookInfo?.publisher ? (
                    <span className="text-[11px] text-ink-muted">{bookInfo.publisher}</span>
                  ) : null
                }
              >
                作品简介
              </SectionTitle>
              <div className="sm:rounded-2xl sm:border sm:border-line sm:bg-elevated/60 sm:px-4 sm:py-3.5">
                {displayIntro ? (
                  <>
                    <div
                      className={cn(
                        "relative space-y-2 overflow-hidden font-serif text-[13.5px] leading-[1.85] text-ink-secondary",
                        introLong && !introExpanded && "max-h-[7.6em]",
                      )}
                    >
                      {displayIntro.split("\n").map((para, i) => (
                        <p key={i} className="indent-[1.5em] first:indent-0">
                          {para}
                        </p>
                      ))}
                      {introLong && !introExpanded ? (
                        <div
                          aria-hidden
                          className="pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-gradient-to-t from-elevated to-transparent"
                        />
                      ) : null}
                    </div>
                    {introLong ? (
                      <button
                        type="button"
                        onClick={() => setIntroExpanded((v) => !v)}
                        className="mt-2 inline-flex cursor-pointer items-center gap-1 text-[11.5px] font-medium text-accent hover:underline"
                      >
                        {introExpanded ? "收起" : "展开全文"}
                        <ChevronDown
                          className={cn(
                            "size-3 transition-transform",
                            introExpanded && "rotate-180",
                          )}
                        />
                      </button>
                    ) : null}
                  </>
                ) : (
                  <p className="py-2 text-center text-xs text-ink-muted">暂无作品简介</p>
                )}
              </div>
            </section>

            {/* Offline Cache Status Bar */}
            <section className="space-y-2.5">
              <SectionTitle
                extra={
                  onShelf && !local && fullyCached && !caching ? (
                    <Badge tone="success">
                      <Check className="size-3" />
                      已全部缓存
                    </Badge>
                  ) : null
                }
              >
                离线缓存
              </SectionTitle>
              {local ? (
                <div className="flex items-center gap-2.5 rounded-2xl border border-line bg-subtle/45 px-4 py-3 text-xs text-ink-secondary">
                  <HardDrive className="size-4 shrink-0 text-success" />
                  <span>本地书库文件已完整驻留本机，可随时离线阅读。</span>
                </div>
              ) : !onShelf ? (
                <div className="flex items-center justify-between gap-3 rounded-2xl border border-dashed border-line-strong bg-subtle/35 px-4 py-3 text-xs text-ink-muted">
                  <span>收入书房后支持一键整书离线缓存与全书检索。</span>
                  <Button
                    size="xs"
                    variant="secondary"
                    loading={savingShelf}
                    onClick={handleAddToShelf}
                  >
                    收入书房
                  </Button>
                </div>
              ) : (
                <div className="space-y-3 sm:rounded-2xl sm:border sm:border-line sm:bg-elevated/70 sm:p-4">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex items-center gap-2 text-xs text-ink-secondary">
                      <span
                        className={cn(
                          "flex size-7 items-center justify-center rounded-lg",
                          fullyCached ? "bg-success-soft text-success" : "bg-subtle text-ink-muted",
                        )}
                      >
                        {caching ? (
                          <Loader2 className="size-3.5 animate-spin text-accent" />
                        ) : (
                          <HardDrive className="size-3.5" />
                        )}
                      </span>
                      {cacheInfoLoading ? (
                        <Spinner label="读取缓存信息…" />
                      ) : (
                        <span className="tabular-nums">
                          已缓存{" "}
                          <strong className="font-semibold text-ink">{cachedCount}</strong>
                          {shelfMatch?.totalChapterNum ? ` / ${shelfMatch.totalChapterNum}` : ""}{" "}
                          章 · 占用{" "}
                          <strong className="font-semibold text-ink">
                            {formatBytes(cacheInfo?.cacheSize ?? 0)}
                          </strong>
                        </span>
                      )}
                    </div>

                    <div className="flex items-center gap-1.5">
                      {caching ? (
                        <Button size="xs" variant="danger" onClick={handleCancelCache}>
                          <Square className="size-3 fill-current" />
                          取消缓存
                        </Button>
                      ) : (
                        <>
                          <Button
                            size="xs"
                            variant={fullyCached ? "secondary" : "primary"}
                            onClick={handleStartCache}
                            title={fullyCached ? "重新检查并补齐缺失章节" : "缓存全部章节供离线阅读"}
                          >
                            {fullyCached ? (
                              <RefreshCw className="size-3" />
                            ) : (
                              <Download className="size-3" />
                            )}
                            {fullyCached ? "检查更新" : cachedCount > 0 ? "继续缓存" : "整书缓存"}
                          </Button>
                          {(cacheInfo?.cacheChapterCount ?? 0) > 0 ? (
                            <InlineConfirm
                              confirmLabel="清除"
                              busy={clearingCache}
                              onConfirm={handleDeleteCache}
                              trigger={(arm) => (
                                <IconButton
                                  size="sm"
                                  tone="danger"
                                  aria-label="清理本书缓存"
                                  title="清理本书缓存"
                                  onClick={arm}
                                >
                                  <Trash2 className="size-3.5" />
                                </IconButton>
                              )}
                            />
                          ) : null}
                        </>
                      )}
                    </div>
                  </div>

                  {/* Live silky progress bar (also shows static ratio when partially cached) */}
                  {caching || (cacheProgress && !cacheProgress.cancelled) || cachePct > 0 ? (
                    <div className="space-y-1.5">
                      <div className="h-1.5 w-full overflow-hidden rounded-full bg-subtle">
                        <motion.div
                          className={cn(
                            "h-full rounded-full",
                            fullyCached && !caching ? "bg-success" : "bg-accent",
                          )}
                          initial={{ width: 0 }}
                          animate={{ width: `${cachePct}%` }}
                          transition={SPRING_SILK}
                        />
                      </div>
                      <div className="flex items-center justify-between gap-2 text-[11px] text-ink-muted">
                        <span className="truncate">
                          {caching || cacheProgress
                            ? cacheProgress?.finished
                              ? "离线缓存已就绪"
                              : cacheProgress?.title || "正在抓取章节…"
                            : fullyCached
                              ? "全书已可离线阅读"
                              : "部分章节已缓存"}
                        </span>
                        <span className="shrink-0 font-mono tabular-nums">
                          {cachePct}%
                          {cacheProgress?.failedCount ? (
                            <span className="text-danger"> · 失败 {cacheProgress.failedCount}</span>
                          ) : null}
                        </span>
                      </div>
                    </div>
                  ) : null}
                </div>
              )}
            </section>

            {/* Related Books if present */}
            {bookInfo?.relatedBooks && bookInfo.relatedBooks.length > 0 ? (
              <section className="space-y-2.5">
                <SectionTitle>相关作品</SectionTitle>
                <div className="flex flex-wrap gap-2">
                  {bookInfo.relatedBooks.slice(0, 8).map((rel) => (
                    <button
                      key={rel.bookUrl}
                      type="button"
                      onClick={() =>
                        setActiveBook({
                          bookUrl: rel.bookUrl,
                          name: rel.name,
                          author: rel.author,
                          coverUrl: rel.coverUrl,
                          origin: effectiveOrigin,
                          originName: displayOriginName,
                        })
                      }
                      className="cursor-pointer rounded-full border border-line bg-elevated px-3 py-1.5 text-xs text-ink-secondary transition-colors hover:border-accent/40 hover:text-ink active:scale-[0.97]"
                    >
                      {rel.name}
                      {rel.author ? <span className="ml-1 text-ink-muted">· {rel.author}</span> : null}
                    </button>
                  ))}
                </div>
              </section>
            ) : null}
          </div>
        </div>
      ) : null}

      {/* Tab 2: TOC */}
      {tab === "toc" ? (
        <div className="flex min-h-0 flex-1 flex-col">
          {/* TOC Filter Bar */}
          <div className="shrink-0 space-y-2 border-b border-line bg-elevated/40 px-5 py-2.5">
            <div className="flex items-center gap-2">
              <Input
                size="sm"
                value={tocKeyword}
                onChange={(e) => setTocKeyword(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && tocQuery) {
                    const first = filteredChapters.find((c) => !c.isVolume);
                    if (first) handleStartReading(first.index);
                  }
                }}
                placeholder="速筛章节标题，回车阅读首个命中…"
                leftIcon={<Search className="size-3.5" />}
                clearable
                onClear={() => setTocKeyword("")}
                className="flex-1"
              />
              <button
                type="button"
                onClick={() => setTocReversed((v) => !v)}
                title={tocReversed ? "当前倒序，点击切正序" : "当前正序，点击切倒序"}
                aria-label="切换章节正倒序"
                aria-pressed={tocReversed}
                className={cn(
                  "inline-flex h-8 shrink-0 cursor-pointer items-center gap-1 rounded-full border px-2.5 text-xs font-medium transition-colors active:scale-[0.96]",
                  tocReversed
                    ? "border-accent/30 bg-accent-soft text-accent"
                    : "border-line bg-elevated text-ink-secondary hover:bg-subtle hover:text-ink",
                )}
              >
                {tocReversed ? (
                  <ArrowUpNarrowWide className="size-3.5" />
                ) : (
                  <ArrowDownNarrowWide className="size-3.5" />
                )}
                {tocReversed ? "倒序" : "正序"}
              </button>
              <IconButton
                size="sm"
                disabled={tocRefreshing}
                onClick={handleRefreshToc}
                title="强制刷新目录"
                aria-label="刷新目录"
              >
                <RefreshCw className={cn("size-3.5", tocRefreshing && "animate-spin")} />
              </IconButton>
            </div>

            {/* Hit count / reading position strip */}
            {tocQuery ? (
              <div className="flex items-center justify-between gap-2 text-[11px]">
                <span className="text-ink-muted tabular-nums">
                  {tocHitCount > 0 ? (
                    <>
                      命中 <strong className="font-semibold text-warn">{tocHitCount}</strong> /{" "}
                      {totalPlainChapters} 章
                    </>
                  ) : filteredChapters.length > 0 ? (
                    <>
                      命中 <strong className="font-semibold text-warn">{filteredChapters.length}</strong>{" "}
                      个分卷
                    </>
                  ) : (
                    <>无匹配章节</>
                  )}
                </span>
                <div className="flex items-center gap-1.5">
                  {currentChapterIndex >= 0 ? (
                    <button
                      type="button"
                      onClick={scrollToCurrentChapter}
                      className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2 py-0.5 text-accent transition-colors hover:bg-accent-soft"
                    >
                      <Crosshair className="size-3" />
                      定位当前
                    </button>
                  ) : null}
                  <button
                    type="button"
                    onClick={() => setTocKeyword("")}
                    className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2 py-0.5 text-ink-secondary transition-colors hover:bg-subtle hover:text-ink"
                  >
                    <X className="size-3" />
                    清空筛选
                  </button>
                </div>
              </div>
            ) : currentChapterIndex >= 0 && effectiveChapters.length > 0 ? (
              <div className="flex items-center justify-between gap-2 text-[11px]">
                <button
                  type="button"
                  onClick={scrollToCurrentChapter}
                  className="group flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left"
                  title="一键滚至当前阅读章节"
                >
                  <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-accent-soft px-2 py-0.5 font-medium text-accent transition-colors group-hover:bg-accent group-hover:text-accent-ink">
                    <Crosshair className="size-3" />
                    定位当前
                  </span>
                  <span className="min-w-0 flex-1 truncate text-ink-secondary group-hover:text-accent">
                    {currentChapterInBook
                      ? currentChapterInBook.title
                      : `第 ${currentChapterIndex + 1} 章`}
                  </span>
                </button>
                <div className="flex shrink-0 items-center gap-1.5">
                  <span className="h-1 w-14 overflow-hidden rounded-full bg-subtle">
                    <span
                      className="block h-full rounded-full bg-accent"
                      style={{ width: `${Math.max(3, tocReadPct)}%` }}
                    />
                  </span>
                  <span className="w-8 text-right font-mono text-accent tabular-nums">
                    {tocReadPct}%
                  </span>
                </div>
              </div>
            ) : null}
          </div>

          {/* Virtualized Chapter List */}
          <div className="relative h-[54dvh] shrink-0 sm:h-auto sm:min-h-0 sm:flex-1">
            {tocLoading && effectiveChapters.length === 0 ? (
              <div className="flex h-full items-center justify-center py-16">
                <Spinner label="正在加载目录…" />
              </div>
            ) : tocError && effectiveChapters.length === 0 ? (
              <EmptyView
                compact
                icon={<BookOpen className="size-5" />}
                title="目录加载失败"
                description={
                  tocError instanceof Error ? tocError.message : "请稍后重试或切换其他书源"
                }
                action={
                  <Button size="sm" variant="secondary" onClick={() => void refetchToc()}>
                    重新加载
                  </Button>
                }
              />
            ) : filteredChapters.length === 0 ? (
              <EmptyView
                compact
                icon={<Search className="size-5" />}
                title={tocKeyword ? "未找到匹配章节" : "目录为空"}
                description={tocKeyword ? "试试换个关键词筛选" : "可尝试刷新目录或切换书源"}
                action={
                  tocKeyword ? (
                    <Button size="sm" variant="secondary" onClick={() => setTocKeyword("")}>
                      清空筛选
                    </Button>
                  ) : undefined
                }
              />
            ) : (
              <Virtuoso
                key={tocReversed ? "desc" : "asc"}
                ref={virtuosoRef}
                className="absolute inset-0"
                style={{ height: "100%" }}
                data={filteredChapters}
                overscan={160}
                initialTopMostItemIndex={
                  currentPosInList >= 0 && !tocQuery && !tocReversed
                    ? { index: currentPosInList, align: "center" }
                    : 0
                }
                rangeChanged={setTocRange}
                itemContent={(_, ch) => {
                  const isCurrent = currentChapterIndex >= 0 && ch.index === currentChapterIndex;
                  const isRead = currentChapterIndex >= 0 && ch.index < currentChapterIndex;
                  if (ch.isVolume) {
                    return (
                      <div className="flex items-center gap-2 border-b border-line bg-subtle/60 px-5 py-2 font-serif text-xs font-semibold tracking-wider text-ink-secondary">
                        <span className="h-3 w-[3px] rounded-full bg-ink-muted/50" aria-hidden />
                        <HighlightText text={ch.title} keyword={tocQuery} />
                      </div>
                    );
                  }
                  return (
                    <button
                      type="button"
                      onClick={() => handleStartReading(ch.index)}
                      aria-current={isCurrent ? "true" : undefined}
                      className={cn(
                        "relative flex w-full cursor-pointer items-center justify-between gap-3 border-b border-line/60 px-5 py-2.5 text-left transition-colors hover:bg-subtle/65",
                        isCurrent && "bg-accent-soft/60 hover:bg-accent-soft/80",
                      )}
                    >
                      {isCurrent ? (
                        <span
                          aria-hidden
                          className="absolute inset-y-1.5 left-0 w-[3px] rounded-r-full bg-accent"
                        />
                      ) : null}
                      <div className="flex min-w-0 flex-1 items-center gap-2.5">
                        <span
                          className={cn(
                            "w-9 shrink-0 font-mono text-[11px] tabular-nums",
                            isCurrent ? "text-accent" : "text-ink-muted",
                          )}
                        >
                          {ch.index + 1}
                        </span>
                        <span
                          className={cn(
                            "truncate text-[13px]",
                            isCurrent
                              ? "font-semibold text-accent"
                              : isRead
                                ? "text-ink-muted"
                                : "text-ink",
                          )}
                        >
                          <HighlightText text={ch.title} keyword={tocQuery} />
                        </span>
                      </div>
                      <div className="flex shrink-0 items-center gap-2">
                        {ch.chapterWordCount ? (
                          <span className="text-[11px] text-ink-muted tabular-nums">
                            {ch.chapterWordCount} 字
                          </span>
                        ) : ch.tag ? (
                          <span className="max-w-[100px] truncate text-[11px] text-ink-muted">
                            {ch.tag}
                          </span>
                        ) : null}
                        {isCurrent ? (
                          <Badge tone="accent">
                            <BookOpen className="size-3" />
                            在读
                          </Badge>
                        ) : null}
                      </div>
                    </button>
                  );
                }}
              />
            )}

            {/* Floating "back to current" pill */}
            <AnimatePresence>
              {currentDirection && filteredChapters.length > 0 ? (
                <motion.button
                  key="locate-current"
                  type="button"
                  onClick={scrollToCurrentChapter}
                  initial={{ opacity: 0, y: 8, scale: 0.94 }}
                  animate={{ opacity: 1, y: 0, scale: 1 }}
                  exit={{ opacity: 0, y: 8, scale: 0.94 }}
                  transition={SPRING_SNAPPY}
                  className="absolute right-4 bottom-4 z-10 inline-flex cursor-pointer items-center gap-1.5 rounded-full border border-accent/25 bg-elevated/95 px-3 py-1.5 text-xs font-medium text-accent shadow-[var(--shadow-md,0_6px_20px_-6px_rgba(0,0,0,0.25))] backdrop-blur hover:bg-accent-soft"
                >
                  {currentDirection === "up" ? (
                    <ArrowUp className="size-3.5" />
                  ) : (
                    <ArrowDown className="size-3.5" />
                  )}
                  回到当前章
                </motion.button>
              ) : null}
            </AnimatePresence>
          </div>
        </div>
      ) : null}

      {/* Tab 3: Sources */}
      {tab === "sources" ? (
        <div className="flex h-[66dvh] shrink-0 flex-col sm:h-auto sm:min-h-0 sm:flex-1">
          {local ? (
            <div className="flex flex-1 items-center justify-center">
              <EmptyView
                icon={<HardDrive className="size-5" />}
                title="本地书籍无需换源"
                description="该书籍来自本地 TXT / EPUB 导入，不依赖网络书源。"
              />
            </div>
          ) : (
            <>
              {/* Source Switch Header */}
              <div className="shrink-0 space-y-2 border-b border-line/70 px-5 py-2.5 sm:space-y-2.5 sm:border-line sm:bg-elevated/45 sm:py-3">
                <div className="flex items-center justify-between gap-2.5">
                  <div className="flex min-w-0 items-center gap-1.5 text-xs">
                    <span className="shrink-0 text-ink-muted">当前源</span>
                    <span className="truncate font-medium text-accent sm:rounded-full sm:bg-accent-soft sm:px-2 sm:py-0.5">
                      {displayOriginName}
                    </span>
                    {searchingSources ? (
                      <span className="inline-flex shrink-0 items-center gap-1.5 text-[11px] text-ink-muted">
                        <span className="relative flex size-2">
                          <span className="absolute inline-flex size-full animate-ping rounded-full bg-accent opacity-75" />
                          <span className="relative inline-flex size-2 rounded-full bg-accent" />
                        </span>
                        已探 {searchedSourceCount} 源
                      </span>
                    ) : null}
                  </div>

                  <div className="flex shrink-0 items-center gap-1.5">
                    {searchingSources ? (
                      <button
                        type="button"
                        onClick={stopSourceStream}
                        className="inline-flex cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-semibold text-danger transition-colors hover:bg-danger-soft sm:h-7 sm:rounded-full sm:bg-danger-soft sm:px-2.5 sm:text-[11.5px]"
                      >
                        <Square className="size-2.5 fill-current" />
                        停止
                      </button>
                    ) : (
                      <button
                        type="button"
                        onClick={startAlternativeStream}
                        className="inline-flex cursor-pointer items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-accent transition-colors hover:bg-accent-soft sm:h-7 sm:rounded-full sm:border sm:border-line-strong sm:bg-elevated sm:px-2.5 sm:text-[11.5px] sm:text-ink"
                      >
                        <Sparkles className="size-3.5 text-accent" />
                        全源重搜
                      </button>
                    )}
                  </div>
                </div>

                {allCandidates.length > 1 ? (
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex items-center gap-3.5 sm:hidden">
                      {(
                        [
                          { value: "smart" as const, label: "默认" },
                          { value: "latest" as const, label: "最新" },
                          { value: "fast" as const, label: "最快" },
                        ]
                      ).map((opt) => (
                        <button
                          key={opt.value}
                          type="button"
                          onClick={() => setSourceSort(opt.value)}
                          className={cn(
                            "cursor-pointer py-0.5 text-xs transition-colors",
                            sourceSort === opt.value
                              ? "font-semibold text-accent"
                              : "text-ink-muted hover:text-ink",
                          )}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>
                    <div className="hidden sm:block">
                      <Segmented
                        layoutId="book-inspector-source-sort"
                        size="sm"
                        value={sourceSort}
                        onChange={setSourceSort}
                        options={[
                          { value: "smart", label: "默认" },
                          {
                            value: "latest",
                            label: "最新",
                            icon: <Sparkles className="size-3" />,
                          },
                          { value: "fast", label: "最快", icon: <Zap className="size-3" /> },
                        ]}
                      />
                    </div>
                    <div className="flex items-center gap-2 text-[11px] text-ink-muted tabular-nums sm:gap-2.5">
                      {(["fast", "mid", "slow"] as const).map((tier) =>
                        latencySummary[tier] > 0 ? (
                          <span key={tier} className="inline-flex items-center gap-1">
                            <span
                              className={cn("size-1.5 rounded-full", LATENCY_STYLE[tier].dot)}
                            />
                            {LATENCY_STYLE[tier].label} {latencySummary[tier]}
                          </span>
                        ) : null,
                      )}
                      <span>共 {allCandidates.length} 源</span>
                    </div>
                  </div>
                ) : null}
              </div>

              {/* Candidate List (hairline rows on mobile, cards on desktop) */}
              <div className="min-h-0 flex-1 overflow-y-auto px-5 py-1 sm:py-3.5">
                {loadingAlternatives && allCandidates.length === 0 ? (
                  <div className="flex justify-center py-14">
                    <Spinner label="正在检索可用书源…" />
                  </div>
                ) : allCandidates.length === 0 && !searchingSources ? (
                  <EmptyView
                    compact
                    icon={<Globe className="size-5" />}
                    title="暂未发现其他候选书源"
                    description="点击「全源重搜」可跨全部启用书源流式探测同名候选源。"
                    action={
                      <Button size="sm" variant="primary" onClick={startAlternativeStream}>
                        <Sparkles className="size-3.5" />
                        开始全源重搜
                      </Button>
                    }
                  />
                ) : (
                  <div className="divide-y divide-line/70 sm:space-y-2.5 sm:divide-y-0">
                    {decoratedCandidates.rows.map((row) => {
                      const { cand, origin: candOrigin, isCurrent, latest, chapterNo } = row;
                      const isSwitching = switchingUrl === cand.bookUrl;
                      const isNewest =
                        decoratedCandidates.maxNo > 0 && chapterNo === decoratedCandidates.maxNo;
                      const delta =
                        !isCurrent && currentLatestNo > 0 && chapterNo > 0
                          ? chapterNo - currentLatestNo
                          : 0;
                      const tier = row.latencyMs !== undefined ? latencyTier(row.latencyMs) : null;
                      return (
                        <motion.div
                          key={row.key}
                          layout="position"
                          transition={SPRING_SNAPPY}
                          className={cn(
                            "relative flex items-center justify-between gap-3 py-3 transition-colors sm:overflow-hidden sm:rounded-2xl sm:border sm:px-4",
                            isCurrent
                              ? "sm:border-accent/40 sm:bg-accent-soft/45 sm:shadow-[var(--shadow-sm)]"
                              : "sm:border-line sm:bg-elevated/75 sm:hover:border-line-strong sm:hover:bg-elevated",
                            isSwitching && "sm:ring-2 sm:ring-accent/30",
                          )}
                        >
                          {isCurrent ? (
                            <span
                              aria-hidden
                              className="absolute inset-y-2 left-0 hidden w-[3px] rounded-r-full bg-accent sm:block"
                            />
                          ) : null}
                          <div className="min-w-0 flex-1 space-y-1 sm:space-y-1.5">
                            <div className="flex min-w-0 items-center gap-1.5">
                              <span
                                className={cn(
                                  "truncate font-serif text-[14px] font-medium",
                                  isCurrent ? "text-accent sm:text-ink" : "text-ink",
                                )}
                              >
                                {cand.originName || candOrigin || "未知书源"}
                              </span>
                              {isCurrent ? (
                                <span className="shrink-0 text-[10.5px] font-medium text-accent sm:rounded-full sm:bg-accent-soft sm:px-2 sm:py-0.5">
                                  当前源
                                </span>
                              ) : null}
                              {isNewest ? (
                                <span className="inline-flex shrink-0 items-center gap-0.5 text-[10.5px] font-medium text-success sm:rounded-full sm:bg-success-soft sm:px-2 sm:py-0.5">
                                  <Sparkles className="hidden size-3 sm:inline" />
                                  最新
                                </span>
                              ) : null}
                              {tier && row.latencyMs !== undefined ? (
                                <span
                                  title={
                                    row.latencySource === "live"
                                      ? "本次探测响应耗时"
                                      : "历史平均响应耗时"
                                  }
                                  className={cn(
                                    "inline-flex shrink-0 items-center gap-1 font-mono text-[10.5px] leading-none tabular-nums sm:rounded-full sm:border sm:px-1.5 sm:py-0.5",
                                    tier === "fast"
                                      ? "text-success sm:border-success/25 sm:bg-success-soft/70"
                                      : tier === "mid"
                                        ? "text-warn sm:border-warn/25 sm:bg-warn-soft/70"
                                        : "text-danger sm:border-danger/25 sm:bg-danger-soft/70",
                                  )}
                                >
                                  <Timer className="hidden size-2.5 sm:inline" />
                                  {formatLatency(row.latencyMs)}
                                </span>
                              ) : null}
                            </div>
                            <div className="flex min-w-0 items-center gap-1.5 text-xs">
                              {latest ? (
                                <>
                                  <span
                                    className={cn(
                                      "truncate",
                                      isNewest ? "font-medium text-success" : "text-ink-secondary",
                                    )}
                                    title={latest}
                                  >
                                    {latest}
                                  </span>
                                  {delta > 0 ? (
                                    <span className="shrink-0 text-[10.5px] font-medium text-success tabular-nums sm:rounded-md sm:bg-success-soft sm:px-1.5 sm:py-px sm:text-[10px]">
                                      +{delta}章
                                    </span>
                                  ) : delta < 0 ? (
                                    <span className="shrink-0 text-[10px] text-ink-muted tabular-nums">
                                      落后 {-delta} 章
                                    </span>
                                  ) : null}
                                </>
                              ) : (
                                <span className="truncate text-ink-muted">
                                  {cand.author || "暂无最新章信息"}
                                </span>
                              )}
                            </div>
                          </div>

                          <div className="flex shrink-0 items-center gap-1.5">
                            {isCurrent ? (
                              <span className="inline-flex size-7 items-center justify-center text-accent sm:size-8 sm:rounded-full sm:bg-accent-soft">
                                <Check className="size-4" />
                              </span>
                            ) : onShelf ? (
                              <button
                                type="button"
                                disabled={Boolean(switchingUrl && !isSwitching)}
                                onClick={() => void handleSwitchSource(cand, false)}
                                className={cn(
                                  "inline-flex h-8 cursor-pointer items-center rounded-lg px-2 text-xs font-medium transition-colors disabled:opacity-45 sm:h-7 sm:rounded-full sm:px-3 sm:text-[11.5px]",
                                  isNewest || tier === "fast"
                                    ? "font-semibold text-accent hover:bg-accent-soft sm:bg-accent sm:text-accent-ink sm:hover:brightness-110"
                                    : "text-ink-secondary hover:bg-subtle hover:text-ink sm:border sm:border-line-strong sm:bg-elevated",
                                )}
                              >
                                {isSwitching ? "切换中…" : "切换"}
                                <span className="hidden sm:inline">{isSwitching ? "" : "此源"}</span>
                              </button>
                            ) : (
                              <>
                                <button
                                  type="button"
                                  disabled={Boolean(switchingUrl)}
                                  onClick={() => void handleSwitchSource(cand, false)}
                                  className="cursor-pointer rounded-lg px-2 py-1 text-xs text-ink-secondary hover:bg-subtle hover:text-ink disabled:opacity-45"
                                >
                                  预览
                                </button>
                                <button
                                  type="button"
                                  disabled={Boolean(switchingUrl && !isSwitching)}
                                  onClick={() => void handleSwitchSource(cand, true)}
                                  className="cursor-pointer rounded-lg px-2 py-1 text-xs font-semibold text-accent hover:bg-accent-soft disabled:opacity-45 sm:rounded-full sm:border sm:border-line-strong sm:bg-elevated sm:px-2.5 sm:text-ink"
                                >
                                  {isSwitching ? "切换中…" : "收书切源"}
                                </button>
                              </>
                            )}
                          </div>
                        </motion.div>
                      );
                    })}

                    {searchingSources ? (
                      <div className="flex items-center justify-center gap-2 py-4 text-xs text-ink-muted">
                        <Loader2 className="size-3.5 animate-spin text-accent" />
                        <span>正在持续扫描其他书源…</span>
                      </div>
                    ) : null}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      ) : null}
    </Sheet>
  );
}
