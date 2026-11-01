import * as React from "react";

import { ChevronDown } from "lucide-react";

import { cn } from "@/components/ui";

export interface DegradedNotice {
  /** 稳定 key */
  id: string;
  /** 短标签, 折叠态里用来汇总(目录 / 书籍信息 / 正文) */
  label: string;
  /** 完整说明(展开态) */
  message: string;
  /** 空闲态动作文案, 例如「刷新」「重试」 */
  actionLabel: string;
  /** 进行中动作文案, 例如「刷新中…」 */
  busyLabel: string;
  busy: boolean;
  onAction: () => void;
}

/**
 * 阅读器降级提示条: 目录/书籍信息/正文最多会同时降级三条, 原来三条各 24px 叠在正文上方,
 * 文案还互相重复。合成一条: 单条直接显示, 多条先给汇总, 展开才逐条列原因和动作。
 * 无论多少条都只占一行, 正文可读性不受影响。
 */
export function DegradedNotices({ items }: { items: DegradedNotice[] }) {
  const [expanded, setExpanded] = React.useState(false);

  if (items.length === 0) {
    return null;
  }

  const rowClass =
    "flex items-center justify-center gap-2 border-b border-border bg-surface px-3 py-1 text-xs text-muted-foreground";
  const actionClass =
    "shrink-0 cursor-pointer font-medium text-accent disabled:opacity-50";

  if (items.length === 1) {
    const item = items[0]!;
    return (
      <div className={rowClass}>
        <span className="truncate">{item.message}</span>
        <button
          type="button"
          className={actionClass}
          disabled={item.busy}
          onClick={item.onAction}
        >
          {item.busy ? item.busyLabel : item.actionLabel}
        </button>
      </div>
    );
  }

  return (
    <div className="border-b border-border bg-surface">
      <div className={rowClass}>
        <span className="truncate">
          {items.length} 项待恢复: {items.map((item) => item.label).join(" · ")}
        </span>
        <button
          type="button"
          aria-expanded={expanded}
          className={cn(
            "flex shrink-0 cursor-pointer items-center gap-0.5 font-medium text-accent outline-none focus-visible:ring-2 focus-visible:ring-ring",
          )}
          onClick={() => setExpanded((prev) => !prev)}
        >
          {expanded ? "收起" : "详情"}
          <ChevronDown
            aria-hidden
            className={cn("size-3 transition-transform", expanded && "rotate-180")}
          />
        </button>
      </div>
      {expanded ? (
        <ul className="divide-y divide-border/60 border-t border-border/60">
          {items.map((item) => (
            <li key={item.id} className={cn(rowClass, "border-b-0")}>
              <span className="truncate">{item.message}</span>
              <button
                type="button"
                className={actionClass}
                disabled={item.busy}
                onClick={item.onAction}
              >
                {item.busy ? item.busyLabel : item.actionLabel}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
