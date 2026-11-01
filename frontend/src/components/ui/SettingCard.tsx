import type { ReactNode } from "react";

import { cn } from "./cn";

export interface SettingCardProps {
  /** 衬线卡片标题 */
  title: string;
  /** 标题下的小字说明, 可选 */
  desc?: string;
  /** 行内容, 通常是若干 SettingRow, 自带 divide-y 分隔 */
  children: ReactNode;
  className?: string;
}

/** 设置卡(移植自砚台原型): 衬线标题 + 描述 + divide-y 行内容. */
export function SettingCard({ title, desc, children, className }: SettingCardProps) {
  return (
    <section className={cn("rounded-xl border border-border bg-surface p-5", className)}>
      <h2 className="font-display text-lg font-semibold">{title}</h2>
      {desc ? <p className="mt-1 text-xs text-muted-foreground">{desc}</p> : null}
      <div className="mt-5 divide-y divide-border/70">{children}</div>
    </section>
  );
}

export interface SettingRowProps {
  label: string;
  /** 关联控件 id: 给了就渲染真 label(点标签可聚焦控件), 不给退化成纯文本 */
  htmlFor?: string;
  /** label 下方的小字说明, 可选 */
  value?: string;
  /** 右侧控件区(按钮/开关/胶囊), 小屏时换行到下方 */
  children?: ReactNode;
  className?: string;
}

/** 设置行: 左 label/value, 右控件; 与 SettingCard 的 divide-y 配合使用. */
export function SettingRow({ label, htmlFor, value, children, className }: SettingRowProps) {
  const Label = htmlFor ? "label" : "p";
  return (
    <div
      className={cn(
        "flex flex-col gap-3 py-4 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between",
        className,
      )}
    >
      <div className="min-w-0">
        <Label htmlFor={htmlFor} className="block text-sm font-medium">
          {label}
        </Label>
        {value ? <p className="mt-1 text-xs text-muted-foreground">{value}</p> : null}
      </div>
      {children ? <div className="flex shrink-0 items-center gap-2">{children}</div> : null}
    </div>
  );
}
