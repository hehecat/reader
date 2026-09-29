import { ArrowLeftRight, RotateCw, Search } from "lucide-react";
import * as React from "react";
import { useSearchParams } from "react-router-dom";
import { Virtuoso } from "react-virtuoso";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Button,
  Drawer,
  DrawerBody,
  DrawerContent,
  DrawerDescription,
  DrawerFooter,
  DrawerHeader,
  DrawerTitle,
  Input,
  Switch,
  cn,
  toast,
} from "@/components/ui";
import { errorMessage } from "@/hooks/useBookshelf";
import { searchBookContent, type BookContentHit } from "@/services/book";
import { humanizeError } from "@/lib/errors";
import {
  attachCacheBookStream,
  cancelCacheBook,
  getBookCacheInfo,
  startCacheBookStream,
  type CacheProgress,
  type CacheStreamHandlers,
} from "@/services/cache";
import type { BookChapter } from "@/types/api";

/** 命中片段高亮: 按关键字切分, 关键字部分包 <mark>(不用 innerHTML, 无注入面) */
function highlight(text: string, query: string): React.ReactNode {
  const q = query.trim();
  if (q === "") {
    return text;
  }
  const escaped = q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const parts = text.split(new RegExp(`(${escaped})`, "gi"));
  return parts.map((part, index) =>
    part.toLowerCase() === q.toLowerCase() ? (
      <mark key={index} className="rounded bg-accent/20 px-0.5 text-accent">
        {part}
      </mark>
    ) : (
      <React.Fragment key={index}>{part}</React.Fragment>
    ),
  );
}

/**
 * 书内搜索输入框(模块级 memo): 抽屉内还有缓存进度流、目录刷新等高频 state,
 * 父级重渲染时若连带重建输入框, PC 上会看到文字/光标闪动 —— 这里 props 不变即不重渲染。
 */
const TocSearchField = React.memo(function TocSearchField({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  return (
    <div className="relative mt-3">
      <Search
        aria-hidden
        className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
      />
      <Input
        aria-label="书内搜索"
        placeholder="搜索章节名 / 正文"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="pl-8"
      />
    </div>
  );
});

export interface TocDrawerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 抽屉头部展示的书名与作者 */
  bookName: string;
  bookAuthor: string;
  chapters: BookChapter[];
  /** 当前章节索引 (高亮) */
  currentIndex: number;
  reversed: boolean;
  onToggleReversed: () => void;
  /** 点击章节: 切章并关闭抽屉 */
  onSelect: (index: number) => void;
  /** 书架记录的章数: 大于目录长度时提示目录可能不完整(翻页残缺/缓存陈旧) */
  totalChapterNum?: number;
  /** 重取目录(跳过服务端缓存); 传了才显示「刷新目录」按钮 */
  onRefreshToc?: () => void;
  /** 目录重取进行中 */
  refreshingToc?: boolean;
  /** 打开换源面板(本地书不传): 本章出问题时从目录直接换源 */
  onSwitchSource?: () => void;
}

