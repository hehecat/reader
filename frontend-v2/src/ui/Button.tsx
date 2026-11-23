import * as React from "react";
import { cn } from "@/lib/cn";
import { motion, SPRING_PRESS, useReducedMotion } from "./motion";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "subtle" | "danger";
export type ButtonSize = "xs" | "sm" | "md" | "lg";

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  loading?: boolean;
}

const VARIANT_CLASSES: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-accent-ink shadow-[var(--shadow-primary)] hover:brightness-[1.07] active:brightness-95",
  secondary:
    "border border-line-strong bg-elevated text-ink shadow-[var(--shadow-sm)] hover:border-ink-muted/40 hover:bg-subtle active:bg-hover",
  subtle: "bg-subtle text-ink hover:bg-hover active:brightness-[0.98]",
  ghost: "text-ink-secondary hover:bg-subtle hover:text-ink active:bg-hover",
  danger: "bg-danger-soft text-danger hover:bg-danger hover:text-white active:brightness-95",
};

const SIZE_CLASSES: Record<ButtonSize, string> = {
  xs: "h-7 gap-1 rounded-full px-2.5 text-xs",
  sm: "h-8 gap-1.5 rounded-full px-3.5 text-xs",
  md: "h-9 gap-2 rounded-full px-4 text-[13px]",
  lg: "h-10 gap-2 rounded-full px-5 text-sm",
};

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", loading = false, disabled, className, children, type = "button", ...props },
  ref,
) {
  const reduced = useReducedMotion();
  return (
    <motion.button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      whileTap={reduced || disabled || loading ? undefined : { scale: 0.965 }}
      transition={SPRING_PRESS}
      className={cn(
        "relative inline-flex shrink-0 cursor-pointer items-center justify-center font-medium whitespace-nowrap outline-none select-none transition-[background-color,color,filter,border-color,box-shadow,opacity] duration-150 focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:ring-offset-2 focus-visible:ring-offset-bg disabled:pointer-events-none disabled:opacity-45 disabled:shadow-none",
        VARIANT_CLASSES[variant],
        SIZE_CLASSES[size],
        className,
      )}
      {...(props as Record<string, unknown>)}
    >
      {loading ? (
        <span
          aria-hidden
          className="size-3.5 shrink-0 animate-spin rounded-full border-[1.75px] border-current border-t-transparent opacity-85"
        />
      ) : null}
      {children}
    </motion.button>
  );
});

export interface IconButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  size?: "sm" | "md";
  active?: boolean;
  tone?: "default" | "danger" | "accent";
}

export const IconButton = React.forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { size = "md", active = false, tone = "default", disabled, className, type = "button", children, ...props },
  ref,
) {
  const reduced = useReducedMotion();
  return (
    <motion.button
      ref={ref}
      type={type}
      disabled={disabled}
      aria-pressed={active ? true : undefined}
      whileTap={reduced || disabled ? undefined : { scale: 0.9 }}
      transition={SPRING_PRESS}
      className={cn(
        // before: 伪元素把触控热区扩展到 ≥40px，视觉尺寸不变
        "relative inline-flex shrink-0 cursor-pointer items-center justify-center rounded-full outline-none select-none transition-colors duration-150 before:absolute before:-inset-1 before:content-[''] focus-visible:ring-2 focus-visible:ring-accent/55 disabled:pointer-events-none disabled:opacity-40",
        size === "sm" ? "size-8" : "size-9",
        active
          ? "bg-accent-soft text-accent"
          : tone === "danger"
            ? "text-ink-muted hover:bg-danger-soft hover:text-danger active:bg-danger-soft"
            : tone === "accent"
              ? "text-accent hover:bg-accent-soft active:bg-accent-soft"
              : "text-ink-secondary hover:bg-subtle hover:text-ink active:bg-hover",
        className,
      )}
      {...(props as Record<string, unknown>)}
    >
      {children}
    </motion.button>
  );
});
