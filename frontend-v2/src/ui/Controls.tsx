import * as React from "react";
import { cn } from "@/lib/cn";
import { AnimatePresence, motion, SPRING_LIQUID, SPRING_PRESS, SPRING_SNAPPY, useReducedMotion } from "./motion";

/* ---------- 1. Segmented (Liquid layoutId spring pill bar) ---------- */

export interface SegmentedOption<T extends string> {
  value: T;
  label: React.ReactNode;
  icon?: React.ReactNode;
  badge?: React.ReactNode;
}

export function Segmented<T extends string>({
  options,
  value,
  onChange,
  layoutId,
  size = "md",
  className,
  fullWidth = false,
  ariaLabel,
}: {
  options: readonly SegmentedOption<T>[];
  value: T;
  onChange: (value: T) => void;
  layoutId: string;
  size?: "sm" | "md";
  className?: string;
  /** 等分撑满容器宽度 */
  fullWidth?: boolean;
  ariaLabel?: string;
}) {
  const reduced = useReducedMotion();
  const listRef = React.useRef<HTMLDivElement>(null);

  // WAI-ARIA Tabs：← → Home End 在选项间移动并即时选中
  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const keys = ["ArrowLeft", "ArrowRight", "Home", "End"];
    if (!keys.includes(e.key) || options.length === 0) return;
    e.preventDefault();
    const current = Math.max(
      0,
      options.findIndex((o) => o.value === value),
    );
    let next = current;
    if (e.key === "ArrowLeft") next = (current - 1 + options.length) % options.length;
    else if (e.key === "ArrowRight") next = (current + 1) % options.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = options.length - 1;
    const target = options[next];
    if (!target) return;
    onChange(target.value);
    listRef.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
  };

  return (
    <div
      ref={listRef}
      role="tablist"
      aria-label={ariaLabel}
      onKeyDown={handleKeyDown}
      className={cn(
        "relative isolate inline-flex items-center gap-0.5 rounded-full border border-line bg-subtle/85 p-0.5 shadow-[inset_0_1px_2px_rgba(0,0,0,0.04)]",
        fullWidth && "flex w-full",
        className,
      )}
    >
      {options.map((opt) => {
        const active = opt.value === value;
        return (
          <motion.button
            key={opt.value}
            type="button"
            role="tab"
            aria-selected={active}
            tabIndex={active ? 0 : -1}
            onClick={() => onChange(opt.value)}
            whileTap={reduced ? undefined : { scale: 0.96 }}
            transition={SPRING_PRESS}
            className={cn(
              "relative z-10 inline-flex cursor-pointer items-center justify-center gap-1.5 rounded-full font-medium whitespace-nowrap outline-none transition-colors duration-200 select-none focus-visible:ring-2 focus-visible:ring-accent/55",
              size === "sm" ? "h-[26px] px-2.5 text-xs" : "h-8 px-3.5 text-[13px]",
              fullWidth && "flex-1",
              active ? "text-ink" : "text-ink-muted hover:text-ink-secondary",
            )}
          >
            {active ? (
              <motion.span
                layoutId={layoutId}
                transition={reduced ? { duration: 0 } : SPRING_LIQUID}
                className="pointer-events-none absolute inset-0 -z-10 rounded-full bg-elevated shadow-[0_1px_2px_rgba(0,0,0,0.06),0_2px_8px_-2px_rgba(0,0,0,0.08)] ring-1 ring-line-strong/70 dark:bg-hover dark:ring-line-strong"
              />
            ) : null}
            {opt.icon}
            <span>{opt.label}</span>
            {opt.badge}
          </motion.button>
        );
      })}
    </div>
  );
}

/* ---------- 2. Switch ---------- */

