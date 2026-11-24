import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDownUp,
  Bookmark as BookmarkIcon,
  BookmarkPlus,
  BookOpen,
  Download,
  Highlighter,
  LocateFixed,
  RefreshCw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import * as React from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { booksApi, featuresApi } from "@/api/endpoints";
import type { BookChapter, BookContentHit, Bookmark, CacheProgress } from "@/api/types";
import { cn } from "@/lib/cn";
import { formatBytes, formatRelativeTime } from "@/lib/format";
import { useNotesStore, type InkColor } from "@/stores/notes";
import { notify } from "@/stores/toast";
import { Button, IconButton } from "@/ui/Button";
import { Badge, EmptyView, InlineConfirm, Spinner } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import { Sheet, SheetTabPanel, SheetTabs, useSheetTabDirection } from "@/ui/Sheet";

const TOC_TAB_ORDER = ["chapters", "search"] as const;
const NOTES_TAB_ORDER = ["bookmarks", "highlights"] as const;

/* ============================================================================
   1. TocSheet — Left Drawer (Chapters List + Full-Book Search + Offline Cache)
   ============================================================================ */

export interface TocSheetProps {
  open: boolean;
  onClose: () => void;
  bookUrl: string;
  bookName: string;
  isInShelf: boolean;
  chapters: BookChapter[];
  currentChapterIndex: number;
  loadingChapters: boolean;
  onSelectChapter: (index: number) => void;
  onRefreshToc: () => void;
  onAddToShelf?: () => void;
}

