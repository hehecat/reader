import { CircleAlert, Grid2X2, List, Search as SearchIcon } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Virtuoso } from "react-virtuoso";

import { BookDetailDialog } from "@/components/book/BookDetailDialog";
import { SearchBar } from "@/components/search/SearchBar";
import { SearchResultCard } from "@/components/search/SearchResultCard";
import { Button, EmptyState, PageIntro, Skeleton, Spinner, cn } from "@/components/ui";
import { useBookshelf } from "@/hooks/useBookshelf";
import { useSearchHistory } from "@/hooks/useSearchHistory";
import { useSearchSSE } from "@/hooks/useSearchSSE";
import { humanizeError } from "@/lib/errors";
import { getJSON, setJSON } from "@/lib/storage";
import type { Book, SearchBook } from "@/types/api";

/** 书架判重键: 后端 saveBook 以 bookUrl 或「书名 + 作者」认书, 这里用同一口径 */
function shelfTitleKey(name: string, author: string): string {
  return `${name}\u0000${author}`;
}

/** 结果网格(砚台 LibraryGrid 风): 移动端单列行卡, sm 两列, lg 三列, xl 四列 */
const GRID_CLASS =
  "grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-x-6 sm:gap-y-8 lg:grid-cols-3 xl:grid-cols-4";

/** 搜索页布局偏好键 (与书架 LAYOUT_KEY 独立) */
/** 目录探针缓存键/TTL: 7 天内同书同源不重复探针 */
const SEARCH_LAYOUT_KEY = "reader.search.layout";
type SearchLayout = "grid" | "list";

interface LayoutToggleProps {
  active: boolean;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}

