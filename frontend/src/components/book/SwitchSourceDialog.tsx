import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { toast } from "@/components/ui/Toast";
import { BOOKS_QUERY_KEY, errorMessage } from "@/hooks/useBookshelf";
import { humanizeError } from "@/lib/errors";
import { getBookContent, getChapterList } from "@/services/book";
import { getAvailableBookSource, setBookSource, type AvailableBookSource } from "@/services/explore";
import type { Book } from "@/types/api";

/** 单个候选源的本章比对结果 */
type ChapterProbe = { title: string; words: number } | { missing: string };

export interface SwitchSourceDialogProps {
  /** 目标书(需已在书架, 候选接口只查书架书); null 时保持关闭 */
  book: Book | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 是否已在书架(候选接口只查书架书); 缺省视为已入架(书架详情场景) */
  inShelf?: boolean;
  /** 未入架引导: 传了就显示「加入书架并继续换源」(阅读器场景) */
  onAddToShelf?: () => void;
  /** 加入书架请求进行中 */
  addingToShelf?: boolean;
  /** 当前章节索引(阅读器场景): 启用「比对各源本章」——看各源同一章是否缺章/字数是否异常 */
  chapterIndex?: number;
  /** 当前章节名(仅用于提示文案) */
  chapterTitle?: string;
  /** 换源成功(已拿到新书)后回调: 阅读器用它重载, 书架详情用它关闭自身 */
  onSwitched?: (candidate: AvailableBookSource) => void;
}

/**
 * 换源面板: 列出该书在其它书源的候选(后端按书名/作者从缓存匹配), 选中即切源并失效相关缓存.
 * 阅读器与书架详情共用; 传 chapterIndex 时可按当前章逐源比对(章节名 + 字数),
 * 用于"本章正文有问题(缺字/防盗/乱码)时选一个正常源".
 */
