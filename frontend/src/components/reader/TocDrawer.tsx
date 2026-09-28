import { ArrowLeftRight } from "lucide-react";
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
  Switch,
  cn,
  toast,
} from "@/components/ui";
import { errorMessage } from "@/hooks/useBookshelf";
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
        <DrawerHeader className="p-5">
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
        </DrawerHeader>
        <DrawerBody className="overflow-hidden p-2">
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
        </DrawerBody>
        <DrawerFooter className="justify-between">
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