export function TocSheet({
  open,
  onClose,
  bookUrl,
  bookName,
  isInShelf,
  chapters,
  currentChapterIndex,
  loadingChapters,
  onSelectChapter,
  onRefreshToc,
  onAddToShelf,
}: TocSheetProps) {
  const [tab, setTab] = React.useState<"chapters" | "search">("chapters");
  const tocTabs = useSheetTabDirection(tab, setTab, TOC_TAB_ORDER);
  const [filterText, setFilterText] = React.useState("");
  const [reversed, setReversed] = React.useState(false);
  const virtuosoRef = React.useRef<VirtuosoHandle>(null);

  // Full-book search state
  const [searchKey, setSearchKey] = React.useState("");
  const [submittedKey, setSubmittedKey] = React.useState("");
  const [searching, setSearching] = React.useState(false);
  const [hits, setHits] = React.useState<BookContentHit[]>([]);
  const [searchError, setSearchError] = React.useState<string | null>(null);
  const searchAbortRef = React.useRef<AbortController | null>(null);

  // Offline cache SSE state
  const [cacheProgress, setCacheProgress] = React.useState<CacheProgress | null>(null);
  const [caching, setCaching] = React.useState(false);
  const cacheCloseRef = React.useRef<(() => void) | null>(null);

  const { data: cacheInfo, refetch: refetchCacheInfo } = useQuery({
    queryKey: ["book-cache-info", bookUrl],
    queryFn: () => featuresApi.getBookCacheInfo(bookUrl),
    enabled: open && Boolean(bookUrl) && isInShelf,
    staleTime: 15_000,
  });

  // Clean up search abort and cache stream on unmount
  React.useEffect(() => {
    return () => {
      searchAbortRef.current?.abort();
      cacheCloseRef.current?.();
    };
  }, []);

  // Reset search when book changes
  React.useEffect(() => {
    setFilterText("");
    setSearchKey("");
    setSubmittedKey("");
    setHits([]);
    setSearchError(null);
    setCacheProgress(null);
    setCaching(false);
    cacheCloseRef.current?.();
    cacheCloseRef.current = null;
  }, [bookUrl]);

  const displayedChapters = React.useMemo(() => {
    const q = filterText.trim().toLowerCase();
    const list = q
      ? chapters.filter((ch) => ch.title.toLowerCase().includes(q))
      : chapters;
    return reversed ? [...list].reverse() : list;
  }, [chapters, filterText, reversed]);

  const activeListIdx = React.useMemo(
    () => displayedChapters.findIndex((ch) => ch.index === currentChapterIndex),
    [displayedChapters, currentChapterIndex],
  );

  // Auto-scroll to current chapter when drawer opens
  React.useEffect(() => {
    if (!open || tab !== "chapters" || activeListIdx < 0) return;
    const timer = window.setTimeout(() => {
      virtuosoRef.current?.scrollToIndex({
        index: activeListIdx,
        align: "center",
        behavior: "auto",
      });
    }, 60);
    return () => window.clearTimeout(timer);
  }, [open, tab, activeListIdx]);

  const handleLocateCurrent = () => {
    if (filterText) setFilterText("");
    const idx = activeListIdx >= 0 ? activeListIdx : 0;
    virtuosoRef.current?.scrollToIndex({
      index: idx,
      align: "center",
      behavior: "smooth",
    });
  };

  const handleSearchSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    const key = searchKey.trim();
    if (!key) return;
    searchAbortRef.current?.abort();
    const controller = new AbortController();
    searchAbortRef.current = controller;

    setSubmittedKey(key);
    setSearching(true);
    setSearchError(null);
    try {
      const res = await featuresApi.searchBookContent(bookUrl, key, controller.signal);
      if (!controller.signal.aborted) {
        setHits(res ?? []);
      }
    } catch (err) {
      if (!controller.signal.aborted) {
        setSearchError(err instanceof Error ? err.message : "全书搜索失败");
        setHits([]);
      }
    } finally {
      if (!controller.signal.aborted) {
        setSearching(false);
      }
    }
  };

  const handleStartCache = () => {
    if (!isInShelf) {
      notify.info("请先将本书收入书房后再整书缓存");
      return;
    }
    cacheCloseRef.current?.();
    setCaching(true);
    setCacheProgress({
      cached: cacheInfo?.cacheChapterCount ?? 0,
      total: chapters.length,
      title: "正在准备缓存任务…",
      finished: false,
      cancelled: false,
      error: null,
    });

    cacheCloseRef.current = featuresApi.cacheBookStream(
      { url: bookUrl },
      {
        onData: (progress) => {
          setCacheProgress(progress);
          if (progress.finished || progress.cancelled) {
            setCaching(false);
            cacheCloseRef.current = null;
            void refetchCacheInfo();
            if (progress.error) {
              notify.error(progress.error);
            } else if (progress.cancelled) {
              notify.info("已取消整书缓存");
            } else {
              notify.success(`整书缓存完成（共 ${progress.cached} 章）`);
            }
          }
        },
        onError: (err) => {
          setCaching(false);
          cacheCloseRef.current = null;
          notify.error(err.message || "缓存任务启动失败");
        },
        onClose: () => {
          setCaching(false);
          cacheCloseRef.current = null;
          void refetchCacheInfo();
        },
      },
    );
  };

  const handleCancelCache = async () => {
    cacheCloseRef.current?.();
    cacheCloseRef.current = null;
    try {
      await featuresApi.cancelCacheBook({ url: bookUrl });
    } catch {
      // ignore cancel race
    }
    setCaching(false);
    setCacheProgress((prev) => (prev ? { ...prev, cancelled: true } : null));
    notify.info("已停止后台缓存");
    void refetchCacheInfo();
  };

  const cachePct =
    cacheProgress && cacheProgress.total > 0
      ? Math.min(100, Math.round((cacheProgress.cached / cacheProgress.total) * 100))
      : 0;

  return (
    <Sheet
      open={open}
      onClose={onClose}
      side="left"
      size="md"
      mobileHeight="full"
      title={bookName || "书籍目录"}
      subtitle={chapters.length > 0 ? `共 ${chapters.length} 章` : "加载目录中…"}
      bodyClassName="flex flex-col overflow-hidden p-0"
      footer={
        <div className="space-y-2">
          {caching && cacheProgress ? (
            <div className="space-y-2">
              <div className="flex items-center justify-between gap-2 text-xs">
                <span className="min-w-0 truncate text-ink-secondary">
                  {cacheProgress.title || "缓存中…"}
                </span>
                <span className="shrink-0 font-mono text-[11px] tabular-nums text-accent">
                  {cacheProgress.cached} / {cacheProgress.total} ({cachePct}%)
                </span>
              </div>
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-subtle">
                <div
                  className="h-full rounded-full bg-accent transition-[width] duration-250"
                  style={{ width: `${cachePct}%` }}
                />
              </div>
              <div className="flex items-center justify-between text-[11px] text-ink-muted">
                <span>
                  成功 {cacheProgress.successCount ?? cacheProgress.cached}
                  {cacheProgress.failedCount ? ` · 失败 ${cacheProgress.failedCount}` : ""}
                </span>
                <button
                  type="button"
                  onClick={() => void handleCancelCache()}
                  className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2 py-0.5 text-danger hover:bg-danger-soft"
                >
                  <X className="size-3" />
                  <span>取消缓存</span>
                </button>
              </div>
            </div>
          ) : (
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0 text-xs text-ink-muted">
                {isInShelf ? (
                  cacheInfo && cacheInfo.cacheChapterCount > 0 ? (
                    <span className="tabular-nums">
                      已离线缓存 {cacheInfo.cacheChapterCount} 章 ·{" "}
                      {formatBytes(cacheInfo.cacheSize)}
                    </span>
                  ) : (
                    <span>后台静默缓存整本章节</span>
                  )
                ) : (
                  <span>收入书房后支持整书缓存</span>
                )}
              </div>
              {isInShelf ? (
                <Button size="sm" variant="secondary" onClick={handleStartCache}>
                  <Download className="size-3.5" />
                  <span>整书缓存</span>
                </Button>
              ) : onAddToShelf ? (
                <Button size="sm" variant="primary" onClick={onAddToShelf}>
                  收入书房
                </Button>
              ) : null}
            </div>
          )}
        </div>
      }
    >
      {/* Top Tab Switch (unified spring underline on mobile, Segmented on desktop) */}
      <div className="shrink-0 border-b border-line px-4 pt-1 pb-3 sm:py-3">
        <SheetTabs<"chapters" | "search">
          layoutId="reader-toc-tab"
          ariaLabel="目录与搜索"
          value={tab}
          onChange={tocTabs.onChange}
          options={[
            {
              value: "chapters",
              label: "章节目录",
              icon: <BookOpen className="size-3.5" />,
            },
            {
              value: "search",
              label: "全书搜索",
              icon: <Search className="size-3.5" />,
            },
          ]}
        />

        {tab === "chapters" ? (
          <div className="mt-2.5 flex items-center gap-1.5">
            <Input
              size="sm"
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              clearable
              onClear={() => setFilterText("")}
              placeholder="筛选章节名…"
              leftIcon={<Search className="size-3.5" />}
              className="flex-1"
            />
            <IconButton
              size="sm"
              active={reversed}
              title={reversed ? "当前倒序，点击切正序" : "倒序排列"}
              aria-label="切换正序倒序"
              onClick={() => setReversed((r) => !r)}
            >
              <ArrowDownUp className="size-3.5" />
            </IconButton>
            <IconButton
              size="sm"
              title="定位当前章节"
              aria-label="定位当前章节"
              onClick={handleLocateCurrent}
            >
              <LocateFixed className="size-3.5" />
            </IconButton>
            <IconButton
              size="sm"
              title="刷新目录"
              aria-label="刷新目录"
              disabled={loadingChapters}
              onClick={onRefreshToc}
            >
              <RefreshCw className={cn("size-3.5", loadingChapters && "animate-spin")} />
            </IconButton>
          </div>
        ) : (
          <form onSubmit={(e) => void handleSearchSubmit(e)} className="mt-2.5 flex items-center gap-2">
            <Input
              size="sm"
              value={searchKey}
              onChange={(e) => setSearchKey(e.target.value)}
              clearable
              onClear={() => {
                setSearchKey("");
                setHits([]);
                setSubmittedKey("");
                setSearchError(null);
              }}
              placeholder="在已缓存章节或本地书中搜索正文…"
              leftIcon={<Search className="size-3.5" />}
              className="flex-1"
            />
            <Button type="submit" size="sm" variant="primary" loading={searching}>
              搜索
            </Button>
          </form>
        )}
      </div>

      {/* Body Area (equal height across both tabs with unified directional transition) */}
      <div className="relative min-h-0 flex-1 overflow-hidden">
        <SheetTabPanel
          activeKey={tab}
          direction={tocTabs.direction}
          className={cn("h-full", tab === "search" && "overflow-y-auto overscroll-contain px-4 py-3")}
        >
          {tab === "chapters" ? (
            <div className="relative h-full">
              {loadingChapters && chapters.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <Spinner label="正在载入目录…" />
                </div>
              ) : displayedChapters.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <EmptyView
                    compact
                    icon={<BookOpen className="size-5" />}
                    title={filterText ? "未找到匹配的章节" : "暂无章节目录"}
                    description={filterText ? "试试更简短的关键词" : "可尝试刷新目录或切换书源"}
                  />
                </div>
              ) : (
                <Virtuoso
                  ref={virtuosoRef}
                  className="absolute inset-0"
                  style={{ height: "100%" }}
                  data={displayedChapters}
                  overscan={240}
                  itemContent={(_idx, ch) => {
                    const isCurrent = ch.index === currentChapterIndex;
                    if (ch.isVolume) {
                      return (
                        <div className="border-b border-line/60 bg-subtle/45 px-5 py-2 font-serif text-xs font-semibold tracking-wider text-ink-secondary">
                          {ch.title}
                        </div>
                      );
                    }
                    return (
                      <button
                        type="button"
                        onClick={() => {
                          onSelectChapter(ch.index);
                          onClose();
                        }}
                        className={cn(
                          "group relative flex w-full cursor-pointer items-center gap-3 border-b border-line/50 px-5 py-3 text-left transition-colors",
                          isCurrent ? "bg-accent-soft/55 text-accent" : "text-ink hover:bg-subtle/70",
                        )}
                      >
                        {isCurrent ? (
                          <span
                            aria-hidden
                            className="absolute inset-y-2 left-1.5 w-[3px] rounded-full bg-accent"
                          />
                        ) : null}
                        <span
                          className={cn(
                            "w-8 shrink-0 font-mono text-[11px] tabular-nums",
                            isCurrent ? "text-accent" : "text-ink-muted",
                          )}
                        >
                          {ch.index + 1}
                        </span>
                        <span
                          className={cn(
                            "min-w-0 flex-1 truncate text-[13px]",
                            isCurrent ? "font-semibold" : "font-normal",
                          )}
                        >
                          {ch.title}
                        </span>
                        {ch.chapterWordCount ? (
                          <span className="shrink-0 text-[10px] tabular-nums text-ink-muted">
                            {ch.chapterWordCount >= 1000
                              ? `${(ch.chapterWordCount / 1000).toFixed(1)}k`
                              : `${ch.chapterWordCount}字`}
                          </span>
                        ) : null}
                      </button>
                    );
                  }}
                />
              )}
            </div>
          ) : (
            <>
              {searching ? (
                <div className="flex h-full items-center justify-center">
                  <Spinner label="正在检索全书正文…" />
                </div>
              ) : searchError ? (
                <div className="flex h-full items-center justify-center">
                  <EmptyView compact title="检索失败" description={searchError} />
                </div>
              ) : !submittedKey ? (
                <div className="flex h-full items-center justify-center">
                  <EmptyView
                    compact
                    icon={<Search className="size-5" />}
                    title="全书正文检索"
                    description="检索本地书籍或已离线缓存章节中的人物、地名与情节片段。"
                  />
                </div>
              ) : hits.length === 0 ? (
                <div className="flex h-full items-center justify-center">
                  <EmptyView
                    compact
                    icon={<Search className="size-5" />}
                    title={`未找到「${submittedKey}」`}
                    description="网络书籍仅检索已缓存的章节，可先点击底部「整书缓存」后再搜。"
                  />
                </div>
              ) : (
                <div className="space-y-2">
                  <p className="px-1 text-[11px] text-ink-muted">
                    共命中 {hits.length} 处（最多展示 100 条）
                  </p>
                  {hits.map((hit, i) => (
                    <button
                      key={`${hit.chapterIndex}-${i}`}
                      type="button"
                      onClick={() => {
                        onSelectChapter(hit.chapterIndex);
                        onClose();
                      }}
                      className="w-full cursor-pointer rounded-2xl border border-line bg-elevated/75 p-3 text-left transition-colors hover:border-line-strong hover:bg-subtle/60"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <span className="truncate font-serif text-[13px] font-medium text-ink">
                          {hit.title}
                        </span>
                        <Badge tone="default">第 {hit.chapterIndex + 1} 章</Badge>
                      </div>
                      <p className="mt-1.5 line-clamp-3 text-xs leading-relaxed text-ink-secondary">
                        {highlightKeyword(hit.snippet, submittedKey)}
                      </p>
                    </button>
                  ))}
                </div>
              )}
            </>
          )}
        </SheetTabPanel>
      </div>
    </Sheet>
  );
}

