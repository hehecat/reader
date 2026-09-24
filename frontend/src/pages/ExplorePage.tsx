import { useQuery } from "@tanstack/react-query";
import { CircleAlert, Compass, Library, RotateCw } from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import { BookDetailDialog } from "@/components/book/BookDetailDialog";
import { ExploreBookGrid } from "@/components/explore/ExploreBookGrid";
import { ExploreMenuTabs } from "@/components/explore/ExploreMenuTabs";
import { ExplorePager } from "@/components/explore/ExplorePager";
import { ExploreSourceSelect, type ExploreSource } from "@/components/explore/ExploreSourceSelect";
import { Button, EmptyState, IconButton, PageIntro, cn } from "@/components/ui";
import { errorMessage } from "@/hooks/useBookshelf";
import { humanizeError } from "@/lib/errors";
import { exploreBook, isExplorable, parseExploreMenus } from "@/services/explore";
import { useExploreHealth } from "@/hooks/useExploreHealth";
import { getBookSources } from "@/services/sources";
import type { SearchBook } from "@/types/api";

/** 结果网格容器 id: 分类标签通过 aria-controls 指向它 */
const GRID_ID = "explore-book-grid";
/** 与搜索页/书源页共享 ["sources"] 缓存: 书源被改动后书海的书源下拉同样刷新 */
const SOURCES_QUERY_KEY = ["sources"];

/** 书海选择状态: 书源/分类/页码放在一个对象里, 换书源或分类时页码一并复位 */
interface ExploreSelection {
  sourceUrl: string;
  menuIndex: number;
  page: number;
}

const INITIAL_SELECTION: ExploreSelection = { sourceUrl: "", menuIndex: 0, page: 1 };

/**
 * 书海页: 选书源 → 选发现分类(exploreUrl 菜单) → 分页网格浏览 → 点卡片看详情/换源/加入书架.
 */
