import * as React from "react";

import { cn } from "./cn";

export interface TextareaProps extends Omit<React.ComponentProps<"textarea">, "size"> {
  /** 校验失败态: 边框转 danger */
  invalid?: boolean;
  /** 随内容自动增高(field-sizing: content), 上限 16rem 后内部滚动 */
  autoSize?: boolean;
}

/** 多行文本输入: 可选自动增高, 失败态边框转 danger. */
export function Textarea({
  invalid = false,
  autoSize = false,
  className,
  rows,
  ...props
}: TextareaProps) {
  return (
    <textarea
      rows={autoSize ? undefined : (rows ?? 3)}
      aria-invalid={invalid || undefined}
      className={cn(
        "flex w-full resize-y rounded-lg border bg-background px-3 py-2 text-sm leading-6 text-foreground outline-none transition duration-150 ease-out placeholder:text-muted-foreground focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-ring/70 disabled:cursor-not-allowed disabled:opacity-50",
        invalid ? "border-danger" : "border-border",
        autoSize && "max-h-64 min-h-20 field-sizing-content overflow-y-auto",
        className,
      )}
      {...props}
    />
  );
}