/** 与书架同形的布局切换钮: 活跃态 accent 描边 + 浅底 */
function LayoutToggle({ active, label, onClick, children }: LayoutToggleProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      className={cn(
        "flex size-9 cursor-pointer items-center justify-center rounded-md border outline-none transition-colors duration-150 ease-out focus-visible:ring-2 focus-visible:ring-accent/60",
        active
          ? "border-accent bg-accent/10 text-accent"
          : "border-border text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

/** 首批结果到达前的骨架卡数 */
const SKELETON_COUNT = 8;

/** 结果区滚动容器: 页面自身不滚动, 滚动发生在 AppShell 的 <main> 里 */
const SCROLL_PARENT_SELECTOR = "main";

/** 断点对应的网格列数 (与 GRID_CLASS 的 sm/lg/xl 一致): 虚拟化按"行"喂数据 */
const COLUMN_BREAKPOINTS = [
  { query: "(min-width: 1280px)", columns: 4 },
  { query: "(min-width: 1024px)", columns: 3 },
  { query: "(min-width: 640px)", columns: 2 },
] as const;

/** 结果区列数: 网格 1/2/3/4 列自适应, 列表恒 1 列 (移动端也是行卡) */
function useResultColumns(layout: SearchLayout): number {
  const [columns, setColumns] = useState(1);
  useEffect(() => {
    const media = COLUMN_BREAKPOINTS.map((item) => ({
      list: window.matchMedia(item.query),
      columns: item.columns,
    }));
    const sync = (): void => {
      if (layout === "list") {
        setColumns(1);
        return;
      }
      setColumns(media.find((item) => item.list.matches)?.columns ?? 1);
    };
    sync();
    for (const item of media) {
      item.list.addEventListener("change", sync);
    }
    return () => {
      for (const item of media) {
        item.list.removeEventListener("change", sync);
      }
    };
  }, [layout]);
  return columns;
}

interface ResultRowProps {
  row: readonly SearchBook[];
  layout: SearchLayout;
  keyword: string;
  shelfKeys: ReadonlySet<string>;
  onSelect: (book: SearchBook) => void;
  onAdded: (saved: Book) => void;
}

/** 虚拟列表的一行: 网格布局里并排 1~4 张卡, 列表布局里 1 张行卡 */
function ResultRow({ row, layout, keyword, shelfKeys, onSelect, onAdded }: ResultRowProps) {
  return (
    <div
      className={cn(
        layout === "grid" ? GRID_CLASS : "flex flex-col",
        // 行间距取代 grid 的 gap-y (虚拟化按行渲染, 行间空隙由行底内边距给)
        layout === "grid" ? "pb-3 sm:pb-8" : "pb-2",
      )}
    >
      {row.map((book) => (
        <SearchResultCard
          layout={layout}
          key={`${book.bookUrl}|${book.origin}`}
          book={book}
          keyword={keyword}
          inShelf={
            shelfKeys.has(book.bookUrl) || shelfKeys.has(shelfTitleKey(book.name, book.author))
          }
          onSelect={onSelect}
          onAdded={onAdded}
        />
      ))}
    </div>
  );
}

/** 搜索页: URL 参数 q 驱动 SSE 流式多源搜索, 结果聚合去重后按砚台网格渲染, 点卡片进详情. */
export default function SearchPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const q = searchParams.get("q")?.trim() ?? "";
  const [keyword, setKeyword] = useState(q);
  const {
    results,
    searching,
    progress,
    error,
    quiet,
    arrived,
    hasMore,
    start,
    stop,
    loadMore,
    hydrate,
    lastIndex,
  } = useSearchSSE();
  /** 会话快照键: 预览/切页返回后恢复原搜索列表 */
  const SEARCH_SNAPSHOT_KEY = "reader.search.snapshot.v1";
  const { history, add: addHistory, clear: clearHistory } = useSearchHistory();
  const { books } = useBookshelf();
  /** 手动停止过: 状态行显示「已停止」, 下一次搜索开始时复位 */
  const [stopped, setStopped] = useState(false);
  /** 已为哪个关键词发起过搜索 */
  const [startedKey, setStartedKey] = useState<string | null>(null);
  /** 本次会话刚加入书架的书: 后端读缓存约 5 秒, ["books"] 重拉可能还没反映 */
  const [pendingShelf, setPendingShelf] = useState<readonly Book[]>([]);
  /** 详情弹窗当前书籍: 点击结果卡打开(砚台 book-detail 结构, 含章节目录) */
  const [selected, setSelected] = useState<SearchBook | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  /** 网格/列表布局偏好 (与书架同形, 独立记忆) */
  const [layout, setLayout] = useState<SearchLayout>(() =>
    getJSON<string>(SEARCH_LAYOUT_KEY) === "list" ? "list" : "grid",
  );
  const changeLayout = (next: SearchLayout): void => {
    setLayout(next);
    setJSON(SEARCH_LAYOUT_KEY, next);
  };

  // q 变化(书架/导航跳转、刷新恢复)自动触发搜索; 有会话快照则先恢复列表不重搜.
  // StrictMode 首挂载会双跑: start 内部先断开旧连接幂等, hydrate 幂等可重复.
  useEffect(() => {
    if (q.length === 0) {
      return;
    }
    setKeyword(q);
    try {
      const snap = JSON.parse(sessionStorage.getItem(SEARCH_SNAPSHOT_KEY) ?? "null");
      if (
        snap !== null &&
        snap.key === q &&
        Array.isArray(snap.items) &&
        snap.items.length > 0
      ) {
        hydrate(q, snap.items, snap.lastIndex ?? -1);
        return;
      }
    } catch {
      /* 快照损坏则正常搜索 */
    }
    addHistory(q);
    start(q);
  }, [q, start, hydrate, addHistory]);

  // 卸载前保存会话快照: 去阅读器预览/切页返回后列表还在 (不重新起搜)
  const snapRef = useRef<{ key: string; items: SearchBook[]; lastIndex: number }>({
    key: "",
    items: [],
    lastIndex: -1,
  });
  snapRef.current = { key: keyword, items: results, lastIndex };
  useEffect(
    () => () => {
      const snap = snapRef.current;
      if (snap.key !== "" && snap.items.length > 0) {
        sessionStorage.setItem(SEARCH_SNAPSHOT_KEY, JSON.stringify(snap));
      }
    },
    [],
  );

  // 已为当前 q 发起过搜索: q 变更时清零, 搜索一开始就记上(见下方 pendingStart)
  useEffect(() => {
    setStartedKey(null);
  }, [q]);
  useEffect(() => {
    if (!searching) {
      return;
    }
    setStartedKey(q);
    setStopped(false);
  }, [searching, q]);

  // 原始错误串进 console 供调试, UI 只展示 humanizeError 后的人话
  useEffect(() => {
    if (error !== null) {
      console.warn("[reader] search:", error);
    }
  }, [error]);

  const handleSearch = (raw: string) => {
    const key = raw.trim();
    if (key.length === 0) {
      return;
    }
    setKeyword(key);
    if (key === q) {
      // 同关键词 URL 不变, effect 不会重跑, 直接重搜(start 会断开进行中的连接)
      addHistory(key);
      start(key);
    } else {
      setSearchParams({ q: key });
    }
  };

  const handleStop = () => {
    setStopped(true);
    stop();
  };

  const openDetail = (book: SearchBook) => {
    setSelected(book);
    setDetailOpen(true);
  };

  const visibleResults = results;

  /** 虚拟化: 结果按「行」喂给 Virtuoso, DOM 里只保留可视区附近的卡片 */
  const columns = useResultColumns(layout);
  const resultRows = useMemo(() => {
    const rows: SearchBook[][] = [];
    for (let i = 0; i < visibleResults.length; i += columns) {
      rows.push(visibleResults.slice(i, i + columns));
    }
    return rows;
  }, [visibleResults, columns]);

  // 页面自身不滚动, 滚动发生在 AppShell 的 <main> 里: 把同一个滚动容器交给 Virtuoso,
  // 吸顶搜索栏与整页滚动行为保持不变, 只是结果区改成按可视行渲染.
  const pageRef = useRef<HTMLDivElement | null>(null);
  const [scrollParent, setScrollParent] = useState<HTMLElement | null>(null);
  useLayoutEffect(() => {
    setScrollParent(pageRef.current?.closest<HTMLElement>(SCROLL_PARENT_SELECTOR) ?? null);
  }, []);

  const handleAdded = useCallback((saved: Book) => {
    setPendingShelf((prev) => [...prev, saved]);
  }, []);

  const initial = q.length === 0 && !searching && results.length === 0 && error === null;
  const showResults = visibleResults.length > 0;
  // q 就绪但搜索还没发起(发起在绘制后的 effect 里): 这一帧继续按加载中渲染,
  // 否则会闪一下「没有找到相关书籍」, 骨架也断一拍; 快照恢复的结果不算 pending.
  const pendingStart = q.length > 0 && !searching && startedKey !== q && results.length === 0;
  // 完成/停止后既无结果也无错误 → 搜索无结果
  const noResult = !initial && !pendingStart && !searching && !showResults && error === null;
  const errorText = error === null ? null : humanizeError(error);

  // 书架成员: bookUrl 或「书名 + 作者」命中(后端 saveBook 同口径); pendingShelf 兜住
  // 刚加入书架、服务端读缓存还没反映的窗口
  const shelfKeys = useMemo(() => {
    const keys = new Set<string>();
    for (const list of [books, pendingShelf]) {
      for (const item of list) {
        keys.add(item.bookUrl);
        keys.add(shelfTitleKey(item.name, item.author));
      }
    }
    return keys;
  }, [books, pendingShelf]);

  return (
    <div ref={pageRef} className="flex min-h-full flex-col bg-background">
      <div className="mx-auto w-full max-w-3xl px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8 lg:max-w-5xl xl:max-w-6xl">
        <PageIntro
          eyebrow="SEARCH"
          title="找下一本要读的书."
          desc="多书源并发搜索, 结果实时聚合."
          action={
            <div className="flex items-center gap-1.5">
              <LayoutToggle
                active={layout === "grid"}
                label="网格布局"
                onClick={() => changeLayout("grid")}
              >
                <Grid2X2 aria-hidden className="size-4" />
              </LayoutToggle>
              <LayoutToggle
                active={layout === "list"}
                label="列表布局"
                onClick={() => changeLayout("list")}
              >
                <List aria-hidden className="size-4" />
              </LayoutToggle>
            </div>
          }
        />

        {/* 搜索栏吸附在内容区顶部: 长结果列表里随时可以换词重搜 */}
        <div className="sticky top-0 z-30 -mx-4 mb-6 border-b border-border/70 bg-background/90 px-4 py-3 backdrop-blur sm:-mx-6 sm:px-6 md:-mx-10 md:px-10">
          <SearchBar
            value={keyword}
            onChange={setKeyword}
            onSearch={handleSearch}
            onStop={handleStop}
            searching={searching}
            history={history}
            historyDefaultOpen={initial}
            onSelectHistory={handleSearch}
            onClearHistory={clearHistory}
          />
        </div>

        {initial ? (
          <EmptyState
            icon={<SearchIcon />}
            title="搜索书籍"
            description="多书源并发搜索, 结果实时流入; 同名同作者的书自动聚合为多源"
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : error !== null && !showResults && !pendingStart ? (
          <EmptyState
            icon={<CircleAlert />}
            title="搜索失败"
            description={errorText}
            action={
              <Button onClick={() => start(keyword)} disabled={keyword.trim().length === 0}>
                重试
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : (
          <>
            {searching ? (
              // 搜索(含静默态)的状态行: 高度固定(min-h-9), 从「搜索中」降级到轻提示时不推挤列表
              <div
                className="flex min-h-9 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 py-2.5 text-xs text-muted-foreground"
                role="status"
                aria-live="polite"
              >
                {quiet ? (
                  // 静默态(settling): 结果已到一批但收尾源还在返回 —— 收起转圈, 只留轻提示.
                  // 列表保持完全可交互(滚动/打开书籍都不受影响), 有新增会立刻切回正常态.
                  <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-muted-foreground/50" />
                ) : (
                  <Spinner size="sm" label="搜索中" />
                )}
                <span>
                  {quiet
                    ? `已收到 ${visibleResults.length} 条，剩余源仍在返回`
                    : `已找到 ${visibleResults.length} 本`}
                </span>
                {arrived > 0 ? (
                  // 静默被新结果打断: 亮出新到条数并高亮, 不让结果静默插入
                  <span className="rounded-full bg-accent/10 px-2 py-0.5 font-medium text-accent transition-colors duration-200 ease-out">
                    新到 {arrived} 条
                  </span>
                ) : null}
                {progress !== null ? (
                  <span>
                    ·{" "}
                    {progress.total > progress.done
                      ? `书源 ${progress.done}/${progress.total}`
                      : `已搜索 ${progress.done} 个书源`}
                  </span>
                ) : null}
              </div>
            ) : null}

            {error !== null && showResults ? (
              <div
                className="mb-3 flex shrink-0 items-center gap-2 rounded-xl border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger"
                role="alert"
              >
                <CircleAlert className="size-4 shrink-0" />
                <span className="min-w-0 flex-1">{errorText}</span>
              </div>
            ) : null}

            {showResults ? (
              // 结果虚拟化: 200 条结果不再一次性塞进 DOM (基线单次长任务 2.2s 全在挂卡片上)
              <div className="shrink-0">
                {scrollParent !== null ? (
                  <Virtuoso
                    data={resultRows}
                    customScrollParent={scrollParent}
                    // 卡片高度随简介行数/断点变化, 交给 Virtuoso 实测; 上下各多挂一屏, 滚动不露白
                    increaseViewportBy={600}
                    computeItemKey={(_index, row) =>
                      row.map((book) => `${book.bookUrl}|${book.origin}`).join(",")
                    }
                    itemContent={(_index, row) => (
                      <ResultRow
                        row={row}
                        layout={layout}
                        keyword={keyword}
                        shelfKeys={shelfKeys}
                        onSelect={openDetail}
                        onAdded={handleAdded}
                      />
                    )}
                  />
                ) : null}
              </div>
            ) : searching || pendingStart ? (
              // 骨架与结果同网格: 移动端是行卡形状, sm 起是封面卡形状
              <div
                aria-hidden
                className={cn(layout === "grid" ? GRID_CLASS : "flex flex-col gap-2", "shrink-0")}
              >
                {Array.from({ length: SKELETON_COUNT }, (_, index) => (
                  <div key={index} className="flex items-center gap-3 sm:block">
                    <Skeleton shape="rect" className="aspect-3/4 w-20 shrink-0 sm:w-full" />
                    <div className="flex min-w-0 flex-1 flex-col gap-2 sm:mt-3">
                      <Skeleton className="w-3/5 sm:mx-auto" />
                      <Skeleton className="w-2/5 sm:mx-auto" />
                    </div>
                  </div>
                ))}
              </div>
            ) : null}

            {noResult ? (
              <EmptyState
                compact
                icon={<SearchIcon />}
                title="没有找到相关书籍"
                description="换个关键词试试, 或到「书源」页确认书源已启用"
                className="rounded-xl border border-dashed border-border py-12"
              />
            ) : null}

            {!searching && !pendingStart && (showResults || noResult || error !== null) ? (
              <div className="mt-3 flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-border py-3">
                <span className="text-xs text-muted-foreground">
                  {error !== null
                    ? `搜索中断, 已找到 ${visibleResults.length} 本`
                    : stopped
                      ? `已停止 · 共找到 ${visibleResults.length} 本`
                      : `共找到 ${visibleResults.length} 本`}
                </span>
                <div className="flex gap-2">
                  {error !== null ? (
                    <Button size="sm" variant="secondary" onClick={loadMore}>
                      重试
                    </Button>
                  ) : hasMore ? (
                    <Button size="sm" variant="secondary" onClick={loadMore}>
                      加载更多
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => start(keyword)}
                    disabled={keyword.trim().length === 0}
                  >
                    重新搜索
                  </Button>
                </div>
              </div>
            ) : null}
          </>
        )}
      </div>

      <BookDetailDialog
        book={selected}
        open={detailOpen}
        onOpenChange={setDetailOpen}
        onAdded={(saved) => setPendingShelf((prev) => [...prev, saved])}
      />
    </div>
  );
}
