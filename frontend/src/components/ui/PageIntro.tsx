import type { ReactNode } from "react";

import { cn } from "./cn";

export interface PageIntroProps {
  /** 大写英文小标, 如 "SOURCE CENTER" */
  eyebrow: string;
  /** 页面名, 渲染为全页唯一 h1 */
  title: string;
  /** 补充描述, 可选 */
  desc?: string;
  /** 右侧操作区(按钮等), lg 起与标题底部对齐 */
  action?: ReactNode;
  className?: string;
}

/** 页首标题区: eyebrow + 页面名(h1) + 描述 + 右侧动作, 底部分隔线. 页面名压到 24/28px, 给内容让位. */
export function PageIntro({ eyebrow, title, desc, action, className }: PageIntroProps) {
  return (
    <section
      className={cn(
        "mb-4 flex flex-col justify-between gap-4 border-b border-border/70 pb-3 sm:mb-6 sm:pb-5 lg:flex-row lg:items-end",
        className,
      )}
    >
      <div className="min-w-0">
        <p className="mb-2 hidden text-xs font-medium tracking-[0.2em] text-accent uppercase sm:block">
          {eyebrow}
        </p>
        <h1 className="font-display text-2xl font-semibold tracking-wide text-balance sm:text-[28px]">
          {title}
        </h1>
        {desc ? (
          <p className="mt-3 hidden max-w-2xl text-sm leading-relaxed text-muted-foreground sm:block">
            {desc}
          </p>
        ) : null}
      </div>
      {action ? <div className="flex shrink-0 items-center gap-2">{action}</div> : null}
    </section>
  );
}
