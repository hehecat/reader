import * as React from "react";

import { cn } from "./cn";

export type SkeletonShape = "text" | "rect" | "circle";

export interface SkeletonProps extends React.ComponentProps<"div"> {
  /** 形状预设: text=一行文字, rect=矩形(需自行给高度), circle=圆形头像位 */
  shape?: SkeletonShape;
}

export interface SkeletonListProps extends React.ComponentProps<"div"> {
  /** 行数, 默认 4 */
  count?: number;
}

const shapeClass: Record<SkeletonShape, string> = {
  text: "h-3 w-full rounded-md",
  rect: "w-full rounded-lg",
  circle: "size-10 shrink-0 rounded-full",
};

/** 骨架屏基元: 脉冲占位块, 具体尺寸用 className 覆盖. */
export function Skeleton({ shape = "text", className, ...props }: SkeletonProps) {
  return (
    <div
      aria-hidden
      className={cn("motion-safe:animate-pulse bg-surface-muted", shapeClass[shape], className)}
      {...props}
    />
  );
}

/** 书架卡片骨架: 3:4 封面 + 两行文字. */
export function SkeletonCard({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div className={cn("flex flex-col gap-2", className)} {...props}>
      <Skeleton shape="rect" className="aspect-3/4 w-full" />
      <Skeleton className="w-4/5" />
      <Skeleton className="w-3/5" />
    </div>
  );
}

/** 列表骨架: count 行, 每行方形缩略图 + 两行文字. */
export function SkeletonList({ count = 4, className, ...props }: SkeletonListProps) {
  return (
    <div aria-hidden className={cn("flex flex-col gap-4", className)} {...props}>
      {Array.from({ length: count }, (_, index) => (
        <div key={index} className="flex items-center gap-3">
          <Skeleton shape="rect" className="size-12 shrink-0" />
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            <Skeleton className="w-3/5" />
            <Skeleton className="w-2/5" />
          </div>
        </div>
      ))}
    </div>
  );
}