/** 目录抽屉: 左侧滑出 (书名 + 作者抬头), 虚拟列表承载数千章节, 当前章节高亮, 支持倒序 */
export function TocDrawer({
  open,
  onOpenChange,
  bookName,
  bookAuthor,
  chapters,
  currentIndex,
  reversed,
  onToggleReversed,
  onSelect,
  onSwitchSource,
  totalChapterNum,
  onRefreshToc,
  refreshingToc = false,
}: TocDrawerProps) {
  const ordered = React.useMemo(
    () => (reversed ? [...chapters].reverse() : chapters),
    [chapters, reversed],
  );

  // 打开时把当前章节顶到列表可视区顶部 (抽屉每次打开重新挂载 Virtuoso)
  const initialTop = React.useMemo(() => {
    const position = ordered.findIndex((chapter) => chapter.index === currentIndex);
    return position >= 0 ? position : 0;
  }, [ordered, currentIndex]);

  // 整书缓存预热: 书链接取自阅读页路由参数; 本地书正文已在服务端, 没有缓存语义
  const [searchParams] = useSearchParams();
  const bookUrl = searchParams.get("url") ?? "";
  const isLocal = bookUrl === "" || bookUrl.startsWith("local://");

  const queryClient = useQueryClient();
  const cacheInfoQuery = useQuery({
    queryKey: ["bookCacheInfo", bookUrl],
    queryFn: () => getBookCacheInfo(bookUrl),
    enabled: open && !isLocal,
  });

  /** 运行中的缓存任务进度; null = 空闲 (无任务或已终态) */
  /** 书内搜索: 空串 = 章节列表; 非空 = 搜索结果视图(章节名 + 正文命中) */
  const [query, setQuery] = React.useState("");
  const [hits, setHits] = React.useState<BookContentHit[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [searchError, setSearchError] = React.useState<string | null>(null);

  /** 书架章数多于目录长度 → 打开抽屉时自动重取一次目录(只一次, 避免反复请求) */
  const autoRefreshed = React.useRef(false);
  React.useEffect(() => {
    if (open) {
      autoRefreshed.current = false;
    }
  }, [open]);
  React.useEffect(() => {
    if (
      open &&
      !autoRefreshed.current &&
      onRefreshToc !== undefined &&
      totalChapterNum !== undefined &&
      totalChapterNum > chapters.length &&
      query.trim() === ""
    ) {
      autoRefreshed.current = true;
      onRefreshToc();
    }
  }, [open, totalChapterNum, chapters.length, onRefreshToc, query]);

  /** 章节名命中(本地即时过滤) */
  const titleHits = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === "") {
      return [];
    }
    return chapters.filter((c) => c.title.toLowerCase().includes(q)).slice(0, 200);
  }, [chapters, query]);

  /** 正文命中(后端 LIKE 已入库正文: 本地书全文 / 书源书已缓存章节), 防抖 400ms */
  React.useEffect(() => {
    const q = query.trim();
    if (q === "" || bookUrl === "") {
      setHits([]);
      setSearchError(null);
      setSearching(false);
      return;
    }
    const timer = window.setTimeout(() => {
      setSearching(true);
      void searchBookContent(bookUrl, q)
        .then((found) => {
          setHits(found);
          setSearchError(null);
        })
        .catch((error) => {
          setHits([]);
          setSearchError(errorMessage(error, "搜索失败"));
        })
        .finally(() => setSearching(false));
    }, 400);
    return () => window.clearTimeout(timer);
  }, [query, bookUrl]);

  /**
   * 正文命中的目录序号: 书源书缓存以「章节 URL 的 hash」为键(md5),
   * 不是目录序号, 因此按标题回查当前目录定位; 定位不到则提示目录可能已变化。
   */
  const resolveIndex = React.useCallback(
    (hit: BookContentHit): number | null => {
      const target = hit.title.trim();
      if (target === "") {
        return null;
      }
      const found = chapters.find((c) => c.title.trim() === target);
      return found ? found.index : null;
    },
    [chapters],
  );

  const [progress, setProgress] = React.useState<CacheProgress | null>(null);
  const [cancelling, setCancelling] = React.useState(false);
  const streamRef = React.useRef<(() => void) | null>(null);

  /**
   * 建流 (启动或附着共用): explicit=用户点了「缓存本书」, 终态一律 toast;
   * 附着探测首帧即终态说明任务早已结束 (服务端任务表保留终态条目), 静默刷新统计即可.
   */
  const connect = React.useCallback(
    (explicit: boolean) => {
      streamRef.current?.();
      let sawRunning = false;
      const handlers: CacheStreamHandlers = {
        onProgress: (frame) => {
          if (frame.finished || frame.cancelled) {
            return; // 终态帧统一走 onDone
          }
          sawRunning = true;
          setProgress(frame);
        },
        onDone: (final) => {
          streamRef.current = null;
          setProgress(null);
          setCancelling(false);
          void queryClient.invalidateQueries({ queryKey: ["bookCacheInfo", bookUrl] });
          if (!sawRunning && !explicit) {
            return;
          }
          if (final.cancelled) {
            toast.info("已取消缓存");
            return;
          }
          if (final.error !== undefined && final.error !== "") {
            toast.error(humanizeError(final.error, "缓存失败"));
            return;
          }
          toast.success(`已缓存 ${final.cached} 章`);
        },
        onError: (err) => {
          streamRef.current = null;
          setProgress(null);
          setCancelling(false);
          if (explicit) {
            toast.error(humanizeError(errorMessage(err, "缓存失败")));
          }
          // 附着探测的错误 (「缓存任务不存在」= 无运行中任务) 静默: 目录不受打扰
        },
      };
      streamRef.current = explicit
        ? startCacheBookStream(bookUrl, handlers)
        : attachCacheBookStream(bookUrl, handlers);
    },
    [bookUrl, queryClient],
  );

  // 打开抽屉时探测是否已有运行中任务 (如章末预热静默启动的), 有则附着同步进度
  React.useEffect(() => {
    if (!open || isLocal) {
      return;
    }
    connect(false);
    return () => {
      streamRef.current?.();
      streamRef.current = null;
    };
  }, [open, isLocal, connect]);

  const handleStart = React.useCallback(() => connect(true), [connect]);

  const handleCancel = React.useCallback(() => {
    setCancelling(true);
    cancelCacheBook(bookUrl).catch((error: unknown) => {
      setCancelling(false);
      toast.error(humanizeError(errorMessage(error, "取消缓存失败")));
    });
    // cancelled 终态帧会沿进度流到达, 由 onDone 统一收流并提示
  }, [bookUrl]);

  // 进度行文案: 目录解析完成前 total=0 用章节列表数兜底; title 就位前是 url, 用书名兜底
  const runningTotal = progress !== null && progress.total > 0 ? progress.total : chapters.length;
  const runningTitle =
    progress !== null && progress.title !== "" && progress.title !== bookUrl
      ? progress.title
      : bookName;
  const idleLabel = cacheInfoQuery.isError
    ? "缓存统计加载失败"
    : cacheInfoQuery.data !== undefined
      ? `已缓存 ${cacheInfoQuery.data.cacheChapterCount} 章`
      : "缓存统计加载中…";

  return (
    <Drawer side="left" open={open} onOpenChange={onOpenChange}>
      <DrawerContent className="w-80 max-w-[85vw]">
        <DrawerHeader className="relative z-10 bg-surface p-5">
          <DrawerTitle className="font-display text-lg leading-tight font-semibold">
            {bookName}
          </DrawerTitle>
          <DrawerDescription className="mt-0.5 text-xs">
            {bookAuthor} · 共 {chapters.length} 章 · 当前第{" "}
            {Math.min(currentIndex + 1, chapters.length)} 章
          </DrawerDescription>
          {isLocal ? null : (
            <div className="mt-3 flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground tabular-nums">
                {progress !== null
                  ? `缓存中 ${progress.cached}/${runningTotal} · ${runningTitle}`
                  : idleLabel}
              </span>
              {progress !== null ? (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={cancelling}
                  onClick={handleCancel}
                  aria-label="取消缓存"
                >
                  {cancelling ? "取消中…" : "取消"}
                </Button>
              ) : (
                <Button size="sm" onClick={handleStart} aria-label="缓存本书">
                  缓存本书
                </Button>
              )}
            </div>
          )}
          <TocSearchField value={query} onChange={setQuery} />
        </DrawerHeader>
        {totalChapterNum !== undefined && totalChapterNum > chapters.length ? (
          <p className="mx-3 mb-1 rounded-lg bg-accent/10 px-2.5 py-1.5 text-xs text-accent">
            目录只有 {chapters.length} 章, 但书架记录 {totalChapterNum} 章 —— 可能抓取不完整或缓存陈旧,
            可点下方「刷新目录」重取。
          </p>
        ) : null}
        <DrawerBody className="overflow-hidden p-2">
          {query.trim() === "" ? (
            <Virtuoso
            data={ordered}
            initialTopMostItemIndex={initialTop}
            style={{ height: "100%" }}
            itemContent={(_position, chapter) => {
              const active = chapter.index === currentIndex;
              return (
                <button
                  type="button"
                  onClick={() => onSelect(chapter.index)}
                  aria-current={active ? "true" : undefined}
                  className={cn(
                    "flex w-full cursor-pointer items-center rounded-lg px-3 py-3 text-left text-sm transition-colors",
                    active
                      ? "bg-accent/10 font-medium text-accent"
                      : "text-foreground/80 hover:bg-surface-muted",
                  )}
                >
                  <span className="min-w-0 flex-1 truncate">{chapter.title}</span>
                </button>
              );
            }}
          />
          ) : (
            <div className="h-full overflow-y-auto pb-2">
              {searching ? (
                <p className="px-2 py-2 text-xs text-muted-foreground">搜索正文中…</p>
              ) : null}
              {titleHits.length === 0 && hits.length === 0 && !searching ? (
                <p className="px-2 py-6 text-center text-sm text-muted-foreground">
                  {searchError ??
                    (isLocal
                      ? "没有找到匹配内容"
                      : "未在已缓存章节中找到 · 书源书仅搜索已缓存正文, 可先用「缓存本书」")}
                </p>
              ) : null}
              {titleHits.length > 0 ? (
                <section className="mb-1">
                  <h4 className="px-3 py-1.5 text-xs text-muted-foreground">
                    章节名 · {titleHits.length}
                  </h4>
                  {titleHits.map((chapter) => (
                    <button
                      key={`title-${chapter.index}`}
                      type="button"
                      onClick={() => onSelect(chapter.index)}
                      className="flex w-full cursor-pointer items-center rounded-lg px-3 py-2 text-left text-sm hover:bg-surface-muted"
                    >
                      <span className="min-w-0 flex-1 truncate">{highlight(chapter.title, query)}</span>
                    </button>
                  ))}
                </section>
              ) : null}
              {hits.length > 0 ? (
                <section>
                  <h4 className="px-3 py-1.5 text-xs text-muted-foreground">
                    正文 · {hits.length} 章命中
                  </h4>
                  {hits.map((hit) => (
                    <button
                      key={`content-${hit.chapterIndex}`}
                      type="button"
                      onClick={() => {
                        const index = resolveIndex(hit);
                        if (index === null) {
                          toast.info("未在目录中定位到该章节(目录可能已更新)");
                          return;
                        }
                        onSelect(index);
                      }}
                      className="flex w-full cursor-pointer flex-col gap-0.5 rounded-lg px-3 py-2 text-left hover:bg-surface-muted"
                    >
                      <span className="text-xs text-muted-foreground">
                        {hit.title || "（无标题）"}
                      </span>
                      <span className="line-clamp-2 text-sm text-foreground/80">
                        {highlight(hit.snippet, query)}
                      </span>
                    </button>
                  ))}
                </section>
              ) : null}
            </div>
          )}
        </DrawerBody>
        <DrawerFooter className="justify-between">
          {onRefreshToc !== undefined ? (
            <Button
              size="sm"
              variant="ghost"
              loading={refreshingToc}
              onClick={onRefreshToc}
              aria-label="刷新目录"
            >
              <RotateCw aria-hidden />
              刷新目录
            </Button>
          ) : null}
          {onSwitchSource !== undefined && !isLocal ? (
            <Button size="sm" variant="ghost" onClick={onSwitchSource} aria-label="换源">
              <ArrowLeftRight aria-hidden />
              换源
            </Button>
          ) : (
            <span className="text-sm text-muted-foreground">倒序目录</span>
          )}
          <Switch checked={reversed} onCheckedChange={onToggleReversed} aria-label="倒序目录" />
        </DrawerFooter>
      </DrawerContent>
    </Drawer>
  );
}
