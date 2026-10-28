import * as React from "react";

import { ReaderLoading } from "@/components/reader/ContentView";
import { EmptyState, cn } from "@/components/ui";

/** 慢响应解释文案的默认延迟: 8 秒内算正常等待, 超过才解释「为什么这么久」 */
export const SLOW_LOADING_HINT_MS = 8000;

/** 延迟标记: delay 后置 true (卸载/改参数时清定时器) */
function useDelayedFlag(delay: number): boolean {
  const [hit, setHit] = React.useState(delay <= 0);
  React.useEffect(() => {
    if (delay <= 0) {
      return;
    }
    const timer = window.setTimeout(() => setHit(true), delay);
    return () => window.clearTimeout(timer);
  }, [delay]);
  return hit;
}

export interface ReaderLoadingStatusProps {
  /** 转圈下方的主文案 */
  label?: string;
  /** 超过该毫秒数补一行慢响应说明 (≤0 表示立刻显示) */
  slowAfterMs?: number;
}

/**
 * 可解释的加载态: 转圈 + 文案, 超过 slowAfterMs 再补一行「源站可能在过 Cloudflare 挑战」.
 * 慢源 (CF 拦截 / 后端 30s 目录预算) 下用户能看到"在等什么、还要等多久",
 * 而不是盯着一个不知道要不要继续等的转圈.
 */
export function ReaderLoadingStatus({
  label = "正在加载章节…",
  slowAfterMs = SLOW_LOADING_HINT_MS,
}: ReaderLoadingStatusProps) {
  const slow = useDelayedFlag(slowAfterMs);
  return (
    <div className="flex flex-col items-center gap-2">
      <ReaderLoading label={label} />
      {slow ? (
        <p
          role="status"
          className="max-w-72 text-center text-xs leading-5 text-balance text-muted-foreground/80"
        >
          源站响应慢, 可能正在被 Cloudflare 挑战, 最长等 30 秒
        </p>
      ) : null}
    </div>
  );
}

export interface ReaderStateCardProps {
  /** 图标 (lucide, 尺寸由 EmptyState 控制) */
  icon?: React.ReactNode;
  title: React.ReactNode;
  /** 后端原始错误文案: 原样展示, 不做二次翻译 */
  message?: React.ReactNode;
  /** 补充解释 (人话建议); 与 message 相同则调用方不该传 */
  hint?: React.ReactNode;
  /** 操作区 (重试 / 换源 / 返回书架 …) */
  actions?: React.ReactNode;
  className?: string;
}

/**
 * 系统级状态卡: 圆角卡包一份 EmptyState, 视觉与正文区空态一致 (圆角/border/暗色自动跟随),
 * 用于「书籍加载失败」「目录加载失败」「目录为空」这类整块状态 —— 必须给出下一步操作.
 */
export function ReaderStateCard({
  icon,
  title,
  message,
  hint,
  actions,
  className,
}: ReaderStateCardProps) {
  return (
    <div
      className={cn(
        "w-full max-w-lg rounded-2xl border border-border bg-surface px-6 py-8 shadow-sm",
        className,
      )}
    >
      <EmptyState className="py-0" icon={icon} title={title} description={message} action={actions} />
      {hint === undefined || hint === "" ? null : (
        <p className="mt-3 text-center text-xs leading-5 text-balance text-muted-foreground/80">
          {hint}
        </p>
      )}
    </div>
  );
}
