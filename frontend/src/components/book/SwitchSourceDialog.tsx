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
import { getAvailableBookSource, setBookSource, type AvailableBookSource } from "@/services/explore";
import type { Book } from "@/types/api";

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
  /** 换源成功(已拿到新书)后回调: 阅读器用它重载, 书架详情用它关闭自身 */
  onSwitched?: (candidate: AvailableBookSource) => void;
}

/**
 * 换源面板: 列出该书在其它书源的候选(后端按书名/作者从缓存匹配),
 * 选中即切源并失效相关缓存. 阅读器与书架详情共用同一实现.
 */
export function SwitchSourceDialog({
  book,
  open,
  onOpenChange,
  inShelf = true,
  onAddToShelf,
  addingToShelf = false,
  onSwitched,
}: SwitchSourceDialogProps) {
  const queryClient = useQueryClient();
  const bookUrl = book?.bookUrl ?? "";
  /** 缓存候选可能过期/为空: 允许用全部书源重新搜索一次(慢) */
  const [forceSearch, setForceSearch] = React.useState(false);

  const candidates = useQuery({
    queryKey: ["switchCandidates", bookUrl, forceSearch],
    queryFn: () => getAvailableBookSource(bookUrl, forceSearch),
    // 未入架且无引导入口时不必发请求(后端只会报「书籍信息错误」)
    enabled: open && bookUrl !== "" && (inShelf || onAddToShelf !== undefined),
  });

  React.useEffect(() => {
    if (!open) {
      setForceSearch(false);
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

  if (book === null) {
    return <Dialog open={false} onOpenChange={onOpenChange} />;
  }

  const list = candidates.data ?? [];
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="sm">
        <DialogHeader>
          <DialogTitle>切换书源</DialogTitle>
          <DialogDescription>
            {book.name}
            {book.originName ? ` · 当前 ${book.originName}` : ""}
          </DialogDescription>
        </DialogHeader>
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
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => setForceSearch(true)}
                >
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
                    <span className="shrink-0 text-xs text-muted-foreground">
                      {candidate.latestChapterTitle ?? ""}
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