export function SwitchSourceDialog({
  book,
  open,
  onOpenChange,
  inShelf = true,
  onAddToShelf,
  addingToShelf = false,
  chapterIndex,
  chapterTitle,
  onSwitched,
}: SwitchSourceDialogProps) {
  const queryClient = useQueryClient();
  const bookUrl = book?.bookUrl ?? "";
  /** 缓存候选可能过期/为空: 允许用全部书源重新搜索一次(慢) */
  const [forceSearch, setForceSearch] = React.useState(false);
  /** 本章比对: bookUrl → 结果(章节名+字数 / 缺章 / 失败) */
  const [probes, setProbes] = React.useState<Record<string, ChapterProbe>>({});
  const [probeProgress, setProbeProgress] = React.useState<{ done: number; total: number } | null>(
    null,
  );
  const probeAbort = React.useRef(false);

  const candidates = useQuery({
    queryKey: ["switchCandidates", bookUrl, forceSearch],
    queryFn: () => getAvailableBookSource(bookUrl, forceSearch),
    // 未入架且无引导入口时不必发请求(后端只会报「书籍信息错误」)
    enabled: open && bookUrl !== "" && (inShelf || onAddToShelf !== undefined),
  });

  React.useEffect(() => {
    if (!open) {
      setForceSearch(false);
      setProbes({});
      setProbeProgress(null);
      probeAbort.current = false;
    }
  }, [open]);

  const applySwitch = useMutation({
    mutationFn: (candidate: AvailableBookSource) =>
      setBookSource({
        bookUrl,
        newBookUrl: candidate.bookUrl,
        bookSourceUrl: candidate.origin,
      }),
    onSuccess: (_updated, candidate) => {
      onOpenChange(false);
      toast.success(`已换到 ${candidate.originName || candidate.origin}`);
      // 旧 URL 下的详情/目录/正文缓存全部作废(新 URL 之前未查过, 无需预置)
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      for (const key of ["bookInfo", "chapters", "content", "switchCandidates"]) {
        void queryClient.invalidateQueries({ queryKey: [key, bookUrl] });
      }
      onSwitched?.(candidate);
    },
    onError: (error) => {
      toast.error(humanizeError(errorMessage(error, "换源失败")));
    },
  });

  /**
   * 逐源比对当前章: 拉候选源目录 → 取同序章节 → 拉该章正文统计字数.
   * 并发 3(两个请求/源, 控制对源站压力), 再次点击可中止.
   */
  const probeChapters = React.useCallback(async () => {
    const list = candidates.data ?? [];
    if (chapterIndex === undefined || list.length === 0) {
      return;
    }
    if (probeProgress !== null) {
      probeAbort.current = true; // 中止
      return;
    }
    probeAbort.current = false;
    setProbeProgress({ done: 0, total: list.length });
    const results: Record<string, ChapterProbe> = {};
    let cursor = 0;
    let done = 0;
    const worker = async () => {
      while (!probeAbort.current) {
        const i = cursor;
        cursor += 1;
        if (i >= list.length) {
          return;
        }
        const candidate = list[i];
        if (candidate === undefined) {
          return;
        }
        try {
          const toc = await getChapterList(candidate.bookUrl);
          const target = toc[chapterIndex];
          if (target === undefined) {
            results[candidate.bookUrl] = { missing: "缺少本章" };
          } else {
            const content = await getBookContent(target.url, chapterIndex, {
              bookSourceUrl: candidate.origin,
            });
            results[candidate.bookUrl] = {
              title: target.title,
              words: content.content.replace(/\s+/g, "").length,
            };
          }
        } catch {
          results[candidate.bookUrl] = { missing: "检测失败" };
        }
        done += 1;
        setProbes({ ...results });
        setProbeProgress({ done, total: list.length });
      }
    };
    await Promise.all(Array.from({ length: Math.min(3, list.length) }, worker));
    probeAbort.current = false;
    setProbeProgress(null);
  }, [candidates.data, chapterIndex, probeProgress]);

  if (book === null) {
    return <Dialog open={false} onOpenChange={onOpenChange} />;
  }

  const list = candidates.data ?? [];
  const canProbe = chapterIndex !== undefined && list.length > 0 && inShelf;
  const probed = Object.keys(probes).length;

  /** 右侧副标题: 本章比对结果优先, 未比对时给最新章节 */
  const rowDetail = (candidate: AvailableBookSource): React.ReactNode => {
    const probe = probes[candidate.bookUrl];
    if (probe === undefined) {
      return candidate.latestChapterTitle ?? "";
    }
    if ("missing" in probe) {
      return <span className="text-danger">{probe.missing}</span>;
    }
    return (
      <>
        {probe.title}
        <span className="ml-1 text-foreground/50">· {probe.words} 字</span>
      </>
    );
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="sm">
        <DialogHeader>
          <DialogTitle>切换书源</DialogTitle>
          <DialogDescription>
            {book.name}
            {book.originName ? ` · 当前 ${book.originName}` : ""}
            {chapterIndex !== undefined ? ` · 基准 第 ${chapterIndex + 1} 章` : ""}
            {chapterTitle ? ` ${chapterTitle}` : ""}
          </DialogDescription>
        </DialogHeader>
        {canProbe ? (
          <div className="flex items-center justify-between gap-3 px-4 pb-1 md:px-5">
            <span className="min-w-0 truncate text-xs text-muted-foreground">
              {probeProgress !== null
                ? `比对中 ${probeProgress.done}/${probeProgress.total}(点击中止)`
                : probed > 0
                  ? `已比对 ${probed} 个源 · 字数异常或显示"缺少本章"的源不建议换`
                  : "比对各源同一章: 缺章 / 字数异常(含乱码防盗)一眼可见 · 字数=去空白字符数"}
            </span>
            <Button
              size="sm"
              variant="secondary"
              className="shrink-0"
              onClick={() => void probeChapters()}
            >
              {probeProgress !== null ? "中止" : "比对本章"}
            </Button>
          </div>
        ) : null}
        <div className="max-h-80 overflow-y-auto px-4 md:px-5">
          {!inShelf && onAddToShelf ? (
            <div className="flex flex-col items-center gap-3 py-6 text-center text-sm text-muted-foreground">
              <p>未入架的书换源需要先加入书架 (用于保存进度与同步)</p>
              <Button size="sm" loading={addingToShelf} onClick={onAddToShelf}>
                加入书架并继续换源
              </Button>
            </div>
          ) : candidates.isLoading || candidates.isFetching ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              {forceSearch ? "正在用全部书源搜索(约 30-60 秒)…" : "正在获取候选书源…"}
            </p>
          ) : list.length === 0 ? (
            <div className="flex flex-col items-center gap-3 py-6 text-center text-sm text-muted-foreground">
              <p>{forceSearch ? "没有找到其它书源的同名书" : "暂无其他可用书源"}</p>
              {forceSearch ? null : (
                <Button size="sm" variant="secondary" onClick={() => setForceSearch(true)}>
                  用全部书源重新搜索
                </Button>
              )}
            </div>
          ) : (
            <>
              <div className="divide-y divide-border/70">
                {list.map((candidate) => (
                  <button
                    key={candidate.bookUrl}
                    type="button"
                    disabled={applySwitch.isPending}
                    onClick={() => applySwitch.mutate(candidate)}
                    className="flex w-full cursor-pointer items-center justify-between gap-3 py-3 text-left text-sm hover:text-accent disabled:opacity-50"
                  >
                    <span className="min-w-0 truncate">
                      {candidate.originName || candidate.origin}
                    </span>
                    <span className="min-w-0 max-w-[55%] shrink-0 truncate text-right text-xs text-muted-foreground">
                      {rowDetail(candidate)}
                    </span>
                  </button>
                ))}
              </div>
              {forceSearch ? null : (
                <div className="pb-3 pt-2 text-center">
                  <Button size="sm" variant="ghost" onClick={() => setForceSearch(true)}>
                    没找到? 用全部书源重新搜索
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
