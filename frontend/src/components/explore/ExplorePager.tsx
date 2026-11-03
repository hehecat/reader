import { ChevronLeft, ChevronRight } from "lucide-react";

import { Button, cn } from "@/components/ui";

export interface ExplorePagerProps {
  /** 当前页码, 从 1 开始 */
  page: number;
  /** 当前页书籍数: 为 0 说明该分类已到底, 下一页不可用 */
  count: number;
  loading: boolean;
  onPrev: () => void;
  onNext: () => void;
  className?: string;
}

/** 页码翻页器: 发现页无法预知总页数, 只能按「当前页有没有书」判断能否继续翻 */
export function ExplorePager({
  page,
  count,
  loading,
  onPrev,
  onNext,
  className,
}: ExplorePagerProps) {
  return (
    <div
      className={cn(
        "flex shrink-0 flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-surface px-3 py-2 text-xs text-muted-foreground",
        className,
      )}
    >
      <span className="tabular-nums">
        第 {page} 页 · {count} 本
      </span>
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          variant="secondary"
          className="h-9 px-3"
          onClick={onPrev}
          disabled={loading || page <= 1}
        >
          <ChevronLeft aria-hidden />
          上一页
        </Button>
        <Button
          size="sm"
          variant="secondary"
          className="h-9 px-3"
          onClick={onNext}
          disabled={loading || count === 0}
        >
          下一页
          <ChevronRight aria-hidden />
        </Button>
      </div>
    </div>
  );
}