export function Switch({
  checked,
  onChange,
  disabled = false,
  ariaLabel,
  size = "md",
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  ariaLabel?: string;
  size?: "sm" | "md";
}) {
  const reduced = useReducedMotion();
  const [pressed, setPressed] = React.useState(false);
  const sm = size === "sm";
  const thumb = sm ? 14 : 18;
  const travel = sm ? 12 : 16;
  const stretch = pressed && !reduced ? 4 : 0;

  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onChange(!checked);
      }}
      onPointerDown={() => setPressed(true)}
      onPointerUp={() => setPressed(false)}
      onPointerLeave={() => setPressed(false)}
      onPointerCancel={() => setPressed(false)}
      className={cn(
        // before: 透明伪元素把触控热区扩到 ≥ 40×32，不改变视觉尺寸
        "relative inline-flex shrink-0 cursor-pointer items-center rounded-full p-[2px] outline-none transition-[background-color,box-shadow] duration-250 ease-out before:absolute before:-inset-x-1 before:-inset-y-2 before:content-[''] focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:ring-offset-2 focus-visible:ring-offset-bg disabled:cursor-not-allowed disabled:opacity-45",
        sm ? "h-[18px] w-[30px]" : "h-[22px] w-[38px]",
        checked
          ? "bg-accent shadow-[inset_0_1px_2px_rgba(0,0,0,0.12)]"
          : "bg-line-strong shadow-[inset_0_1px_2px_rgba(0,0,0,0.08)] hover:bg-ink-muted/45",
      )}
    >
      <motion.span
        initial={false}
        animate={{
          x: checked ? travel - stretch : 0,
          width: thumb + stretch,
        }}
        transition={reduced ? { duration: 0 } : SPRING_LIQUID}
        style={{ height: thumb }}
        className="block rounded-full bg-white shadow-[0_1px_3px_rgba(0,0,0,0.22),0_0_0_0.5px_rgba(0,0,0,0.06)]"
      />
    </button>
  );
}

/* ---------- 3. Slider ---------- */

export function Slider({
  value,
  min,
  max,
  step = 1,
  onChange,
  onCommit,
  ariaLabel,
  className,
  disabled = false,
  formatValue,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (value: number) => void;
  onCommit?: (value: number) => void;
  ariaLabel?: string;
  className?: string;
  disabled?: boolean;
  /** 拖动时在滑块上方浮现数值气泡 */
  formatValue?: (value: number) => React.ReactNode;
}) {
  const reduced = useReducedMotion();
  const [dragging, setDragging] = React.useState(false);
  const pct = max > min ? Math.min(100, Math.max(0, ((value - min) / (max - min)) * 100)) : 0;

  const commit = (el: HTMLInputElement) => onCommit?.(Number(el.value));
  const formatted = formatValue?.(value);
  const valueText =
    typeof formatted === "string" || typeof formatted === "number" ? String(formatted) : undefined;

  return (
    <div
      className={cn(
        "group/slider relative flex h-7 touch-pan-y items-center",
        disabled && "pointer-events-none opacity-45",
        className,
      )}
    >
      <input
        type="range"
        aria-label={ariaLabel}
        aria-valuetext={valueText}
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(Number(e.target.value))}
        onPointerDown={() => setDragging(true)}
        onPointerUp={(e) => {
          setDragging(false);
          commit(e.currentTarget);
        }}
        onPointerCancel={() => setDragging(false)}
        onBlur={() => setDragging(false)}
        onKeyUp={(e) => commit(e.currentTarget)}
        className="peer absolute inset-0 z-0 h-full w-full cursor-pointer opacity-0"
      />
      <div className="pointer-events-none relative h-1.5 w-full overflow-hidden rounded-full bg-line-strong/70 shadow-[inset_0_1px_1px_rgba(0,0,0,0.06)]">
        <div
          className={cn(
            "h-full rounded-full bg-accent",
            !dragging && !reduced && "transition-[width] duration-150 ease-out",
          )}
          style={{ width: `${pct}%` }}
        />
      </div>
      <div
        aria-hidden
        className={cn(
          "pointer-events-none absolute top-1/2 size-[18px] -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-accent bg-elevated shadow-[0_1px_4px_rgba(0,0,0,0.18)] duration-200 ease-out group-hover/slider:scale-110 peer-focus-visible:ring-4 peer-focus-visible:ring-accent/25",
          dragging || reduced
            ? "transition-[scale,box-shadow]"
            : "transition-[left,scale,box-shadow]",
          dragging && "scale-[1.18] shadow-[0_2px_10px_rgba(0,0,0,0.22)] group-hover/slider:scale-[1.18]",
        )}
        style={{ left: `${pct}%` }}
      >
        {formatted !== undefined ? (
          <span
            className={cn(
              "absolute bottom-full left-1/2 mb-2 -translate-x-1/2 rounded-md bg-ink px-1.5 py-0.5 font-mono text-[10px] whitespace-nowrap text-bg tabular-nums shadow-sm transition-all duration-150",
              dragging ? "translate-y-0 opacity-100" : "translate-y-1 opacity-0",
            )}
          >
            {formatted}
          </span>
        ) : null}
      </div>
    </div>
  );
}