export default function ExplorePage() {
  const navigate = useNavigate();
  const sourcesQuery = useQuery({
    queryKey: SOURCES_QUERY_KEY,
    queryFn: getBookSources,
    staleTime: 5 * 60_000,
  });

  /** 书海自动筛选: 已判定"无内容"的源(6h 内)不显示, 避免点进去一片空白 */
  const health = useExploreHealth();
  const allExploreSources = React.useMemo<ExploreSource[]>(() => {
    const list: ExploreSource[] = [];
    for (const source of sourcesQuery.data ?? []) {
      if (isExplorable(source)) {
        list.push({ source, menus: parseExploreMenus(source) });
      }
    }
    return list;
  }, [sourcesQuery.data]);
  const exploreSources = React.useMemo(
    () => allExploreSources.filter((item) => !health.isHidden(item.source.bookSourceUrl)),
    [allExploreSources, health],
  );
  const hiddenByHealth = allExploreSources.length - exploreSources.length;

  /** 批量重新检测: 并发 3 只探每个源的首个发现分类, 结果写回本地(空源自动隐藏); 再次点击可中止 */
  const [probing, setProbing] = React.useState<{ done: number; total: number } | null>(null);
  const probingRef = React.useRef(false);
  const recheckAll = () => {
    if (probingRef.current) {
      probingRef.current = false; // 中止
      setProbing(null);
      return;
    }
    const list = allExploreSources.filter((item) => item.menus.length > 0);
    if (list.length === 0) {
      return;
    }
    probingRef.current = true;
    setProbing({ done: 0, total: list.length });
    let cursor = 0;
    let done = 0;
    const worker = async () => {
      while (probingRef.current) {
        const i = cursor;
        cursor += 1;
        if (i >= list.length) {
          return;
        }
        const item = list[i];
        if (item === undefined) {
          return;
        }
        let ok = false;
        try {
          const firstMenu = item.menus[0];
          const hits = await exploreBook({
            bookSourceUrl: item.source.bookSourceUrl,
            ruleFindUrl: firstMenu?.url ?? "",
            page: 1,
          });
          ok = hits.length > 0;
        } catch {
          ok = false;
        }
        health.mark(item.source.bookSourceUrl, ok);
        done += 1;
        setProbing({ done, total: list.length });
      }
    };
    void Promise.all(Array.from({ length: Math.min(3, list.length) }, worker)).then(() => {
      probingRef.current = false;
      setProbing(null);
    });
  };

  const [selection, setSelection] = React.useState(INITIAL_SELECTION);
  const [selected, setSelected] = React.useState<SearchBook | null>(null);
  const [detailOpen, setDetailOpen] = React.useState(false);
  /** 分类/工具行锚点: 换书源/分类/翻页后滚回这里, 页首介绍不参与回滚 */
  const toolbarRef = React.useRef<HTMLDivElement>(null);
  const skipScrollReset = React.useRef(true);

  // 选中的书源可能因停用/删除而消失, 回落到第一个可探索书源
  const active =
    exploreSources.find((item) => item.source.bookSourceUrl === selection.sourceUrl) ??
    exploreSources[0];
  const menus = active?.menus ?? [];
  const menuIndex = selection.menuIndex < menus.length ? selection.menuIndex : 0;
  const sourceUrl = active?.source.bookSourceUrl ?? "";
  const ruleFindUrl = menus[menuIndex]?.url ?? "";
  const canExplore = sourceUrl.length > 0 && ruleFindUrl.length > 0;

  const exploreQuery = useQuery({
    queryKey: ["explore", sourceUrl, ruleFindUrl, selection.page],
    queryFn: ({ signal }) =>
      exploreBook(
        { bookSourceUrl: sourceUrl, ruleFindUrl, page: selection.page },
        { signal },
      ),
    enabled: canExplore,
  });

  // 探测结果落库: 本次分类有内容 → 标记可用; 空 → 标记无内容(下次进书海隐藏)
  const exploreFetchedOk = exploreQuery.isSuccess;
  const exploreEmpty = exploreQuery.isSuccess && (exploreQuery.data?.length ?? 0) === 0;
  React.useEffect(() => {
    if (!canExplore || !exploreFetchedOk) {
      return;
    }
    health.mark(sourceUrl, !exploreEmpty);
  }, [canExplore, exploreFetchedOk, exploreEmpty, sourceUrl, health]);

  // 书源自身的发现页可能重复列出同一本书, 按 bookUrl|origin 去重
  const books = React.useMemo(() => {
    const seen = new Set<string>();
    const list: SearchBook[] = [];
    for (const item of exploreQuery.data ?? []) {
      const key = `${item.bookUrl}|${item.origin}`;
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      list.push(item);
    }
    return list;
  }, [exploreQuery.data]);

  // 换书源/分类/翻页后回到工具行顶部, 否则新结果要从旧滚动位置开始找; 首帧页面本就在顶, 不动
  React.useEffect(() => {
    if (skipScrollReset.current) {
      skipScrollReset.current = false;
      return;
    }
    toolbarRef.current?.scrollIntoView({ block: "start" });
  }, [selection]);

  const sourcesError = sourcesQuery.error;
  const noSources =
    !sourcesQuery.isLoading && sourcesError === null && exploreSources.length === 0;

  const openDetail = (target: SearchBook) => {
    setSelected(target);
    setDetailOpen(true);
  };

  const prevPage = () => {
    setSelection((prev) => ({ ...prev, page: Math.max(1, prev.page - 1) }));
  };

  const nextPage = () => {
    setSelection((prev) => ({ ...prev, page: prev.page + 1 }));
  };

  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="EXPLORE THE LIBRARY"
        title="去书海里, 遇见下一本想读的书."
        desc="从书源的发现分类里浏览与筛选."
        action={
          <>
            <ExploreSourceSelect
              sources={exploreSources}
              value={sourceUrl}
              onChange={(nextUrl) =>
                setSelection({ sourceUrl: nextUrl, menuIndex: 0, page: 1 })
              }
              className="w-52 min-w-0 sm:w-72"
            />
            <IconButton
              variant="ghost"
              aria-label="刷新书源列表"
              tooltip="刷新书源"
              disabled={sourcesQuery.isFetching}
              onClick={() => void sourcesQuery.refetch()}
            >
              <RotateCw aria-hidden className={cn(sourcesQuery.isFetching && "ui-spin")} />
            </IconButton>
          </>
        }
      />

      <div ref={toolbarRef} className="mb-6 flex flex-col gap-2">
        <div className="flex items-end justify-between gap-2">
          <Button
            size="sm"
            variant="ghost"
            className="text-xs"
            disabled={sourcesQuery.isLoading || allExploreSources.length === 0}
            onClick={recheckAll}
          >
            {probing !== null
              ? `检测中 ${probing.done}/${probing.total}(点击中止)`
              : "重新检测书海源"}
          </Button>
          <p className="self-end text-xs text-muted-foreground">
            {sourcesQuery.isLoading
              ? "读取书源中"
              : `${exploreSources.length} 个可探索书源${
                  hiddenByHealth > 0 ? ` · 已隐藏 ${hiddenByHealth} 个无内容源` : ""
                }${menus.length > 0 ? ` · ${menus.length} 个发现分类` : ""}`}
          </p>
        </div>
        <ExploreMenuTabs
          menus={menus}
          value={menuIndex}
          onChange={(index) =>
            setSelection((prev) => ({ ...prev, menuIndex: index, page: 1 }))
          }
          controlsId={GRID_ID}
          className="w-full"
        />
      </div>

      <div className="min-w-0 flex-1">
        {sourcesError !== null ? (
          <EmptyState
            icon={<CircleAlert aria-hidden />}
            title="书源加载失败"
            description={humanizeError(errorMessage(sourcesError, "网络异常或登录态已失效"))}
            action={
              <Button size="sm" variant="secondary" onClick={() => void sourcesQuery.refetch()}>
                <RotateCw aria-hidden />
                重试
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : noSources ? (
          <EmptyState
            icon={<Compass aria-hidden />}
            title="没有可探索的书源"
            description="当前书源没有提供发现分类, 到书源页编辑规则或换一本书源"
            action={
              <Button size="sm" onClick={() => navigate("/sources")}>
                <Library aria-hidden />
                去书源页
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : !canExplore ? (
          <EmptyState
            icon={<Compass aria-hidden />}
            title="选择书源开始探索"
            description="从上方选一个书源, 再挑一个发现分类"
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : exploreQuery.isError ? (
          <EmptyState
            icon={<CircleAlert aria-hidden />}
            title="探索失败"
            description={humanizeError(errorMessage(exploreQuery.error, "发现分类加载失败"))}
            action={
              <Button
                size="sm"
                variant="secondary"
                onClick={() => void exploreQuery.refetch()}
              >
                <RotateCw aria-hidden />
                重试
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : exploreQuery.isLoading ? (
          <ExploreBookGrid books={books} loading onSelect={openDetail} id={GRID_ID} />
        ) : books.length === 0 ? (
          <EmptyState
            compact
            icon={<Compass aria-hidden />}
            title={
              selection.page > 1 ? `第 ${selection.page} 页没有书籍` : "这个分类下没有书籍"
            }
            description={
              selection.page > 1
                ? "该分类已经翻到底了"
                : "源站分类页没返回书目: 可能源站改版、有反爬或分类已失效. 换书源/分类试试, 或到书源页更新发现规则"
            }
            action={
              selection.page > 1 ? (
                <Button size="sm" variant="secondary" onClick={prevPage}>
                  返回上一页
                </Button>
              ) : undefined
            }
            className="rounded-xl border border-dashed border-border py-12"
          />
        ) : (
          <ExploreBookGrid books={books} onSelect={openDetail} id={GRID_ID} />
        )}
      </div>

      {canExplore && (exploreQuery.isLoading || books.length > 0 || selection.page > 1) ? (
        <ExplorePager
          className="mt-6"
          page={selection.page}
          count={books.length}
          loading={exploreQuery.isFetching}
          onPrev={prevPage}
          onNext={nextPage}
        />
      ) : null}

      <BookDetailDialog book={selected} open={detailOpen} onOpenChange={setDetailOpen} />
    </div>
  );
}
