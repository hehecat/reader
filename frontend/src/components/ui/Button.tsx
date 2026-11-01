import * as React from "react";

import { cn } from "./cn";
import { Slot } from "./Slot";
import { Spinner, type SpinnerSize } from "./Spinner";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg" | "icon";

export interface ButtonProps extends React.ComponentProps<"button"> {
  /** 视觉档位, 默认 primary */
  variant?: ButtonVariant;
  /** 尺寸档位, 默认 md; icon 为正方形(只放图标) */
  size?: ButtonSize;
  /** 加载态: 前置 spinner 并禁用交互(asChild 时只透传 aria-busy/data-loading) */
  loading?: boolean;
  /** 用 Slot 渲染唯一子元素, 例如 `<Button asChild><a href="…">…</a></Button>` */
  asChild?: boolean;
}

const baseClass =
  "inline-flex shrink-0 cursor-pointer select-none items-center justify-center gap-2 rounded-lg font-medium whitespace-nowrap outline-none transition duration-150 ease-out focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background active:scale-97 disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0";

const variantClass: Record<ButtonVariant, string> = {
  primary: "bg-accent text-accent-foreground shadow-sm hover:bg-accent/90",
  secondary: "border border-border bg-surface text-foreground hover:bg-surface-muted",
  ghost: "text-foreground hover:bg-surface-muted",
  danger: "bg-danger text-background shadow-sm hover:bg-danger/90",
};

const sizeClass: Record<ButtonSize, string> = {
  sm: "h-8 px-3 text-xs",
  md: "h-9 px-4 text-sm",
  lg: "h-11 rounded-xl px-6 text-base",
  icon: "size-9",
};

const spinnerSize: Record<ButtonSize, SpinnerSize> = {
  sm: "sm",
  md: "sm",
  lg: "md",
  icon: "sm",
};

/** 通用按钮: variant/size 档位 + loading 态, 支持 asChild 委托渲染(链接、路由、菜单触发器). */
export function Button({
  variant = "primary",
  size = "md",
  loading = false,
  disabled,
  asChild = false,
  type,
  className,
  children,
  ...props
}: ButtonProps) {
  const classes = cn(baseClass, variantClass[variant], sizeClass[size], className);

  // asChild 时 Slot 只接受唯一子元素, 无法插入 spinner: 只把 loading 透传给调用方自行呈现.
  if (asChild) {
    return (
      <Slot
        aria-busy={loading || undefined}
        data-loading={loading || undefined}
        disabled={disabled}
        className={classes}
        {...props}
      >
        {children}
      </Slot>
    );
  }

  return (
    <button
      type={type ?? "button"}
      disabled={disabled === true || loading}
      aria-busy={loading || undefined}
      className={classes}
      {...props}
    >
      {loading ? <Spinner size={spinnerSize[size]} /> : null}
      {children}
    </button>
  );
}
