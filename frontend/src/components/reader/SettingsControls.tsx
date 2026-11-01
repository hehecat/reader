import { Minus, Plus } from "lucide-react";
import * as React from "react";

import { cn } from "@/components/ui";
import { KAI_FONT_STACK, SANS_FONT_STACK, SERIF_FONT_STACK } from "@/hooks/useReaderTheme";
import type { FontFamilyMode } from "@/stores/settings-store";

/**
 * 阅读设置的控件原子 (取自砚台原型 reader-settings): Field 分组 / Segmented 分段按钮 / StepperRow 步进器.
 * 阅读器浮动面板与设置页阅读偏好卡共用, 两处口径一致.
 */

/** 设置项分组: 小字 label + 控件区; label 用 id 绑到控件组上, 读屏能念出「字号, 分组」 */
export function Field({ label, children }: { label: string; children: React.ReactNode }) {
  const labelId = React.useId();
  return (
    <div className="mb-4">
      <span id={labelId} className="mb-2 block text-xs font-medium text-muted-foreground">
        {label}
      </span>
      <div role="group" aria-labelledby={labelId}>
        {children}
      </div>
    </div>
  );
}

/** 步进按钮: 方形描边, 悬停转 accent */
function StepperButton({
  children,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      {...props}
      className={cn(
        "flex size-9 shrink-0 cursor-pointer items-center justify-center rounded-lg border border-border text-foreground transition-colors outline-none hover:border-accent/50 focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default disabled:opacity-40",
        props.className,
      )}
    >
      {children}
    </button>
  );
}

export interface StepperRowProps {
  value: number;
  min: number;
  max: number;
  step: number;
  /** 越界收敛交给调用方的 setter (store 里带 clamp) */
  onChange: (next: number) => void;
  /** 中间读数文案, 如 18px / 40 px/s */
  format: (value: number) => string;
  decreaseLabel: string;
  increaseLabel: string;
  className?: string;
}

/** − 读数 + 三段式步进器 */
export function StepperRow({
  value,
  min,
  max,
  step,
  onChange,
  format,
  decreaseLabel,
  increaseLabel,
  className,
}: StepperRowProps) {
  return (
    <div className={cn("flex items-center gap-3", className)}>
      <StepperButton
        onClick={() => onChange(value - step)}
        disabled={value <= min}
        aria-label={decreaseLabel}
      >
        <Minus className="size-4" aria-hidden />
      </StepperButton>
      <span className="flex-1 text-center text-sm tabular-nums">{format(value)}</span>
      <StepperButton
        onClick={() => onChange(value + step)}
        disabled={value >= max}
        aria-label={increaseLabel}
      >
        <Plus className="size-4" aria-hidden />
      </StepperButton>
    </div>
  );
}

export interface SegmentOption<T extends string | number> {
  value: T;
  label: string;
  /** 图标按钮形态 (主题组): 图标在上文字在下 */
  icon?: React.ComponentType<{ className?: string }>;
  /** 内联样式: 字体分段的 font-family 预览等 */
  style?: React.CSSProperties;
}

/** 分段按钮组: 描边胶囊, 选中转 accent (原型样式, 替代旧滑杆/胶囊) */
export function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  ariaLabel,
  columns = "grid-cols-3",
  className,
}: {
  value: T;
  options: SegmentOption<T>[];
  onChange: (next: T) => void;
  ariaLabel: string;
  /** 网格列数 class (grid-cols-2 / grid-cols-3 / grid-cols-4) */
  columns?: string;
  className?: string;
}) {
  // radiogroup 的契约: 只有选中项可 Tab 到, 组内用方向键移动并即时选中
  // (22 个独立停靠点会让键盘用户在每个设置项上都按一遍 Tab)
  const buttonsRef = React.useRef<(HTMLButtonElement | null)[]>([]);
  const selectedIndex = options.findIndex((option) => option.value === value);

  const focusAt = (index: number): void => {
    const next = options[index];
    if (next === undefined) {
      return;
    }
    onChange(next.value);
    buttonsRef.current[index]?.focus();
  };

  const handleKeyDown = (event: React.KeyboardEvent, index: number): void => {
    const last = options.length - 1;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(index === last ? 0 : index + 1);
    } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(index === 0 ? last : index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(last);
    }
  };

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className={cn("grid w-full gap-2", columns, className)}
    >
      {options.map((option, index) => {
        const selected = option.value === value;
        const Icon = option.icon;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            // 没有命中项时让第一项可 Tab, 否则整组键盘进不来
            tabIndex={selected || (selectedIndex === -1 && index === 0) ? 0 : -1}
            ref={(element) => {
              buttonsRef.current[index] = element;
            }}
            onKeyDown={(event) => handleKeyDown(event, index)}
            onClick={() => onChange(option.value)}
            style={option.style}
            className={cn(
              "flex cursor-pointer items-center justify-center rounded-lg border py-2 text-xs outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
              Icon !== undefined && "flex-col gap-1 py-2.5",
              selected
                ? "border-accent bg-accent/10 text-accent"
                : "border-border text-muted-foreground hover:border-accent/50 hover:text-foreground",
            )}
          >
            {Icon !== undefined ? <Icon className="size-4" aria-hidden /> : null}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

/** 字体分段选项: 每个按钮用自己代表的字体渲染 (所见即所得), 自定义项弹出 font-family 输入 */
export const FONT_FAMILY_OPTIONS: SegmentOption<FontFamilyMode>[] = [
  {
    value: "serif",
    label: "宋体",
    style: { fontFamily: SERIF_FONT_STACK, fontSize: "13px" },
  },
  {
    value: "sans",
    label: "黑体",
    style: { fontFamily: SANS_FONT_STACK, fontSize: "13px" },
  },
  {
    value: "kai",
    label: "楷体",
    style: { fontFamily: KAI_FONT_STACK, fontSize: "13px" },
  },
  { value: "custom", label: "自定义" },
];