function highlightKeyword(text: string, keyword: string): React.ReactNode {
  if (!keyword) return text;
  const escaped = keyword.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const parts = text.split(new RegExp(`(${escaped})`, "gi"));
  return parts.map((part, idx) =>
    part.toLowerCase() === keyword.toLowerCase() ? (
      <mark key={idx} className="rounded bg-accent-soft px-0.5 font-medium text-accent">
        {part}
      </mark>
    ) : (
      <React.Fragment key={idx}>{part}</React.Fragment>
    ),
  );
}

/* ============================================================================
   2. NotesSheet — Right Drawer (Bookmarks + Highlights & Annotations)
   ============================================================================ */

export interface NotesSheetProps {
  open: boolean;
  onClose: () => void;
  bookUrl: string;
  bookName: string;
  bookAuthor: string;
  currentChapterIndex: number;
  currentChapterTitle: string;
  currentParaIndex: number;
  currentExcerpt: string;
  bookmarks: Bookmark[];
  onJumpTo: (chapterIndex: number, paraIndex: number) => void;
}

const NOTE_BAR_COLORS: Record<InkColor, string> = {
  amber: "bg-[#d89b28]",
  sage: "bg-[#3d8b63]",
  vermilion: "bg-[#c3412f]",
};

export function NotesSheet({
  open,
  onClose,
  bookUrl,
  bookName,
  bookAuthor,
  currentChapterIndex,
  currentChapterTitle,
  currentParaIndex,
  currentExcerpt,
  bookmarks,
  onJumpTo,
}: NotesSheetProps) {
  const queryClient = useQueryClient();
  const [tab, setTab] = React.useState<"bookmarks" | "highlights">("bookmarks");
  const notesTabs = useSheetTabDirection(tab, setTab, NOTES_TAB_ORDER);
  const [bookmarkNote, setBookmarkNote] = React.useState("");
  const [savingBm, setSavingBm] = React.useState(false);
  const [clearingBm, setClearingBm] = React.useState(false);
  const [colorFilter, setColorFilter] = React.useState<InkColor | "all">("all");

  const allNotes = useNotesStore((s) => s.byBook[bookUrl]);
  const notes = React.useMemo(() => allNotes ?? [], [allNotes]);
  const removeNote = useNotesStore((s) => s.removeNote);
  const clearBook = useNotesStore((s) => s.clearBook);

  const filteredNotes = React.useMemo(
    () => (colorFilter === "all" ? notes : notes.filter((n) => n.color === colorFilter)),
    [colorFilter, notes],
  );

  const handleSaveBookmark = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!bookUrl || savingBm) return;
    setSavingBm(true);
    try {
      const stamp = new Date();
      const timeTag = `${String(stamp.getHours()).padStart(2, "0")}:${String(stamp.getMinutes()).padStart(2, "0")}`;
      const title = `${currentChapterTitle || `第 ${currentChapterIndex + 1} 章`} · ${timeTag}`;
      await booksApi.saveBookmark({
        bookUrl,
        title,
        bookName: bookName || "",
        bookAuthor: bookAuthor || "",
        paragraphIndex: currentParaIndex,
        chapterIndex: currentChapterIndex,
        chapterName: currentChapterTitle || `第 ${currentChapterIndex + 1} 章`,
        bookText: currentExcerpt.slice(0, 120),
        content: bookmarkNote.trim(),
        createdAt: Date.now(),
      });
      setBookmarkNote("");
      await queryClient.invalidateQueries({ queryKey: ["bookmarks", bookUrl] });
      notify.success("书签已保存");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "保存书签失败");
    } finally {
      setSavingBm(false);
    }
  };

  const handleDeleteBookmark = async (bm: Bookmark) => {
    try {
      await booksApi.deleteBookmark(bookUrl, bm.title);
      await queryClient.invalidateQueries({ queryKey: ["bookmarks", bookUrl] });
      notify.info("已移除书签");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "删除书签失败");
    }
  };

  const handleClearBookmarks = async () => {
    if (bookmarks.length === 0) return;
    setClearingBm(true);
    try {
      await booksApi.deleteBookmarks(
        bookUrl,
        bookmarks.map((b) => b.title),
      );
      await queryClient.invalidateQueries({ queryKey: ["bookmarks", bookUrl] });
      notify.info("已清空本书全部书签");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "清空书签失败");
    } finally {
      setClearingBm(false);
    }
  };

  return (
    <Sheet
      open={open}
      onClose={onClose}
      side="right"
      size="md"
      mobileHeight="tall"
      title="阅读笔记"
      subtitle={bookName || undefined}
      bodyClassName="flex flex-col overflow-hidden p-0"
      footer={
        tab === "bookmarks" && bookmarks.length > 0 ? (
          <div className="flex items-center justify-between">
            <span className="text-xs text-ink-muted">共 {bookmarks.length} 枚书签</span>
            <InlineConfirm
              confirmLabel="清空全部书签"
              busy={clearingBm}
              onConfirm={handleClearBookmarks}
              trigger={(arm) => (
                <Button size="xs" variant="ghost" onClick={arm} className="text-ink-muted hover:text-danger">
                  <Trash2 className="size-3.5" />
                  <span>清空书签</span>
                </Button>
              )}
            />
          </div>
        ) : tab === "highlights" && notes.length > 0 ? (
          <div className="flex items-center justify-between">
            <span className="text-xs text-ink-muted">共 {notes.length} 条划线批注</span>
            <InlineConfirm
              confirmLabel="清空全部划线"
              onConfirm={() => {
                clearBook(bookUrl);
                notify.info("已清空本书划线批注");
              }}
              trigger={(arm) => (
                <Button size="xs" variant="ghost" onClick={arm} className="text-ink-muted hover:text-danger">
                  <Trash2 className="size-3.5" />
                  <span>清空划线</span>
                </Button>
              )}
            />
          </div>
        ) : undefined
      }
    >
      {/* Pinned Tab Header (equal-height Sheet + unified sliding indicator) */}
      <div className="shrink-0 px-5 pt-1 sm:border-b sm:border-line sm:py-3">
        <SheetTabs<"bookmarks" | "highlights">
          layoutId="reader-notes-tab"
          ariaLabel="阅读笔记页签"
          value={tab}
          onChange={notesTabs.onChange}
          options={[
            {
              value: "bookmarks",
              label: "书签",
              icon: <BookmarkIcon className="size-3.5" />,
              badge:
                bookmarks.length > 0 ? (
                  <span className="rounded-full bg-subtle px-1.5 py-0.5 text-[10px] tabular-nums text-ink-secondary">
                    {bookmarks.length}
                  </span>
                ) : null,
            },
            {
              value: "highlights",
              label: "划线批注",
              icon: <Highlighter className="size-3.5" />,
              badge:
                notes.length > 0 ? (
                  <span className="rounded-full bg-subtle px-1.5 py-0.5 text-[10px] tabular-nums text-ink-secondary">
                    {notes.length}
                  </span>
                ) : null,
            },
          ]}
        />
      </div>

      {/* Scrollable Tab Body with directional slide+fade */}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        <SheetTabPanel
          activeKey={tab}
          direction={notesTabs.direction}
          className="overflow-y-auto overscroll-contain px-5 py-4 pb-[max(1rem,env(safe-area-inset-bottom))]"
        >
        {tab === "bookmarks" ? (
          <div className="space-y-3.5">
            {/* Inline Quick-Add Bookmark (flat hairline on mobile, card on desktop) */}
            <form
              onSubmit={(e) => void handleSaveBookmark(e)}
              className="border-b border-line/70 pb-3.5 sm:rounded-2xl sm:border sm:border-line sm:bg-elevated/80 sm:p-3.5"
            >
              <div className="flex items-center justify-between gap-2">
                <span className="truncate font-serif text-xs font-semibold text-ink">
                  {currentChapterTitle || `第 ${currentChapterIndex + 1} 章`}
                </span>
                <span className="shrink-0 font-mono text-[10px] text-ink-muted">
                  第 {currentParaIndex + 1} 段
                </span>
              </div>
              {currentExcerpt ? (
                <p className="mt-1.5 line-clamp-2 font-serif text-xs leading-relaxed text-ink-muted">
                  “{currentExcerpt}”
                </p>
              ) : null}
              <div className="mt-2.5 flex items-center gap-2">
                <Input
                  size="sm"
                  value={bookmarkNote}
                  onChange={(e) => setBookmarkNote(e.target.value)}
                  placeholder="记一句此刻想法（可选）…"
                  className="flex-1"
                />
                <Button type="submit" size="sm" variant="primary" loading={savingBm}>
                  <BookmarkPlus className="size-3.5" />
                  <span>存书签</span>
                </Button>
              </div>
            </form>

            {bookmarks.length === 0 ? (
              <EmptyView
                compact
                icon={<BookmarkIcon className="size-5" />}
                title="尚无书签"
                description="点击上方「存书签」或左上胶囊的书签图标，记录当前阅读位置。"
              />
            ) : (
              <div className="divide-y divide-line/70 sm:space-y-2.5 sm:divide-y-0">
                {bookmarks.map((bm) => (
                  <div
                    key={bm.title}
                    onClick={() => {
                      onJumpTo(bm.chapterIndex, bm.paragraphIndex);
                      onClose();
                    }}
                    className="group cursor-pointer py-3 transition-colors hover:bg-subtle/40 sm:rounded-2xl sm:border sm:border-line sm:bg-elevated/75 sm:p-3.5 sm:hover:border-line-strong sm:hover:bg-subtle/60"
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0 flex-1">
                        <p className="truncate font-serif text-[13px] font-medium text-ink">
                          {bm.chapterName || bm.title}
                        </p>
                        <p className="mt-0.5 text-[11px] tabular-nums text-ink-muted">
                          第 {bm.chapterIndex + 1} 章
                          {bm.createdAt ? ` · ${formatRelativeTime(bm.createdAt)}` : ""}
                        </p>
                      </div>
                      <button
                        type="button"
                        aria-label="删除书签"
                        onClick={(e) => {
                          e.stopPropagation();
                          void handleDeleteBookmark(bm);
                        }}
                        className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-ink-muted opacity-70 transition-colors hover:bg-danger-soft hover:text-danger group-hover:opacity-100"
                      >
                        <Trash2 className="size-3.5" />
                      </button>
                    </div>
                    {bm.bookText ? (
                      <p className="mt-2 line-clamp-2 border-l-2 border-line-strong pl-2.5 font-serif text-xs leading-relaxed text-ink-secondary">
                        {bm.bookText}
                      </p>
                    ) : null}
                    {bm.content ? (
                      <p className="mt-2 rounded-xl bg-subtle/75 px-2.5 py-1.5 text-xs text-ink">
                        {bm.content}
                      </p>
                    ) : null}
                  </div>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div className="space-y-3">
            {notes.length > 0 ? (
              <div className="flex flex-wrap items-center gap-3.5 sm:gap-1.5">
                {(
                  [
                    { value: "all", label: "全部" },
                    { value: "amber", label: "琥珀" },
                    { value: "sage", label: "竹青" },
                    { value: "vermilion", label: "朱砂" },
                  ] as const
                ).map((item) => (
                  <button
                    key={item.value}
                    type="button"
                    onClick={() => setColorFilter(item.value)}
                    className={cn(
                      "inline-flex cursor-pointer items-center gap-1.5 py-1 text-xs transition-colors sm:rounded-full sm:border sm:px-2.5",
                      colorFilter === item.value
                        ? "font-semibold text-accent sm:border-accent/40 sm:bg-accent-soft sm:font-medium"
                        : "text-ink-muted hover:text-ink sm:border-line sm:bg-elevated/60 sm:text-ink-secondary sm:hover:bg-subtle",
                    )}
                  >
                    {item.value !== "all" ? (
                      <span className={cn("size-2 rounded-full", NOTE_BAR_COLORS[item.value])} />
                    ) : null}
                    <span>{item.label}</span>
                  </button>
                ))}
              </div>
            ) : null}

            {filteredNotes.length === 0 ? (
              <EmptyView
                compact
                icon={<Highlighter className="size-5" />}
                title={notes.length === 0 ? "尚无划线批注" : "该色系下暂无划线"}
                description={
                  notes.length === 0
                    ? "在正文中划选任意句段，即可唤出墨盒添加荧光划线或批注。"
                    : "切换其他色系筛选查看全部笔记。"
                }
              />
            ) : (
              <div className="space-y-2.5">
                {filteredNotes.map((n) => (
                  <div
                    key={n.id}
                    onClick={() => {
                      onJumpTo(n.chapterIndex, n.paragraphIndex);
                      onClose();
                    }}
                    className="group relative cursor-pointer overflow-hidden rounded-2xl border border-line bg-elevated/75 p-3.5 pl-4 transition-colors hover:border-line-strong hover:bg-subtle/60"
                  >
                    <span
                      aria-hidden
                      className={cn("absolute inset-y-3 left-1.5 w-1 rounded-full", NOTE_BAR_COLORS[n.color])}
                    />
                    <div className="flex items-start justify-between gap-2">
                      <p className="font-serif text-[13px] leading-relaxed text-ink">
                        “{n.quote}”
                      </p>
                      <button
                        type="button"
                        aria-label="删除划线"
                        onClick={(e) => {
                          e.stopPropagation();
                          removeNote(bookUrl, n.id);
                          notify.info("已删除划线");
                        }}
                        className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-ink-muted opacity-70 transition-colors hover:bg-danger-soft hover:text-danger group-hover:opacity-100"
                      >
                        <Trash2 className="size-3.5" />
                      </button>
                    </div>
                    {n.note ? (
                      <p className="mt-2 rounded-xl bg-subtle/75 px-2.5 py-1.5 text-xs leading-relaxed text-ink-secondary">
                        {n.note}
                      </p>
                    ) : null}
                    <p className="mt-2 truncate text-[11px] text-ink-muted">
                      {n.chapterTitle || `第 ${n.chapterIndex + 1} 章`} ·{" "}
                      {formatRelativeTime(n.createdAt)}
                    </p>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
        </SheetTabPanel>
      </div>
    </Sheet>
  );
}
