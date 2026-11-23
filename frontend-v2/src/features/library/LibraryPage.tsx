import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowUpDown,
  BellDot,
  BookOpen,
  Check,
  CheckCheck,
  CheckSquare,
  Compass,
  Download,
  Ellipsis,
  FileUp,
  Folder,
  FolderCog,
  Grid2x2,
  Info,
  List,
  ListTree,
  RefreshCw,
  Search,
  Sparkles,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { booksApi, featuresApi } from "@/api/endpoints";
import type { Book, BookGroupWithCount } from "@/api/types";
import { BookInspectorSheet } from "@/features/book/BookInspectorSheet";
import { ImportBookSheet } from "@/features/book/ImportBookSheet";
import { GroupManagerSheet } from "@/features/library/GroupManagerSheet";
import { cn } from "@/lib/cn";
import { buildReaderUrl, cleanChapterTitle, cleanIntro, formatRelativeTime } from "@/lib/format";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button, IconButton } from "@/ui/Button";
import { EmptyView, InlineConfirm } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import {
  AnimatePresence,
  motion,
  PageEnter,
  SPRING_POP,
  SPRING_SILK,
  SPRING_SNAPPY,
  useReducedMotion,
} from "@/ui/motion";
import { Popover, PopoverDivider, PopoverItem } from "@/ui/Popover";

type SortKey = "recent" | "unread" | "title";
type ViewMode = "grid" | "list";
type GroupFilter = "all" | "ungrouped" | number;
type InspectorTab = "overview" | "toc" | "sources";

const LAYOUT_KEY = "reader.shelfLayout";

const SORT_LABELS: Record<SortKey, string> = {
  recent: "最近阅读",
  unread: "未读优先",
  title: "书名排序",
};

const GRID_CLASS = "grid grid-cols-3 gap-x-3.5 gap-y-9 sm:grid-cols-4 sm:gap-x-6 lg:grid-cols-5 xl:gap-x-8";

function getBookCover(book: Book): string | null | undefined {
  return book.customCoverUrl || book.displayCover || book.coverUrl;
}

function hasStarted(book: Book): boolean {
  return book.durChapterTime > 0 || book.durChapterIndex > 0;
}

function getProgressPct(book: Book): number {
  if (!book.totalChapterNum || book.totalChapterNum <= 0) return 0;
  const readCount = book.durChapterTime > 0 ? book.durChapterIndex + 1 : book.durChapterIndex;
  return Math.min(100, Math.max(0, Math.round((readCount / book.totalChapterNum) * 100)));
}

function getProgressLabel(book: Book): string {
  if (book.totalChapterNum > 0) {
    if (hasStarted(book)) {
      return `${Math.min(book.durChapterIndex + 1, book.totalChapterNum)} / ${book.totalChapterNum} 章`;
    }
    return `共 ${book.totalChapterNum} 章 · 未读`;
  }
  return cleanChapterTitle(book.durChapterTitle) || "";
}

function getUnreadCount(book: Book): number {
  return book.lastCheckCount > 0 ? book.lastCheckCount : 0;
}

function getRemainingChapters(book: Book): number {
  if (!book.totalChapterNum || book.totalChapterNum <= 0) return 0;
  return Math.max(0, book.totalChapterNum - 1 - book.durChapterIndex);
}

function isFinished(book: Book): boolean {
  return hasStarted(book) && book.totalChapterNum > 0 && getRemainingChapters(book) === 0 && getUnreadCount(book) === 0;
}

function isBookInGroup(book: Book, filter: GroupFilter): boolean {
  if (filter === "all") return true;
  const ids =
    book.groupIds && book.groupIds.length > 0
      ? book.groupIds
      : book.group > 0
        ? [book.group]
        : [];
  if (filter === "ungrouped") return ids.length === 0;
  return ids.includes(filter);
}

function isLocalBook(book: Book): boolean {
  return (
    book.origin === "local" ||
    book.origin === "loc_book" ||
    book.bookUrl.startsWith("local://")
  );
}

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

/** 在文本中柔和高亮命中的筛选词（大小写不敏感） */
function Highlight({ text, query }: { text: string; query: string }) {
  const q = query.trim();
  if (!q) return <>{text}</>;
  const idx = text.toLowerCase().indexOf(q.toLowerCase());
  if (idx < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, idx)}
      <mark className="rounded-[3px] bg-accent-soft px-0.5 text-accent">{text.slice(idx, idx + q.length)}</mark>
      {text.slice(idx + q.length)}
    </>
  );
}