/* ---------- 4. Badge ---------- */

export type BadgeTone = "default" | "accent" | "success" | "warn" | "danger";

const BADGE_MAP: Record<BadgeTone, string> = {
  default: "bg-subtle text-ink-secondary border-line",
  accent: "bg-accent-soft text-accent border-accent/20",
  success: "bg-success-soft text-success border-success/20",
  warn: "bg-warn-soft text-warn border-warn/20",
  danger: "bg-danger-soft text-danger border-danger/20",
};

export function Badge({
  tone = "default",
  children,
  className,
}: {
  tone?: BadgeTone;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium leading-none tabular-nums",
        BADGE_MAP[tone],
        className,
      )}
    >
      {children}
    </span>
  );
}

/* ---------- 5. InlineConfirm (Zero-modal calm destructive confirmation!) ---------- */

export function InlineConfirm({
  trigger,
  confirmLabel = "确认删除",
  onConfirm,
  busy = false,
}: {
  trigger: (open: () => void) => React.ReactNode;
  confirmLabel?: string;
  onConfirm: () => void | Promise<void>;
  busy?: boolean;
}) {
  const [armed, setArmed] = React.useState(false);

  React.useEffect(() => {
    if (!armed) return;
    const timer = window.setTimeout(() => setArmed(false), 5000);
    return () => window.clearTimeout(timer);
  }, [armed]);

  return (
    <AnimatePresence mode="wait" initial={false}>
      {armed ? (
        <motion.div
          key="confirm"
          initial={{ opacity: 0, scale: 0.94 }}
          animate={{ opacity: 1, scale: 1 }}
          exit={{ opacity: 0, scale: 0.94 }}
          transition={SPRING_SNAPPY}
          className="inline-flex items-center gap-1 rounded-full border border-danger/30 bg-danger-soft/90 p-0.5 pl-2.5"
        >
          <button
            type="button"
            disabled={busy}
            onClick={async (e) => {
              e.stopPropagation();
              await onConfirm();
              setArmed(false);
            }}
            className="cursor-pointer rounded-full bg-danger px-2.5 py-1 text-[11px] font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-50"
          >
            {busy ? "处理中…" : confirmLabel}
          </button>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setArmed(false);
            }}
            className="cursor-pointer rounded-full px-2 py-1 text-[11px] text-ink-secondary hover:text-ink"
          >
            取消
          </button>
        </motion.div>
      ) : (
        <motion.div
          key="trigger"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          className="inline-flex"
        >
          {trigger(() => setArmed(true))}
        </motion.div>
      )}
    </AnimatePresence>
  );
}

/* ---------- 6. EmptyView & Spinner ---------- */

export function EmptyView({
  icon,
  title,
  description,
  action,
  compact = false,
  className,
}: {
  icon?: React.ReactNode;
  title: string;
  description?: string;
  action?: React.ReactNode;
  compact?: boolean;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center text-center",
        compact ? "gap-2 py-10 px-4" : "gap-3 py-18 px-6",
        className,
      )}
    >
      {icon ? (
        <div className="flex size-12 items-center justify-center rounded-2xl border border-line bg-subtle text-ink-muted shadow-[inset_0_1px_0_var(--highlight)]">
          {icon}
        </div>
      ) : null}
      <p className="font-serif text-base font-medium text-ink">{title}</p>
      {description ? (
        <p className="max-w-sm text-xs leading-relaxed text-ink-muted text-balance">
          {description}
        </p>
      ) : null}
      {action ? <div className="mt-2 flex flex-wrap items-center justify-center gap-2">{action}</div> : null}
    </div>
  );
}

export function Spinner({ label }: { label?: string }) {
  return (
    <div role="status" className="inline-flex items-center gap-2 text-xs text-ink-muted">
      <span aria-hidden className="size-4 animate-spin rounded-full border-2 border-line-strong border-t-accent [animation-duration:0.8s]" />
      {label ? <span>{label}</span> : null}
    </div>
  );
}
