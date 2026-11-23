import { AlertCircle, X } from "lucide-react";
import * as React from "react";
import { cn } from "@/lib/cn";

export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {
  label?: string;
  hint?: string;
  error?: string;
  leftIcon?: React.ReactNode;
  /** 输入框尾部插槽（如密码可见性切换、单位、快捷键角标） */
  rightSlot?: React.ReactNode;
  clearable?: boolean;
  onClear?: () => void;
  size?: "sm" | "md" | "lg";
}

const FIELD_SHELL =
  "group relative flex min-w-0 items-center rounded-xl border bg-elevated shadow-[inset_0_1px_2px_rgba(0,0,0,0.03)] transition-[border-color,box-shadow,background-color] duration-150";

export const Input = React.forwardRef<HTMLInputElement, InputProps>(function Input(
  {
    label,
    hint,
    error,
    leftIcon,
    rightSlot,
    clearable,
    onClear,
    size = "md",
    className,
    id,
    value,
    disabled,
    ...props
  },
  ref,
) {
  const autoId = React.useId();
  const inputId = id ?? autoId;
  const messageId = `${inputId}-msg`;
  const hasValue =
    typeof value === "string"
      ? value.length > 0
      : Array.isArray(value)
        ? value.length > 0
        : typeof value === "number";
  const message = error || hint;

  return (
    <div className={cn("flex min-w-0 flex-col gap-1.5", className)}>
      {label ? (
        <label
          htmlFor={inputId}
          className={cn(
            "text-xs font-medium transition-colors",
            error ? "text-danger" : "text-ink-secondary",
          )}
        >
          {label}
        </label>
      ) : null}
      <div
        className={cn(
          FIELD_SHELL,
          error
            ? "border-danger/70 focus-within:border-danger focus-within:ring-[3px] focus-within:ring-danger/15"
            : "border-line-strong/85 hover:border-ink-muted/45 focus-within:border-accent/65 focus-within:ring-[3px] focus-within:ring-accent/15 focus-within:hover:border-accent/65",
          disabled && "pointer-events-none bg-subtle opacity-60",
          size === "sm" ? "h-8 px-2.5 text-xs" : size === "lg" ? "h-11 px-3.5 text-sm" : "h-9.5 px-3 text-[13px]",
        )}
      >
        {leftIcon ? (
          <span
            className={cn(
              "mr-2 flex shrink-0 items-center transition-colors",
              error ? "text-danger/80" : "text-ink-muted group-focus-within:text-accent",
            )}
          >
            {leftIcon}
          </span>
        ) : null}
        <input
          ref={ref}
          id={inputId}
          value={value}
          disabled={disabled}
          aria-invalid={error ? true : undefined}
          aria-describedby={message ? messageId : undefined}
          className="h-full min-w-0 flex-1 bg-transparent text-ink outline-none placeholder:text-ink-muted/70 disabled:cursor-not-allowed"
          {...props}
        />
        {clearable && hasValue ? (
          <button
            type="button"
            onClick={onClear}
            aria-label="清空输入"
            className="relative ml-1.5 flex size-5 shrink-0 cursor-pointer items-center justify-center rounded-full bg-ink-muted/12 text-ink-muted transition-colors before:absolute before:-inset-2 before:content-[''] hover:bg-ink-muted/25 hover:text-ink"
          >
            <X className="size-3" strokeWidth={2.5} />
          </button>
        ) : null}
        {rightSlot ? <span className="ml-1.5 flex shrink-0 items-center">{rightSlot}</span> : null}
      </div>
      {error ? (
        <p id={messageId} role="alert" className="flex items-center gap-1 text-[11px] leading-snug text-danger">
          <AlertCircle className="size-3 shrink-0" aria-hidden />
          {error}
        </p>
      ) : hint ? (
        <p id={messageId} className="text-[11px] leading-snug text-ink-muted">
          {hint}
        </p>
      ) : null}
    </div>
  );
});

export interface TextareaProps extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  label?: string;
  hint?: string;
  mono?: boolean;
  error?: string;
}

export const Textarea = React.forwardRef<HTMLTextAreaElement, TextareaProps>(function Textarea(
  { label, hint, mono = false, error, className, id, ...props },
  ref,
) {
  const autoId = React.useId();
  const fieldId = id ?? autoId;
  const messageId = `${fieldId}-msg`;
  const message = error || hint;
  return (
    <div className={cn("flex min-w-0 flex-col gap-1.5", className)}>
      {label ? (
        <label
          htmlFor={fieldId}
          className={cn("text-xs font-medium", error ? "text-danger" : "text-ink-secondary")}
        >
          {label}
        </label>
      ) : null}
      <textarea
        ref={ref}
        id={fieldId}
        aria-invalid={error ? true : undefined}
        aria-describedby={message ? messageId : undefined}
        className={cn(
          "min-h-[84px] w-full rounded-xl border bg-elevated px-3 py-2.5 text-[13px] leading-relaxed text-ink shadow-[inset_0_1px_2px_rgba(0,0,0,0.03)] outline-none transition-[border-color,box-shadow] duration-150 placeholder:text-ink-muted/70 disabled:cursor-not-allowed disabled:bg-subtle disabled:opacity-60",
          error
            ? "border-danger/70 focus:border-danger focus:ring-[3px] focus:ring-danger/15"
            : "border-line-strong/85 hover:border-ink-muted/45 focus:border-accent/65 focus:ring-[3px] focus:ring-accent/15",
          mono && "font-mono text-xs leading-relaxed",
        )}
        {...props}
      />
      {error ? (
        <p id={messageId} role="alert" className="text-[11px] leading-snug text-danger">
          {error}
        </p>
      ) : hint ? (
        <p id={messageId} className="text-[11px] leading-snug text-ink-muted">
          {hint}
        </p>
      ) : null}
    </div>
  );
});