export default function LibraryPage() {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const queryClient = useQueryClient();
  const reduced = useReducedMotion();

  /* ---------------- Queries ---------------- */
  const {
    data: shelf = [],
    isLoading: shelfLoading,
    error: shelfError,
    refetch: refetchShelf,
  } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(false),
    staleTime: 30_000,
  });

  const { data: rawGroups = [] } = useQuery({
    queryKey: ["groups"],
    queryFn: () => booksApi.getGroups(),
    staleTime: 60_000,
  });

  // 过滤掉后端负数系统伪分组，避免与“全部 / 未分组”重复
  const groups = React.useMemo(() => rawGroups.filter((g) => g.id > 0), [rawGroups]);

  /* ---------------- UI State ---------------- */
  const [activeGroup, setActiveGroup] = React.useState<GroupFilter>("all");
  const [searchQuery, setSearchQuery] = React.useState("");
  const [searchFocused, setSearchFocused] = React.useState(false);
  const searchInputRef = React.useRef<HTMLInputElement>(null);
  const [sortBy, setSortBy] = React.useState<SortKey>("recent");
  const [viewMode, setViewMode] = React.useState<ViewMode>(() => {
    try {
      return localStorage.getItem(LAYOUT_KEY) === "list" ? "list" : "grid";
    } catch {
      return "grid";
    }
  });
  const [refreshing, setRefreshing] = React.useState(false);

  const changeViewMode = (mode: ViewMode) => {
    setViewMode(mode);
    try {
      localStorage.setItem(LAYOUT_KEY, mode);
    } catch {
      // ignore
    }
  };

  // Sheets (v2 non-blocking spatial sheets)
  const [inspectorBook, setInspectorBook] = React.useState<Book | null>(null);
  const [inspectorTab, setInspectorTab] = React.useState<InspectorTab>("overview");
  const [importOpen, setImportOpen] = React.useState(false);
  const [importFile, setImportFile] = React.useState<File | null>(null);
  const [groupSheetOpen, setGroupSheetOpen] = React.useState(false);
  const anySheetOpen = Boolean(inspectorBook) || importOpen || groupSheetOpen;

  // Multi-select
  const [selectMode, setSelectMode] = React.useState(false);
  const [selectedUrls, setSelectedUrls] = React.useState<Set<string>>(() => new Set());
  const [batchBusy, setBatchBusy] = React.useState(false);

  // Drag & Drop Overlay
  const [dragOverlay, setDragOverlay] = React.useState(false);
  const [dragFileCount, setDragFileCount] = React.useState(0);
  const dragDepthRef = React.useRef(0);

  React.useEffect(() => {
    if (searchParams.get("import") === "1") {
      setImportOpen(true);
      const next = new URLSearchParams(searchParams);
      next.delete("import");
      setSearchParams(next, { replace: true });
    }
  }, [searchParams, setSearchParams]);

  React.useEffect(() => {
    if (typeof activeGroup === "number" && groups.length > 0 && !groups.some((g) => g.id === activeGroup)) {
      setActiveGroup("all");
    }
  }, [activeGroup, groups]);

  // Global drag-and-drop for .txt/.epub
  React.useEffect(() => {
    const hasFiles = (e: DragEvent) =>
      Boolean(e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files"));

    const onDragEnter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepthRef.current += 1;
      setDragFileCount(e.dataTransfer?.items?.length ?? 0);
      setDragOverlay(true);
    };
    const onDragOver = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    };
    const onDragLeave = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
      if (dragDepthRef.current === 0) setDragOverlay(false);
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepthRef.current = 0;
      setDragOverlay(false);
      const files = Array.from(e.dataTransfer?.files ?? []);
      const bookFile = files.find((f) => /\.(txt|epub)$/i.test(f.name)) ?? files[0];
      if (!bookFile) return;
      if (!/\.(txt|epub)$/i.test(bookFile.name)) {
        notify.error("仅支持拖入 .txt 或 .epub 电子书文件");
        return;
      }
      if (files.length > 1) {
        notify.info(`一次导入一本：已选取「${bookFile.name}」`);
      }
      setImportFile(bookFile);
      setImportOpen(true);
    };

    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onDragEnter);
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("dragleave", onDragLeave);
      window.removeEventListener("drop", onDrop);
    };
  }, []);

  const exitSelectMode = React.useCallback(() => {
    setSelectMode(false);
    setSelectedUrls(new Set());
  }, []);

  // 键盘：`/` 聚焦架内搜索；Esc 退出批量模式
  React.useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (anySheetOpen || e.defaultPrevented) return;
      if (e.key === "/" && !e.metaKey && !e.ctrlKey && !e.altKey && !isTypingTarget(e.target)) {
        e.preventDefault();
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
        return;
      }
      if (e.key === "Escape" && selectMode && !isTypingTarget(e.target)) {
        exitSelectMode();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [anySheetOpen, selectMode, exitSelectMode]);

  const ungroupedCount = React.useMemo(
    () => shelf.filter((b) => isBookInGroup(b, "ungrouped")).length,
    [shelf],
  );

  const updatedCount = React.useMemo(() => shelf.filter((b) => getUnreadCount(b) > 0).length, [shelf]);

  // 「继续阅读」展台：最近一次翻阅的书 + 其余最近在读
  const recentReading = React.useMemo(
    () =>
      shelf
        .filter((b) => b.durChapterTime > 0)
        .sort((a, b) => b.durChapterTime - a.durChapterTime)
        .slice(0, 4),
    [shelf],
  );

  const trimmedQuery = searchQuery.trim();

  const visibleBooks = React.useMemo(() => {
    const q = trimmedQuery.toLowerCase();
    const filtered = shelf.filter((b) => {
      if (!isBookInGroup(b, activeGroup)) return false;
      if (!q) return true;
      return (
        b.name.toLowerCase().includes(q) ||
        (b.author || "").toLowerCase().includes(q) ||
        (b.originName || "").toLowerCase().includes(q)
      );
    });

    return [...filtered].sort((a, b) => {
      if (sortBy === "title") {
        return a.name.localeCompare(b.name, "zh-Hans-CN");
      }
      if (sortBy === "unread") {
        const diffNew = getUnreadCount(b) - getUnreadCount(a);
        if (diffNew !== 0) return diffNew;
        const diffRem = getRemainingChapters(b) - getRemainingChapters(a);
        if (diffRem !== 0) return diffRem;
      }
      return (b.durChapterTime || b.createdAt || 0) - (a.durChapterTime || a.createdAt || 0);
    });
  }, [shelf, activeGroup, trimmedQuery, sortBy]);

  const showHero =
    !shelfLoading &&
    !shelfError &&
    recentReading.length > 0 &&
    activeGroup === "all" &&
    !trimmedQuery &&
    !selectMode;

  const isFiltering = Boolean(trimmedQuery) || activeGroup !== "all";

  /* ---------------- Actions ---------------- */
  const handleRefreshShelf = async () => {
    setRefreshing(true);
    try {
      const fresh = await booksApi.getShelf(true);
      queryClient.setQueryData(["shelf"], fresh);
      await queryClient.invalidateQueries({ queryKey: ["groups"] });
      const fresher = fresh.filter((b) => getUnreadCount(b) > 0).length;
      notify.success(fresher > 0 ? `检查完成：${fresher} 本书有新章节` : "书架已检查最新章节，暂无更新");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "刷新书架失败");
    } finally {
      setRefreshing(false);
    }
  };

  const openInspector = (book: Book, initial: InspectorTab = "overview") => {
    setInspectorBook(book);
    setInspectorTab(initial);
  };

  const openImport = () => {
    setImportFile(null);
    setImportOpen(true);
  };

  const readBook = (book: Book) => navigate(buildReaderUrl(book.bookUrl, book.durChapterIndex));

  const clearFilters = () => {
    setActiveGroup("all");
    setSearchQuery("");
  };

  const handleAssignGroup = async (bookUrl: string, groupId: number, groupName: string) => {
    try {
      await booksApi.setBookGroup(bookUrl, groupId);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已移至「${groupName}」`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "修改分组失败");
    }
  };

  const handleRemoveBook = async (book: Book) => {
    try {
      await booksApi.deleteBook(book.bookUrl);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已删除《${book.name}》`);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "删除失败");
    }
  };

  const handleQuickCache = (book: Book) => {
    if (isLocalBook(book)) {
      notify.info("本地书籍无需网络缓存");
      return;
    }
    notify.info(`已启动《${book.name}》整书后台缓存`);
    const stop = featuresApi.cacheBookStream(
      { url: book.bookUrl },
      {
        onData: (p) => {
          if (p.finished) {
            notify.success(`《${book.name}》已缓存 ${p.cached} 章`);
            stop();
          } else if (p.error) {
            notify.error(`《${book.name}》缓存异常：${p.error}`);
            stop();
          }
        },
        onError: (err) => {
          notify.error(err.message || "启动缓存失败");
        },
      },
    );
  };

  const toggleSelect = (bookUrl: string) => {
    setSelectedUrls((prev) => {
      const next = new Set(prev);
      if (next.has(bookUrl)) next.delete(bookUrl);
      else next.add(bookUrl);
      return next;
    });
  };

  const allVisibleSelected =
    visibleBooks.length > 0 && visibleBooks.every((b) => selectedUrls.has(b.bookUrl));

  const toggleSelectAllVisible = () => {
    if (allVisibleSelected) {
      setSelectedUrls(new Set());
    } else {
      setSelectedUrls(new Set(visibleBooks.map((b) => b.bookUrl)));
    }
  };

  const handleBatchMoveGroup = async (groupId: number, groupName: string, closePopover: () => void) => {
    if (selectedUrls.size === 0) return;
    setBatchBusy(true);
    try {
      await Promise.all(
        Array.from(selectedUrls).map((url) => booksApi.setBookGroup(url, groupId)),
      );
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已移动 ${selectedUrls.size} 本书到「${groupName}」`);
      closePopover();
      exitSelectMode();
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "批量分组失败");
    } finally {
      setBatchBusy(false);
    }
  };

  const handleBatchDelete = async () => {
    if (selectedUrls.size === 0) return;
    setBatchBusy(true);
    try {
      const urls = Array.from(selectedUrls);
      await booksApi.deleteBooks(urls);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      notify.success(`已删除 ${urls.length} 本书籍`);
      exitSelectMode();
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "批量删除失败");
    } finally {
      setBatchBusy(false);
    }
  };

  const cardHandlers = (book: Book) => ({
    book,
    groups,
    query: trimmedQuery,
    selectMode,
    selected: selectedUrls.has(book.bookUrl),
    onToggleSelect: () => toggleSelect(book.bookUrl),
    onRead: () => readBook(book),
    onInspect: (initial: InspectorTab) => openInspector(book, initial),
    onAssignGroup: (gid: number, gname: string) => void handleAssignGroup(book.bookUrl, gid, gname),
    onQuickCache: () => handleQuickCache(book),
    onRemove: () => handleRemoveBook(book),
  });

  return (
    <PageEnter className="mx-auto flex min-h-full w-full max-w-6xl flex-col px-4 pt-3 pb-24 sm:px-6 md:px-10 md:pt-8">
      {/* 1. Page Header & Utility Bar (Mobile hides redundant title already shown in AppShell top bar) */}
      <section className="mb-3.5 flex flex-col justify-between gap-2.5 border-b border-line/70 pb-3 md:mb-6 md:gap-4 md:border-line md:pb-5 lg:flex-row lg:items-end">
        <div className={cn("min-w-0", updatedCount === 0 && "hidden md:block")}>
          <p className="mb-1.5 hidden text-[11px] font-medium tracking-[0.22em] text-accent/90 uppercase md:block">
            GOOD STORIES, QUIET MOMENTS
          </p>
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h1 className="hidden font-serif text-2xl font-semibold tracking-wide text-ink md:block md:text-[28px]">
              书架
            </h1>
            {shelf.length > 0 ? (
              <span className="hidden text-xs text-ink-muted tabular-nums md:inline">
                共 {shelf.length} 本藏书
              </span>
            ) : null}
            {updatedCount > 0 ? (
              <button
                type="button"
                onClick={() => {
                  setActiveGroup("all");
                  setSortBy("unread");
                }}
                title="按未读更新优先排序"
                className="inline-flex cursor-pointer items-center gap-1 rounded-full border border-accent/25 bg-accent-soft px-2 py-0.5 text-[11px] font-medium text-accent transition-colors hover:border-accent/45 hover:bg-accent/15 tabular-nums"
              >
                <BellDot className="size-3" />
                {updatedCount} 本有更新
              </button>
            ) : null}
          </div>
          <p className="mt-1.5 hidden max-w-2xl text-[13px] leading-relaxed text-ink-muted sm:block">
            收纳你的故事，每一次打开都从上次停下的地方继续。
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2 lg:shrink-0 lg:flex-nowrap">
          <div className="relative w-full sm:w-56">
            <Input
              ref={searchInputRef}
              size="sm"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onFocus={() => setSearchFocused(true)}
              onBlur={() => setSearchFocused(false)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  if (searchQuery) setSearchQuery("");
                  else e.currentTarget.blur();
                }
              }}
              placeholder="搜索书名、作者或书源"
              aria-label="在书架中筛选（快捷键 /）"
              leftIcon={<Search className="size-3.5" />}
              clearable
              onClear={() => setSearchQuery("")}
              className="w-full"
            />
            {!searchQuery && !searchFocused ? (
              <kbd
                aria-hidden
                className="pointer-events-none absolute top-1/2 right-2.5 hidden -translate-y-1/2 items-center justify-center rounded border border-line-strong/80 bg-subtle px-1.5 py-0.5 font-mono text-[10px] leading-none text-ink-muted sm:inline-flex"
              >
                /
              </kbd>
            ) : null}
          </div>

          <div className="flex w-full items-center justify-between gap-1.5 sm:w-auto sm:justify-start">
            {/* Layout Toggles */}
            <div
              role="group"
              aria-label="书架视图切换"
              className="inline-flex shrink-0 items-center rounded-xl border border-line bg-elevated p-0.5"
            >
              <button
                type="button"
                onClick={() => changeViewMode("grid")}
                aria-label="网格布局"
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
                aria-label="列表布局"
                aria-pressed={viewMode === "list"}
                title="列表视图"
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

            <div className="flex items-center gap-0.5 sm:gap-1.5">
              {/* Sort Popover */}
              <Popover
                align="end"
                trigger={
                  <button
                    type="button"
                    title={`排序：${SORT_LABELS[sortBy]}`}
                    aria-label="排序方式"
                    className={cn(
                      "inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/55 sm:size-8 sm:justify-center sm:rounded-full sm:px-0",
                      sortBy !== "recent"
                        ? "bg-accent-soft text-accent"
                        : "text-ink-secondary hover:bg-subtle hover:text-ink",
                    )}
                  >
                    <ArrowUpDown className="size-3.5 sm:size-4" />
                    <span className="sm:hidden">排序</span>
                  </button>
                }
              >
                {(close) => (
                  <div>
                    <p className="px-2.5 pt-1 pb-1.5 text-[11px] font-medium text-ink-muted">
                      排序方式
                    </p>
                    {(["recent", "unread", "title"] as const).map((key) => (
                      <PopoverItem
                        key={key}
                        icon={
                          sortBy === key ? (
                            <Check className="size-3.5 text-accent" />
                          ) : (
                            <span className="size-3.5" />
                          )
                        }
                        onClick={() => {
                          setSortBy(key);
                          close();
                        }}
                      >
                        {SORT_LABELS[key]}
                      </PopoverItem>
                    ))}
                  </div>
                )}
              </Popover>

              {/* Import Local Book */}
              <button
                type="button"
                title="导入本地 TXT / EPUB"
                aria-label="导入本地书籍"
                onClick={openImport}
                className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary outline-none transition-colors hover:bg-subtle hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 sm:size-8 sm:justify-center sm:rounded-full sm:px-0"
              >
                <Upload className="size-3.5 sm:size-4" />
                <span className="sm:hidden">导入</span>
              </button>

              {/* Refresh Shelf */}
              <button
                type="button"
                disabled={refreshing}
                title="刷新检查更新"
                aria-label="刷新书架"
                onClick={() => void handleRefreshShelf()}
                className="inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium text-ink-secondary outline-none transition-colors hover:bg-subtle hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 disabled:opacity-45 sm:size-8 sm:justify-center sm:rounded-full sm:px-0"
              >
                <RefreshCw className={cn("size-3.5 sm:size-4", refreshing && "animate-spin")} />
                <span className="sm:hidden">刷新</span>
              </button>

              {/* Multi-select */}
              <button
                type="button"
                title={selectMode ? "退出批量整理 (Esc)" : "批量整理书架"}
                aria-label="批量管理书架"
                onClick={() => {
                  if (selectMode) exitSelectMode();
                  else setSelectMode(true);
                }}
                className={cn(
                  "inline-flex h-8 cursor-pointer items-center gap-1 rounded-lg px-2 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/55 sm:size-8 sm:justify-center sm:rounded-full sm:px-0",
                  selectMode
                    ? "bg-accent-soft text-accent"
                    : "text-ink-secondary hover:bg-subtle hover:text-ink",
                )}
              >
                <CheckSquare className="size-3.5 sm:size-4" />
                <span className="sm:hidden">{selectMode ? "完成" : "管理"}</span>
              </button>
            </div>
          </div>
        </div>
      </section>

      {/* 2. Continue Reading Spotlight (Desktop Hero — hidden on mobile to keep shelf calm & avoid box-in-box duplication) */}
      <AnimatePresence initial={false}>
        {showHero ? (
          <motion.section
            key="continue-hero"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: -8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: -6 }}
            transition={reduced ? { duration: 0.12 } : SPRING_SILK}
            className="mb-6 hidden md:block md:mb-7"
          >
            <ContinueReadingSpotlight
              primary={recentReading[0]}
              others={recentReading.slice(1)}
              onRead={readBook}
              onInspect={openInspector}
            />
          </motion.section>
        ) : null}
      </AnimatePresence>

      {/* 3. Group Tabs Row */}
      <div className="mb-4 flex items-center justify-between gap-2 md:mb-5">
        <div className="no-scrollbar scroll-fade-x flex min-w-0 flex-1 items-center gap-4 overflow-x-auto py-0.5 pr-4 md:gap-1.5">
          <GroupPill
            active={activeGroup === "all"}
            label="全部"
            count={shelf.length}
            onClick={() => setActiveGroup("all")}
          />
          {groups.map((g) => (
            <GroupPill
              key={g.id}
              active={activeGroup === g.id}
              label={g.name}
              count={g.bookCount}
              onClick={() => setActiveGroup(g.id)}
            />
          ))}
          {groups.length > 0 && ungroupedCount > 0 ? (
            <GroupPill
              active={activeGroup === "ungrouped"}
              label="未分组"
              count={ungroupedCount}
              onClick={() => setActiveGroup("ungrouped")}
            />
          ) : null}
        </div>

        <Button
          variant="ghost"
          size="xs"
          onClick={() => setGroupSheetOpen(true)}
          className="shrink-0 text-ink-muted hover:text-ink"
        >
          <FolderCog className="size-3.5" />
          <span className="hidden sm:inline">管理分组</span>
        </Button>
      </div>

      {/* Active filter / select context bar */}
      <AnimatePresence initial={false}>
        {isFiltering && shelf.length > 0 ? (
          <motion.div
            key="filter-bar"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: -4 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: -4 }}
            transition={{ duration: 0.16 }}
            className="mb-4 flex flex-wrap items-center justify-between gap-2 rounded-xl border border-line bg-subtle/45 px-3.5 py-2 text-xs text-ink-secondary"
          >
            <div className="flex flex-wrap items-center gap-2">
              <span>
                筛选结果 <strong className="font-semibold text-ink tabular-nums">{visibleBooks.length}</strong> /{" "}
                <span className="tabular-nums">{shelf.length}</span> 本
              </span>
              {trimmedQuery ? (
                <span className="inline-flex items-center gap-1 rounded-full border border-accent/25 bg-accent-soft px-2 py-0.5 text-[11px] text-accent">
                  关键词：{trimmedQuery}
                  <button
                    type="button"
                    aria-label="清除搜索关键词"
                    onClick={() => setSearchQuery("")}
                    className="ml-0.5 cursor-pointer rounded-full p-0.5 hover:bg-accent/15"
                  >
                    <X className="size-2.5" />
                  </button>
                </span>
              ) : null}
            </div>
            <button
              type="button"
              onClick={clearFilters}
              className="cursor-pointer text-[11px] font-medium text-ink-muted underline-offset-4 transition-colors hover:text-ink hover:underline"
            >
              重置筛选
            </button>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* 4. Bookshelf Grid / List */}
      {shelfLoading ? (
        viewMode === "grid" ? (
          <div className={GRID_CLASS}>
            {Array.from({ length: 10 }).map((_, i) => (
              <div key={i} className="space-y-2.5">
                <div className="aspect-3/4 w-full animate-pulse rounded-r-md rounded-l-xs bg-subtle" />
                <div className="mx-auto h-3.5 w-3/4 animate-pulse rounded bg-subtle" />
                <div className="mx-auto h-2.5 w-1/2 animate-pulse rounded bg-subtle/80" />
              </div>
            ))}
          </div>
        ) : (
          <div className="divide-y divide-line/70 md:flex md:flex-col md:gap-3 md:divide-y-0">
            {Array.from({ length: 6 }).map((_, i) => (
              <div
                key={i}
                className="flex items-center gap-3.5 py-3.5 md:gap-4 md:rounded-2xl md:border md:border-line md:bg-elevated/60 md:p-4"
              >
                <div className="aspect-3/4 w-14 shrink-0 animate-pulse rounded bg-subtle sm:w-16" />
                <div className="flex-1 space-y-2">
                  <div className="h-4 w-40 animate-pulse rounded bg-subtle" />
                  <div className="h-3 w-56 animate-pulse rounded bg-subtle/80" />
                  <div className="h-3 w-4/5 animate-pulse rounded bg-subtle/60" />
                </div>
              </div>
            ))}
          </div>
        )
      ) : shelfError ? (
        <div className="rounded-2xl border border-dashed border-line bg-elevated/60 py-12">
          <EmptyView
            icon={<BookOpen className="size-5" />}
            title="未能加载书架"
            description={shelfError instanceof Error ? shelfError.message : "请检查服务端连接"}
            action={
              <Button variant="primary" size="sm" onClick={() => void refetchShelf()}>
                重新加载
              </Button>
            }
          />
        </div>
      ) : shelf.length === 0 ? (
        <EmptyShelfGuide
          onSearch={() => navigate("/discover")}
          onExplore={() => navigate("/explore")}
          onImport={openImport}
        />
      ) : visibleBooks.length === 0 ? (
        <div className="rounded-2xl border border-dashed border-line bg-elevated/60 py-10">
          <EmptyView
            compact
            icon={<Search className="size-5" />}
            title="当前条件下没有匹配的书籍"
            description={
              trimmedQuery
                ? `未在当前分组找到包含「${trimmedQuery}」的书名、作者或书源。`
                : "该分组下还没有放入藏书，可点击书籍右上角菜单归入此分组。"
            }
            action={
              <>
                <Button size="sm" variant="secondary" onClick={clearFilters}>
                  查看全部藏书
                </Button>
                {trimmedQuery ? (
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => navigate(`/discover?q=${encodeURIComponent(trimmedQuery)}`)}
                  >
                    <Search className="size-3.5" />
                    去全网搜索「{trimmedQuery}」
                  </Button>
                ) : null}
              </>
            }
          />
        </div>
      ) : viewMode === "grid" ? (
        <div className={GRID_CLASS}>
          {visibleBooks.map((book) => (
            <ShelfGridCard key={book.bookUrl} {...cardHandlers(book)} />
          ))}
        </div>
      ) : (
        <div className="divide-y divide-line/70 md:flex md:flex-col md:gap-3 md:divide-y-0">
          {visibleBooks.map((book) => (
            <ShelfListCard key={book.bookUrl} {...cardHandlers(book)} />
          ))}
        </div>
      )}

      {/* Multi-select Floating Bar */}
      <AnimatePresence>
        {selectMode ? (
          <motion.div
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 24, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: 20, scale: 0.96 }}
            transition={SPRING_POP}
            className="surface-glass fixed bottom-[calc(4.25rem+env(safe-area-inset-bottom,0px))] left-1/2 z-40 flex max-w-[calc(100vw-1.5rem)] -translate-x-1/2 flex-wrap items-center justify-center gap-1.5 rounded-2xl border border-line-strong/80 px-3 py-2 shadow-[var(--shadow-float)] sm:flex-nowrap sm:gap-2 sm:rounded-full sm:px-3.5 md:bottom-6"
          >
            <div className="flex items-center gap-1.5 pr-1 pl-1 text-xs font-medium text-ink tabular-nums">
              <span className="flex size-5 items-center justify-center rounded-full bg-accent-soft text-[11px] font-semibold text-accent">
                {selectedUrls.size}
              </span>
              <span className="text-ink-secondary">
                / {visibleBooks.length} 本
              </span>
            </div>

            <Button size="xs" variant="ghost" onClick={toggleSelectAllVisible}>
              <CheckCheck className="size-3.5" />
              {allVisibleSelected ? "取消全选" : "全选本页"}
            </Button>

            <span className="hidden h-4 w-px bg-line sm:inline-block" aria-hidden />

            <Popover
              side="top"
              align="end"
              trigger={
                <Button
                  size="xs"
                  variant="secondary"
                  disabled={selectedUrls.size === 0 || batchBusy}
                >
                  <Folder className="size-3.5" />
                  移动分组
                </Button>
              }
            >
              {(close) => (
                <div className="max-h-60 overflow-y-auto">
                  <p className="px-2.5 pt-1 pb-1 text-[11px] font-medium text-ink-muted">
                    移至目标分组
                  </p>
                  <PopoverItem
                    icon={<Folder className="size-3.5" />}
                    onClick={() => void handleBatchMoveGroup(0, "未分组", close)}
                  >
                    未分组
                  </PopoverItem>
                  {groups.map((g) => (
                    <PopoverItem
                      key={g.id}
                      icon={<Folder className="size-3.5" />}
                      onClick={() => void handleBatchMoveGroup(g.id, g.name, close)}
                    >
                      {g.name}
                    </PopoverItem>
                  ))}
                </div>
              )}
            </Popover>

            <InlineConfirm
              confirmLabel={`删除 ${selectedUrls.size} 本`}
              busy={batchBusy}
              onConfirm={handleBatchDelete}
              trigger={(arm) => (
                <Button
                  size="xs"
                  variant="danger"
                  disabled={selectedUrls.size === 0 || batchBusy}
                  onClick={arm}
                >
                  <Trash2 className="size-3.5" />
                  删除
                </Button>
              )}
            />

            <Button size="xs" variant="primary" onClick={exitSelectMode}>
              完成
            </Button>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* Drag-and-Drop Overlay */}
      <AnimatePresence>
        {dragOverlay ? (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16 }}
            className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-bg/84 p-6 backdrop-blur-md"
          >
            <motion.div
              initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.94, y: 12 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.96, y: 8 }}
              transition={SPRING_POP}
              className="relative flex w-full max-w-md flex-col items-center gap-4 overflow-hidden rounded-3xl border-2 border-dashed border-accent/65 bg-elevated/95 px-10 py-12 text-center shadow-[var(--shadow-float)]"
            >
              <div className="pointer-events-none absolute inset-x-0 top-0 h-24 bg-gradient-to-b from-accent/10 to-transparent" />
              <motion.div
                animate={reduced ? undefined : { y: [0, -5, 0] }}
                transition={{ duration: 1.6, repeat: Infinity, ease: "easeInOut" }}
                className="relative flex size-14 items-center justify-center rounded-2xl bg-accent-soft text-accent shadow-[var(--shadow-sm)]"
              >
                <FileUp className="size-7" />
              </motion.div>
              <div className="space-y-1.5">
                <p className="font-serif text-xl font-semibold text-ink">
                  松手即可导入电子书
                </p>
                <p className="text-xs leading-relaxed text-ink-muted">
                  自动识别字符编码、切分章节目录，并预览确认后再收入书房
                </p>
              </div>
              <div className="flex items-center gap-2 pt-1">
                <span className="rounded-full border border-accent/25 bg-accent-soft px-2.5 py-0.5 font-mono text-[11px] font-medium text-accent">
                  .TXT
                </span>
                <span className="rounded-full border border-accent/25 bg-accent-soft px-2.5 py-0.5 font-mono text-[11px] font-medium text-accent">
                  .EPUB
                </span>
                {dragFileCount > 1 ? (
                  <span className="rounded-full border border-line bg-subtle px-2.5 py-0.5 text-[11px] text-ink-muted tabular-nums">
                    检测到 {dragFileCount} 个文件
                  </span>
                ) : null}
              </div>
            </motion.div>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* Non-blocking Spatial Sheets */}
      <BookInspectorSheet
        open={Boolean(inspectorBook)}
        onClose={() => setInspectorBook(null)}
        book={inspectorBook}
        initialTab={inspectorTab}
        onSourceSwitched={(updated) => {
          if ("isInShelf" in updated) {
            setInspectorBook(updated);
          }
        }}
      />

      <ImportBookSheet
        open={importOpen}
        onClose={() => {
          setImportOpen(false);
          setImportFile(null);
        }}
        initialFile={importFile}
      />

      <GroupManagerSheet
        open={groupSheetOpen}
        onClose={() => setGroupSheetOpen(false)}
      />
    </PageEnter>
  );
}

/* ---------------- Sub-components ---------------- */

function ContinueReadingSpotlight({
  primary,
  others,
  onRead,
  onInspect,
}: {
  primary: Book;
  others: Book[];
  onRead: (book: Book) => void;
  onInspect: (book: Book, tab: InspectorTab) => void;
}) {
  const pct = getProgressPct(primary);
  const unread = getUnreadCount(primary);
  const remaining = getRemainingChapters(primary);
  const chapterLabel = cleanChapterTitle(primary.durChapterTitle);
  const lastReadLabel = formatRelativeTime(primary.durChapterTime);
  const metaParts = [primary.author?.trim(), primary.originName?.trim()].filter(Boolean);

  return (
    <div className="relative overflow-hidden rounded-2xl border border-line bg-elevated/90 shadow-[var(--shadow-sm)]">
      {/* Subtle warm editorial wash */}
      <div
        aria-hidden
        className="pointer-events-none absolute -top-20 -right-16 size-64 rounded-full bg-accent/[0.06] blur-3xl"
      />

      <div className="relative grid gap-5 p-4 sm:p-5 lg:grid-cols-[minmax(0,1fr)_270px] lg:gap-6 lg:p-6">
        {/* Primary Spotlight Book */}
        <div className="flex min-w-0 items-start gap-4 sm:gap-5">
          <button
            type="button"
            onClick={() => onRead(primary)}
            aria-label={`继续阅读《${primary.name}》`}
            className="relative w-20 shrink-0 cursor-pointer outline-none sm:w-24"
          >
            <BookCover
              src={getBookCover(primary)}
              referer={primary.origin}
              title={primary.name}
              author={primary.author}
              pageEdges
              interactive
            />
            {unread > 0 ? (
              <span className="pointer-events-none absolute -top-1.5 -right-1.5 z-20 rounded-full bg-accent px-1.5 py-0.5 text-[10px] font-bold leading-none text-accent-ink shadow-sm ring-2 ring-elevated tabular-nums">
                +{unread > 99 ? "99" : unread}
              </span>
            ) : null}
          </button>

          <div className="flex min-w-0 flex-1 flex-col justify-between self-stretch">
            <div>
              <div className="flex flex-wrap items-center gap-2">
                <span className="inline-flex items-center gap-1 rounded-full border border-accent/25 bg-accent-soft px-2 py-0.5 text-[11px] font-medium text-accent">
                  <BookOpen className="size-3" />
                  继续阅读
                </span>
                {lastReadLabel ? (
                  <span className="text-[11px] text-ink-muted">
                    上次阅读 · {lastReadLabel}
                  </span>
                ) : null}
                {unread > 0 ? (
                  <span className="rounded-full bg-accent px-1.5 py-0.5 text-[10px] font-semibold leading-none text-accent-ink tabular-nums">
                    新更 {unread} 章
                  </span>
                ) : null}
              </div>

              <button
                type="button"
                onClick={() => onRead(primary)}
                className="mt-2 block max-w-full cursor-pointer text-left outline-none group"
              >
                <h2 className="truncate font-serif text-lg font-semibold tracking-wide text-ink transition-colors group-hover:text-accent sm:text-xl">
                  {primary.name}
                </h2>
              </button>

              {metaParts.length > 0 ? (
                <p className="mt-0.5 truncate text-xs text-ink-muted">
                  {metaParts.join(" · ")}
                </p>
              ) : null}

              {chapterLabel ? (
                <p className="mt-2 truncate text-xs text-ink-secondary">
                  读至：{chapterLabel}
                </p>
              ) : null}
            </div>

            {/* Progress + Actions */}
            <div className="mt-3.5 space-y-3">
              {primary.totalChapterNum > 0 ? (
                <div className="space-y-1.5">
                  <div className="flex items-center justify-between gap-2 text-[11px] text-ink-muted tabular-nums">
                    <span>
                      第 {Math.min(primary.durChapterIndex + 1, primary.totalChapterNum)} /{" "}
                      {primary.totalChapterNum} 章
                      {remaining > 0 ? ` · 剩余 ${remaining} 章` : " · 已追平最新"}
                    </span>
                    <span className="font-semibold text-accent">{pct}%</span>
                  </div>
                  <div className="h-1.5 w-full overflow-hidden rounded-full bg-subtle">
                    <div
                      className="h-full rounded-full bg-accent transition-[width] duration-300"
                      style={{ width: `${Math.max(2, pct)}%` }}
                    />
                  </div>
                </div>
              ) : null}

              <div className="flex flex-wrap items-center gap-2">
                <Button variant="primary" size="sm" onClick={() => onRead(primary)}>
                  <BookOpen className="size-3.5" />
                  展卷续读
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => onInspect(primary, "toc")}
                >
                  <ListTree className="size-3.5" />
                  目录
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => onInspect(primary, "overview")}
                >
                  <Info className="size-3.5" />
                  详情
                </Button>
              </div>
            </div>
          </div>
        </div>

        {/* Secondary Recently Read Rail */}
        {others.length > 0 ? (
          <div className="flex flex-col justify-between border-t border-line/80 pt-3.5 lg:border-t-0 lg:border-l lg:pt-0 lg:pl-5">
            <p className="mb-2 text-[11px] font-medium tracking-wider text-ink-muted uppercase">
              最近翻阅
            </p>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-3 lg:grid-cols-1">
              {others.map((book) => {
                const bPct = getProgressPct(book);
                const bUnread = getUnreadCount(book);
                return (
                  <button
                    key={book.bookUrl}
                    type="button"
                    onClick={() => onRead(book)}
                    className="group flex min-w-0 cursor-pointer items-center gap-2.5 rounded-xl border border-transparent bg-subtle/45 px-2.5 py-2 text-left transition-colors hover:border-line hover:bg-subtle/90"
                  >
                    <div className="w-8 shrink-0">
                      <BookCover
                        src={getBookCover(book)}
                        referer={book.origin}
                        title={book.name}
                        author={book.author}
                        interactive={false}
                      />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-1.5">
                        <p className="truncate text-xs font-medium text-ink transition-colors group-hover:text-accent">
                          {book.name}
                        </p>
                        {bUnread > 0 ? (
                          <span className="shrink-0 rounded-full bg-accent px-1.5 py-0.5 text-[9px] font-bold leading-none text-accent-ink tabular-nums">
                            +{bUnread > 99 ? "99" : bUnread}
                          </span>
                        ) : null}
                      </div>
                      <div className="mt-1 flex items-center gap-2">
                        {book.totalChapterNum > 0 ? (
                          <>
                            <div className="h-1 flex-1 overflow-hidden rounded-full bg-bg">
                              <div
                                className="h-full rounded-full bg-accent/80"
                                style={{ width: `${Math.max(3, bPct)}%` }}
                              />
                            </div>
                            <span className="shrink-0 text-[10px] text-ink-muted tabular-nums">
                              {bPct}%
                            </span>
                          </>
                        ) : (
                          <span className="truncate text-[10px] text-ink-muted">
                            {formatRelativeTime(book.durChapterTime) || "继续阅读"}
                          </span>
                        )}
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}

function EmptyShelfGuide({
  onSearch,
  onExplore,
  onImport,
}: {
  onSearch: () => void;
  onExplore: () => void;
  onImport: () => void;
}) {
  return (
    <div className="rounded-2xl border border-dashed border-line-strong/80 bg-elevated/60 px-6 py-12 sm:px-10">
      <EmptyView
        compact
        icon={<BookOpen className="size-5 text-accent" />}
        title="你的私人书房还是空的"
        description="全网聚合搜书、在书海画廊逛排行榜，或直接将本地 .txt / .epub 拖入此页面开始阅读。"
      />
      <div className="mx-auto mt-4 grid max-w-2xl grid-cols-1 gap-3 sm:grid-cols-3">
        <button
          type="button"
          onClick={onSearch}
          className="group flex cursor-pointer flex-col items-start gap-2 rounded-2xl border border-line bg-elevated p-4 text-left transition-all hover:border-accent/45 hover:shadow-[var(--shadow-sm)]"
        >
          <span className="flex size-9 items-center justify-center rounded-xl bg-accent-soft text-accent transition-transform group-hover:scale-105">
            <Search className="size-4" />
          </span>
          <div>
            <p className="font-serif text-sm font-semibold text-ink">全网聚合搜索</p>
            <p className="mt-0.5 text-[11px] leading-relaxed text-ink-muted">
              多书源实时并行找书，支持一键试读与入架
            </p>
          </div>
        </button>

        <button
          type="button"
          onClick={onExplore}
          className="group flex cursor-pointer flex-col items-start gap-2 rounded-2xl border border-line bg-elevated p-4 text-left transition-all hover:border-accent/45 hover:shadow-[var(--shadow-sm)]"
        >
          <span className="flex size-9 items-center justify-center rounded-xl bg-subtle text-ink-secondary transition-colors group-hover:bg-accent-soft group-hover:text-accent">
            <Compass className="size-4" />
          </span>
          <div>
            <p className="font-serif text-sm font-semibold text-ink">逛书海画廊</p>
            <p className="mt-0.5 text-[11px] leading-relaxed text-ink-muted">
              按书店频道与分类榜单发现口碑好书
            </p>
          </div>
        </button>

        <button
          type="button"
          onClick={onImport}
          className="group flex cursor-pointer flex-col items-start gap-2 rounded-2xl border border-line bg-elevated p-4 text-left transition-all hover:border-accent/45 hover:shadow-[var(--shadow-sm)]"
        >
          <span className="flex size-9 items-center justify-center rounded-xl bg-subtle text-ink-secondary transition-colors group-hover:bg-accent-soft group-hover:text-accent">
            <Upload className="size-4" />
          </span>
          <div>
            <p className="font-serif text-sm font-semibold text-ink">导入本地藏书</p>
            <p className="mt-0.5 text-[11px] leading-relaxed text-ink-muted">
              点击选取或直接拖入 TXT / EPUB 文件
            </p>
          </div>
        </button>
      </div>
    </div>
  );
}

function GroupPill({
  active,
  label,
  count,
  onClick,
}: {
  active: boolean;
  label: string;
  count: number;
  onClick: () => void;
}) {
  const reduced = useReducedMotion();
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "relative z-10 inline-flex shrink-0 cursor-pointer items-center gap-1.5 py-1.5 text-[13px] whitespace-nowrap transition-colors outline-none focus-visible:ring-2 focus-visible:ring-accent/60 md:rounded-full md:px-3.5 md:text-xs md:font-medium",
        active
          ? "font-semibold text-accent"
          : "text-ink-muted hover:text-ink md:hover:bg-subtle",
      )}
    >
      {active ? (
        <motion.span
          layoutId="library-group-pill"
          transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
          className="pointer-events-none absolute inset-x-0 bottom-0 h-[2px] rounded-full bg-accent md:inset-0 md:-z-10 md:h-auto md:rounded-full md:border md:border-accent/35 md:bg-accent-soft"
        />
      ) : null}
      <span>{label}</span>
      <span className={cn("text-[11px] tabular-nums", active ? "text-accent/85" : "opacity-70")}>
        {count}
      </span>
    </button>
  );
}

function BookActionPopover({
  book,
  groups,
  onInspect,
  onAssignGroup,
  onQuickCache,
  onRemove,
  trigger,
}: {
  book: Book;
  groups: BookGroupWithCount[];
  onInspect: (tab: InspectorTab) => void;
  onAssignGroup: (groupId: number, groupName: string) => void;
  onQuickCache: () => void;
  onRemove: () => Promise<void>;
  trigger: React.ReactNode;
}) {
  const [groupMenuOpen, setGroupMenuOpen] = React.useState(false);

  return (
    <Popover
      align="end"
      onOpenChange={(o) => {
        if (!o) setGroupMenuOpen(false);
      }}
      trigger={trigger}
      className="min-w-[184px]"
    >
      {(close) => (
        <div>
          <PopoverItem
            icon={<Info className="size-3.5" />}
            onClick={() => {
              close();
              onInspect("overview");
            }}
          >
            书籍详情
          </PopoverItem>
          <PopoverItem
            icon={<ListTree className="size-3.5" />}
            onClick={() => {
              close();
              onInspect("toc");
            }}
          >
            查看目录
          </PopoverItem>
          {!isLocalBook(book) ? (
            <PopoverItem
              icon={<Sparkles className="size-3.5" />}
              onClick={() => {
                close();
                onInspect("sources");
              }}
            >
              切换书源
            </PopoverItem>
          ) : null}

          <PopoverItem
            icon={<Folder className="size-3.5" />}
            onClick={() => setGroupMenuOpen((v) => !v)}
          >
            移动分组
          </PopoverItem>
          {groupMenuOpen ? (
            <div className="mx-1.5 mb-1 max-h-36 space-y-0.5 overflow-y-auto rounded-lg bg-subtle/70 p-1">
              <button
                type="button"
                onClick={() => {
                  onAssignGroup(0, "未分组");
                  close();
                }}
                className="flex w-full cursor-pointer items-center justify-between rounded-md px-2.5 py-1.5 text-left text-xs text-ink-secondary hover:bg-elevated hover:text-ink"
              >
                <span>未分组</span>
                {book.group === 0 && (!book.groupIds || book.groupIds.length === 0) ? (
                  <Check className="size-3 text-accent" />
                ) : null}
              </button>
              {groups.map((g) => {
                const active = isBookInGroup(book, g.id);
                return (
                  <button
                    key={g.id}
                    type="button"
                    onClick={() => {
                      onAssignGroup(g.id, g.name);
                      close();
                    }}
                    className="flex w-full cursor-pointer items-center justify-between rounded-md px-2.5 py-1.5 text-left text-xs text-ink-secondary hover:bg-elevated hover:text-ink"
                  >
                    <span className="truncate">{g.name}</span>
                    {active ? <Check className="size-3 shrink-0 text-accent" /> : null}
                  </button>
                );
              })}
            </div>
          ) : null}

          {!isLocalBook(book) ? (
            <PopoverItem
              icon={<Download className="size-3.5" />}
              onClick={() => {
                close();
                onQuickCache();
              }}
            >
              缓存本书
            </PopoverItem>
          ) : null}

          <PopoverDivider />

          <div className="px-1 py-0.5">
            <InlineConfirm
              confirmLabel="确认删除"
              onConfirm={async () => {
                await onRemove();
                close();
              }}
              trigger={(arm) => (
                <PopoverItem
                  danger
                  icon={<Trash2 className="size-3.5" />}
                  onClick={arm}
                >
                  删除
                </PopoverItem>
              )}
            />
          </div>
        </div>
      )}
    </Popover>
  );
}

interface CardProps {
  book: Book;
  groups: BookGroupWithCount[];
  query: string;
  selectMode: boolean;
  selected: boolean;
  onToggleSelect: () => void;
  onRead: () => void;
  onInspect: (tab: InspectorTab) => void;
  onAssignGroup: (groupId: number, groupName: string) => void;
  onQuickCache: () => void;
  onRemove: () => Promise<void>;
}

function ShelfGridCard({
  book,
  groups,
  query,
  selectMode,
  selected,
  onToggleSelect,
  onRead,
  onInspect,
  onAssignGroup,
  onQuickCache,
  onRemove,
}: CardProps) {
  const unread = getUnreadCount(book);
  const pct = getProgressPct(book);
  const progressText = getProgressLabel(book);
  const started = hasStarted(book);
  const finished = isFinished(book);
  const local = isLocalBook(book);

  return (
    <div
      className={cn(
        "group relative flex select-none flex-col rounded-xl",
        selectMode && "cursor-pointer",
      )}
      onClick={() => {
        if (selectMode) onToggleSelect();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onInspect("overview");
      }}
    >
      <div
        className={cn(
          "relative rounded-r-md rounded-l-xs transition-all",
          selectMode && selected && "ring-2 ring-accent ring-offset-2 ring-offset-bg",
          selectMode && !selected && "opacity-85 hover:opacity-100",
        )}
      >
        <div
          role="button"
          tabIndex={0}
          aria-label={`阅读《${book.name}》`}
          onClick={(e) => {
            if (selectMode) return;
            e.stopPropagation();
            onRead();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              if (selectMode) onToggleSelect();
              else onRead();
            }
          }}
          className="cursor-pointer outline-none"
        >
          <BookCover
            src={getBookCover(book)}
            referer={book.origin}
            title={book.name}
            author={book.author}
            intro={book.intro}
            interactive={!selectMode}
          />
        </div>

        {selectMode ? (
          <span
            aria-hidden
            className={cn(
              "pointer-events-none absolute top-2 left-2 z-20 flex size-6 items-center justify-center rounded-lg border shadow-sm transition-colors",
              selected
                ? "border-accent bg-accent text-accent-ink"
                : "border-white/80 bg-black/45 text-transparent backdrop-blur-xs",
            )}
          >
            <Check className="size-3.5 stroke-[2.6]" />
          </span>
        ) : (
          <>
            {unread > 0 ? (
              <span
                title={`有 ${unread} 章未读更新`}
                className="pointer-events-none absolute -top-1.5 -right-1.5 z-20 rounded-full bg-accent px-1.5 py-0.5 text-[10px] font-bold leading-none text-accent-ink shadow-md ring-2 ring-bg tabular-nums"
              >
                +{unread > 99 ? "99" : unread}
              </span>
            ) : local ? (
              <span className="pointer-events-none absolute top-2 left-2 z-20 rounded-md bg-black/60 px-1.5 py-0.5 text-[9px] font-medium tracking-wider text-white/95 backdrop-blur-xs">
                本地
              </span>
            ) : null}

            <div className="absolute right-1.5 bottom-1.5 z-20 opacity-90 transition-opacity sm:opacity-0 sm:group-hover:opacity-100 sm:focus-within:opacity-100">
              <BookActionPopover
                book={book}
                groups={groups}
                onInspect={onInspect}
                onAssignGroup={onAssignGroup}
                onQuickCache={onQuickCache}
                onRemove={onRemove}
                trigger={
                  <button
                    type="button"
                    aria-label={`《${book.name}》的操作`}
                    className="flex size-7.5 cursor-pointer items-center justify-center rounded-lg bg-bg/90 text-ink shadow-sm ring-1 ring-black/5 backdrop-blur-sm transition-colors hover:bg-elevated dark:ring-white/10"
                  >
                    <Ellipsis className="size-4" />
                  </button>
                }
              />
            </div>
          </>
        )}
      </div>

      {/* Title / Author / Progress */}
      <button
        type="button"
        onClick={() => (selectMode ? onToggleSelect() : onRead())}
        className="mt-2.5 cursor-pointer text-center outline-none"
      >
        <p
          title={book.name}
          className="truncate px-0.5 text-[13.5px] font-medium text-ink transition-colors group-hover:text-accent"
        >
          <Highlight text={book.name} query={query} />
        </p>
        {book.author?.trim() ? (
          <p title={book.author} className="mt-0.5 truncate px-0.5 text-[11.5px] text-ink-muted">
            <Highlight text={book.author.trim()} query={query} />
          </p>
        ) : null}
        {progressText ? (
          <p
            title={progressText}
            className="mt-1 flex items-center justify-center gap-1 truncate px-0.5 text-[11px] text-ink-muted tabular-nums"
          >
            <span className="truncate">{progressText}</span>
            {started && book.totalChapterNum > 0 ? (
              <span className={cn("shrink-0 font-medium", finished ? "text-success" : "text-accent/90")}>
                {finished ? "已追平" : `${pct}%`}
              </span>
            ) : null}
          </p>
        ) : null}
      </button>

      {book.totalChapterNum > 0 ? (
        <div
          aria-hidden
          className="mt-1.5 h-1 overflow-hidden rounded-full bg-subtle"
        >
          <div
            className={cn(
              "h-full rounded-full transition-[width] duration-300",
              finished ? "bg-success" : "bg-accent",
            )}
            style={{ width: `${started ? Math.max(3, pct) : 0}%` }}
          />
        </div>
      ) : null}
    </div>
  );
}

function ShelfListCard({
  book,
  groups,
  query,
  selectMode,
  selected,
  onToggleSelect,
  onRead,
  onInspect,
  onAssignGroup,
  onQuickCache,
  onRemove,
}: CardProps) {
  const unread = getUnreadCount(book);
  const pct = getProgressPct(book);
  const progressText = getProgressLabel(book);
  const chapterTitle = cleanChapterTitle(book.durChapterTitle);
  const intro = cleanIntro(book.intro);
  const started = hasStarted(book);
  const finished = isFinished(book);
  const local = isLocalBook(book);
  const lastReadLabel = formatRelativeTime(book.durChapterTime);
  const groupNames = groups.filter((g) => isBookInGroup(book, g.id)).map((g) => g.name);

  return (
    <div
      onClick={() => (selectMode ? onToggleSelect() : onRead())}
      onContextMenu={(e) => {
        e.preventDefault();
        onInspect("overview");
      }}
      className={cn(
        "group relative flex cursor-pointer select-none items-center gap-3.5 py-3.5 transition-colors active:bg-subtle/55 md:gap-4 md:rounded-2xl md:border md:bg-elevated md:p-4 md:transition-all md:hover:border-accent/45 md:hover:shadow-[var(--shadow-sm)]",
        selectMode && selected
          ? "rounded-xl bg-accent-soft/20 px-2 md:border-accent md:px-4 md:ring-1 md:ring-accent/40"
          : "md:border-line",
      )}
    >
      {selectMode ? (
        <span
          aria-hidden
          className={cn(
            "flex size-5.5 shrink-0 items-center justify-center rounded-md border transition-colors",
            selected ? "border-accent bg-accent text-accent-ink" : "border-line-strong bg-bg",
          )}
        >
          {selected ? <Check className="size-3.5 stroke-[2.5]" /> : null}
        </span>
      ) : null}

      <div className="relative w-14 shrink-0 sm:w-16">
        <BookCover
          src={getBookCover(book)}
          referer={book.origin}
          title={book.name}
          author={book.author}
          interactive={false}
        />
        {unread > 0 ? (
          <span className="pointer-events-none absolute -top-1.5 -right-1.5 z-10 rounded-full bg-accent px-1.5 py-0.5 text-[10px] font-bold leading-none text-accent-ink shadow-sm ring-2 ring-elevated tabular-nums">
            +{unread > 99 ? "99" : unread}
          </span>
        ) : null}
      </div>

      <div className="min-w-0 flex-1 space-y-1.5">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <p className="truncate font-serif text-base font-semibold text-ink transition-colors group-hover:text-accent">
            <Highlight text={book.name} query={query} />
          </p>
          {local ? (
            <span className="shrink-0 rounded-full border border-line bg-subtle px-1.5 py-0.5 text-[10px] font-medium leading-none text-ink-secondary">
              本地
            </span>
          ) : null}
          {groupNames.slice(0, 2).map((name) => (
            <span
              key={name}
              className="hidden shrink-0 rounded-full border border-line bg-subtle/70 px-2 py-0.5 text-[10px] leading-none text-ink-muted sm:inline-flex"
            >
              {name}
            </span>
          ))}
        </div>

        <p className="truncate text-xs text-ink-muted">
          {book.author?.trim() ? <Highlight text={book.author.trim()} query={query} /> : "佚名"}
          {book.originName ? (
            <>
              {" · "}
              <Highlight text={book.originName} query={query} />
            </>
          ) : null}
          {lastReadLabel ? ` · ${lastReadLabel}读过` : ""}
        </p>

        {book.totalChapterNum > 0 ? (
          <div className="flex items-center gap-2.5 pt-0.5">
            <div className="h-1.5 w-24 shrink-0 overflow-hidden rounded-full bg-subtle sm:w-32">
              <div
                className={cn(
                  "h-full rounded-full transition-[width] duration-300",
                  finished ? "bg-success" : "bg-accent",
                )}
                style={{ width: `${started ? Math.max(3, pct) : 0}%` }}
              />
            </div>
            <span className="truncate text-[11.5px] text-ink-secondary tabular-nums">
              {progressText}
              {started ? ` (${finished ? "已追平" : `${pct}%`})` : ""}
              {chapterTitle ? ` · ${chapterTitle}` : ""}
            </span>
          </div>
        ) : chapterTitle ? (
          <p className="truncate text-[11.5px] text-ink-secondary">读至：{chapterTitle}</p>
        ) : null}

        {intro ? (
          <p className="line-clamp-1 text-xs leading-relaxed text-ink-muted/90">{intro}</p>
        ) : null}
      </div>

      {!selectMode ? (
        <div onClick={(e) => e.stopPropagation()} className="flex shrink-0 items-center gap-1">
          <Button
            size="xs"
            variant="subtle"
            onClick={onRead}
            className="hidden sm:inline-flex"
          >
            {started ? "续读" : "开始阅读"}
          </Button>
          <BookActionPopover
            book={book}
            groups={groups}
            onInspect={onInspect}
            onAssignGroup={onAssignGroup}
            onQuickCache={onQuickCache}
            onRemove={onRemove}
            trigger={
              <IconButton size="sm" aria-label={`《${book.name}》的操作`}>
                <Ellipsis className="size-4" />
              </IconButton>
            }
          />
        </div>
      ) : null}
    </div>
  );
}
