import { X } from "lucide-react";
import * as React from "react";

import { cn } from "./cn";

export type InputSize = "sm" | "md";

export interface InputProps extends Omit<React.ComponentProps<"input">, "size" | "prefix"> {
  /** 高度档位, 默认 md(40px) */
  size?: InputSize;
  /** 输入框左侧图标(自行给图标设尺寸, 建议 size-4) */
  prefixIcon?: React.ReactNode;
  /** 输入框右侧自定义内容(在清除按钮之后) */
  suffix?: React.ReactNode;
  /** 有内容时显示清除按钮 */
  clearable?: boolean;
  /** 点击清除: 受控用法必须在这里把 value 置空 */
  onClear?: () => void;
  /** 校验失败态: 边框转 danger */
  invalid?: boolean;
  /** className 作用在外层容器上(宽高、间距等布局) */
  className?: string;
}

const sizeClass: Record<InputSize, string> = {
  sm: "h-8 gap-1.5 px-2.5 text-xs",
  md: "h-10 gap-2 px-3 text-sm",
};

/** 文本输入框: 支持前缀图标、清除按钮与失败态; 非受控时内部维护值, clearable 可直接清空. */
export function Input({
  size = "md",
  prefixIcon,
  suffix,
  clearable = false,
  onClear,
  invalid = false,
  className,
  value,
  defaultValue,
  onChange,
  disabled,
  readOnly,
  type = "text",
  ...props
}: InputProps) {
  const controlled = value !== undefined;
  const [inner, setInner] = React.useState(() => String(defaultValue ?? ""));
  const current = controlled ? String(value ?? "") : inner;
  const showClear = clearable && !disabled && !readOnly && current.length > 0;

  const handleChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    if (!controlled) setInner(event.target.value);
    onChange?.(event);
  };

  const handleClear = () => {
    if (!controlled) setInner("");
    onClear?.();
  };

  return (
    <div
      className={cn(
        // min-w-0: 作为 flex 子项时 min-width:auto 会取到内容 min-content 而拒绝收缩(w-full 也压不住), 窄屏会顶出兄弟元素
        "flex w-full min-w-0 items-center rounded-lg border bg-background text-foreground transition duration-150 ease-out focus-within:border-accent focus-within:ring-2 focus-within:ring-ring/70",
        invalid ? "border-danger" : "border-border",
        disabled && "cursor-not-allowed opacity-50",
        sizeClass[size],
        className,
      )}
    >
      {prefixIcon ? (
        <span className="flex shrink-0 items-center text-muted-foreground">{prefixIcon}</span>
      ) : null}
      <input
        type={type}
        value={current}
        disabled={disabled}
        readOnly={readOnly}
        aria-invalid={invalid || undefined}
        onChange={handleChange}
        className="h-full min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
        {...props}
      />
      {showClear ? (
        <button
          type="button"
          aria-label="清除输入"
          onClick={handleClear}
          className="relative -mr-1 inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground outline-none transition duration-150 ease-out before:absolute before:-inset-1 before:content-[''] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X className="size-3.5" />
        </button>
      ) : null}
      {suffix ? (
        <span className="flex shrink-0 items-center text-muted-foreground">{suffix}</span>
      ) : null}
    </div>
  );
}
