import { X } from "lucide-react";
import * as React from "react";
import { createPortal } from "react-dom";
import { cn } from "@/lib/cn";
import { useDragControls, type PanInfo } from "motion/react";
import { Segmented } from "./Controls";
import {
  AnimatePresence,
  EASE_PAPER,
  motion,
  SPRING_SHEET,
  SPRING_SNAPPY,
  useMediaQuery,
  useReducedMotion,
} from "./motion";

export interface SheetProps {
  open: boolean;
  onClose: () => void;
  title?: React.ReactNode;
  subtitle?: React.ReactNode;
  headerExtra?: React.ReactNode;
  footer?: React.ReactNode;
  side?: "right" | "left";
  size?: "sm" | "md" | "lg" | "xl";
  /**
   * 移动端抽屉高度策略：
   * - "auto": 随内容自适应（默认，适合无页签的紧凑表单）
   * - "tall": 锁定 76dvh 等高（适合多页签面板，切换页签时不跳动）
   * - "full": 锁定 86dvh 等高（适合目录/详情/检视器等长列表多页签面板）
   */
  mobileHeight?: "auto" | "tall" | "full";
  /** 极淡柔焦背景点击关闭；设为 false 可实现完全轻盈的非阻塞边栏 */
  scrim?: boolean;
  children: React.ReactNode;
  bodyClassName?: string;
}

const MOBILE_HEIGHT_MAP: Record<NonNullable<SheetProps["mobileHeight"]>, string> = {
  auto: "",
  tall: "h-[76dvh] sm:h-auto",
  full: "h-[86dvh] sm:h-auto",
};

const WIDTH_MAP: Record<NonNullable<SheetProps["size"]>, string> = {
  sm: "sm:w-[360px]",
  md: "sm:w-[460px]",
  lg: "sm:w-[580px]",
  xl: "sm:w-[720px]",
};

let activeSheetDepthCount = 0;

function syncSheetDepth(delta: 1 | -1) {
  if (typeof document === "undefined") return;
  activeSheetDepthCount = Math.max(0, activeSheetDepthCount + delta);
  document.documentElement.classList.toggle("sheet-depth-active", activeSheetDepthCount > 0);
}

/** 打开中的抽屉栈：Escape 只关闭最上层，避免层叠抽屉一次全关 */
const sheetStack: symbol[] = [];

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** 下拉关闭阈值 */
const DISMISS_OFFSET = 110;
const DISMISS_VELOCITY = 600;

/**
 * 空间悬浮抽屉 (Spatial Inspector Sheet):
 * - 桌面端: 从右侧/左侧轻盈滑出的悬浮面板（距视口边缘 12px 圆角浮层），头部固定、内容独立滚动
 * - 移动端: 自底部上滑的圆角拉手抽屉，支持按住拉手 / 头部下拉关闭，并适配底部安全区
 */
export function Sheet({
  open,
  onClose,
  title,
  subtitle,
  headerExtra,
  footer,
  side = "right",
  size = "md",
  mobileHeight = "auto",
  scrim = true,
  children,
  bodyClassName,
}: SheetProps) {
  const reduced = useReducedMotion();
  const isMobile = useMediaQuery("(max-width: 639px)");
  const dragControls = useDragControls();
  const panelRef = React.useRef<HTMLElement>(null);
  const [scrolled, setScrolled] = React.useState(false);
  const titleId = React.useId();

  const onCloseRef = React.useRef(onClose);
  React.useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  React.useEffect(() => {
    if (!open || !scrim) return;
    syncSheetDepth(1);
    return () => syncSheetDepth(-1);
  }, [open, scrim]);

  // Escape（仅栈顶抽屉响应）+ 焦点进入面板 / 关闭后归还
  React.useEffect(() => {
    if (!open) return;
    const token = Symbol("sheet");
    sheetStack.push(token);
    const previouslyFocused = document.activeElement as HTMLElement | null;

    const focusTimer = window.setTimeout(() => {
      const panel = panelRef.current;
      if (!panel || panel.contains(document.activeElement)) return;
      const autoTarget = panel.querySelector<HTMLElement>("[autofocus], [data-autofocus]");
      (autoTarget ?? panel).focus({ preventScroll: true });
    }, 40);

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      if (sheetStack[sheetStack.length - 1] !== token) return;
      e.stopPropagation();
      onCloseRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.clearTimeout(focusTimer);
      window.removeEventListener("keydown", onKey);
      const idx = sheetStack.indexOf(token);
      if (idx >= 0) sheetStack.splice(idx, 1);
      if (previouslyFocused && document.contains(previouslyFocused)) {
        previouslyFocused.focus({ preventScroll: true });
      }
    };
  }, [open]);

  // 模态抽屉内 Tab 焦点循环
  const handleKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    if (e.key !== "Tab" || !scrim) return;
    const panel = panelRef.current;
    if (!panel) return;
    const nodes = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null || el === document.activeElement,
    );
    if (nodes.length === 0) {
      e.preventDefault();
      return;
    }
    const first = nodes[0]!;
    const last = nodes[nodes.length - 1]!;
    if (e.shiftKey && (document.activeElement === first || document.activeElement === panel)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  const handleDragEnd = (_: unknown, info: PanInfo) => {
    if (info.offset.y > DISMISS_OFFSET || info.velocity.y > DISMISS_VELOCITY) onClose();
  };

  const startDrag = (e: React.PointerEvent) => {
    if (!isMobile || reduced) return;
    if ((e.target as HTMLElement).closest("button, a, input, textarea, select, [role='tab']")) return;
    dragControls.start(e);
  };

  const initialAnim = reduced
    ? { opacity: 0 }
    : isMobile
      ? { opacity: 1, y: "100%" }
      : { opacity: 0, x: side === "right" ? 48 : -48, scale: 0.985 };

  const exitAnim = reduced
    ? { opacity: 0 }
    : isMobile
      ? { opacity: 1, y: "100%" }
      : { opacity: 0, x: side === "right" ? 36 : -36, scale: 0.99 };

  if (typeof document === "undefined") return null;

  return createPortal(
    <AnimatePresence onExitComplete={() => setScrolled(false)}>
      {open ? (
        <div
          className={cn(
            "fixed inset-0 z-50 flex items-end sm:items-stretch",
            side === "right" ? "justify-end" : "justify-start",
          )}
        >
          {/* Scrim */}
          <motion.div
            aria-hidden
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.22 }}
            onClick={onClose}
            className={cn(
              "fixed inset-0",
              scrim ? "bg-[var(--scrim)] backdrop-blur-[2px]" : "bg-transparent",
            )}
          />

          {/* Floating Sheet Panel */}
          <motion.aside
            ref={panelRef}
            role="dialog"
            aria-modal={scrim ? "true" : undefined}
            aria-labelledby={title ? titleId : undefined}
            tabIndex={-1}
            onKeyDown={handleKeyDown}
            initial={initialAnim}
            animate={{ opacity: 1, x: 0, y: 0, scale: 1 }}
            exit={exitAnim}
            transition={reduced ? { duration: 0.12 } : SPRING_SHEET}
            drag={isMobile && !reduced ? "y" : false}
            dragListener={false}
            dragControls={dragControls}
            dragConstraints={{ top: 0, bottom: 0 }}
            dragElastic={{ top: 0.04, bottom: 0.7 }}
            onDragEnd={handleDragEnd}
            className={cn(
              "surface-sheet relative z-10 flex max-h-[92dvh] w-full flex-col overflow-hidden rounded-t-[26px] outline-none",
              MOBILE_HEIGHT_MAP[mobileHeight],
              "sm:fixed sm:inset-y-3 sm:max-h-none sm:rounded-2xl",
              side === "right" ? "sm:right-3" : "sm:left-3",
              WIDTH_MAP[size],
            )}
          >
            {/* Mobile grab bar：按住即可下拉关闭 */}
            <div
              onPointerDown={startDrag}
              className="flex shrink-0 cursor-grab touch-none justify-center pt-2.5 pb-1.5 active:cursor-grabbing sm:hidden"
            >
              <span className="h-1 w-10 rounded-full bg-ink-muted/30" />
            </div>

            {title || headerExtra ? (
              <header
                onPointerDown={startDrag}
                className={cn(
                  "relative z-10 flex shrink-0 items-start justify-between gap-3 border-b px-5 pt-2 pb-3.5 transition-[border-color,box-shadow] duration-200 sm:pt-4",
                  scrolled
                    ? "border-line shadow-[0_6px_14px_-12px_rgba(0,0,0,0.35)]"
                    : "border-line/70",
                )}
              >
                <div className="min-w-0 flex-1">
                  {title ? (
                    <h2
                      id={titleId}
                      className="truncate font-serif text-[17px] leading-snug font-semibold tracking-tight text-ink"
                    >
                      {title}
                    </h2>
                  ) : null}
                  {subtitle ? (
                    <p className="mt-0.5 truncate text-xs text-ink-muted">{subtitle}</p>
                  ) : null}
                </div>
                <div className="-mr-1.5 flex shrink-0 items-center gap-1.5">
                  {headerExtra}
                  <button
                    type="button"
                    onClick={onClose}
                    aria-label="关闭面板"
                    className="relative flex size-8 cursor-pointer items-center justify-center rounded-full text-ink-muted outline-none transition-colors before:absolute before:-inset-1 before:content-[''] hover:bg-subtle hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 active:bg-hover"
                  >
                    <X className="size-4" />
                  </button>
                </div>
              </header>
            ) : null}

            <div
              onScroll={(e) => {
                const next = e.currentTarget.scrollTop > 4;
                if (next !== scrolled) setScrolled(next);
              }}
              className={cn(
                "min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-4",
                !footer && "pb-[max(1rem,env(safe-area-inset-bottom))]",
                bodyClassName,
              )}
            >
              {children}
            </div>

            {footer ? (
              <footer className="shrink-0 border-t border-line bg-elevated/60 px-5 pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] sm:pb-3">
                {footer}
              </footer>
            ) : null}
          </motion.aside>
        </div>
      ) : null}
    </AnimatePresence>,
    document.body,
  );
}

/* ==========================================================================
   SheetTabs & SheetTabPanel
   统一的弹出抽屉页签语言：
   - 移动端：无边框底部滑动指示线（SPRING_SNAPPY）
   - 桌面端：Segmented 胶囊切换（SPRING_LIQUID）
   - 内容区：方向感知的轻盈横向淡入淡出（EASE_PAPER），配合 Sheet mobileHeight 实现零跳动等高切换
   ========================================================================== */

export interface SheetTabOption<T extends string> {
  value: T;
  label: React.ReactNode;
  icon?: React.ReactNode;
  badge?: React.ReactNode;
}

export interface SheetTabsProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: ReadonlyArray<SheetTabOption<T>>;
  layoutId: string;
  ariaLabel?: string;
  /** 桌面端 Segmented 是否撑满整行（默认 true） */
  fullWidthDesktop?: boolean;
  /** 桌面端右侧附加信息（如上次阅读时间） */
  desktopExtra?: React.ReactNode;
  className?: string;
}

export function SheetTabs<T extends string>({
  value,
  onChange,
  options,
  layoutId,
  ariaLabel,
  fullWidthDesktop = true,
  desktopExtra,
  className,
}: SheetTabsProps<T>) {
  const reduced = useReducedMotion();

  return (
    <div className={cn("shrink-0", className)}>
      {/* Mobile: borderless underline tabs with spring-sliding indicator */}
      <div
        role="tablist"
        aria-label={ariaLabel}
        className="flex items-center gap-6 overflow-x-auto border-b border-line/70 no-scrollbar sm:hidden"
      >
        {options.map((opt) => {
          const active = value === opt.value;
          return (
            <button
              key={opt.value}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => onChange(opt.value)}
              className={cn(
                "relative inline-flex shrink-0 cursor-pointer items-center gap-1.5 py-2.5 text-[13px] transition-colors outline-none focus-visible:text-accent",
                active ? "font-semibold text-accent" : "text-ink-muted hover:text-ink",
              )}
            >
              {opt.icon ? <span className="inline-flex shrink-0">{opt.icon}</span> : null}
              <span>{opt.label}</span>
              {opt.badge ? <span className="inline-flex shrink-0">{opt.badge}</span> : null}
              {active ? (
                <motion.span
                  layoutId={`${layoutId}-mobile-underline`}
                  transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                  className="pointer-events-none absolute inset-x-0 bottom-0 h-[2px] rounded-full bg-accent"
                />
              ) : null}
            </button>
          );
        })}
      </div>

      {/* Desktop: Segmented pill with optional trailing meta */}
      <div className="hidden items-center justify-between gap-3 sm:flex">
        <Segmented<T>
          layoutId={layoutId}
          size="sm"
          fullWidth={fullWidthDesktop}
          ariaLabel={ariaLabel}
          value={value}
          onChange={onChange}
          options={options}
        />
        {desktopExtra ? <div className="shrink-0">{desktopExtra}</div> : null}
      </div>
    </div>
  );
}

/**
 * 方向感知的页签切换状态辅助 hook：
 * 记录切换方向（1 = 向右前进，-1 = 向左返回），供 SheetTabPanel 呈现自然滑入动效。
 */
export function useSheetTabDirection<T extends string>(
  value: T,
  onChange: (next: T) => void,
  order: readonly T[],
) {
  const [direction, setDirection] = React.useState<1 | -1>(1);

  const handleChange = React.useCallback(
    (next: T) => {
      if (next === value) return;
      const curIdx = order.indexOf(value);
      const nextIdx = order.indexOf(next);
      if (curIdx >= 0 && nextIdx >= 0) {
        setDirection(nextIdx >= curIdx ? 1 : -1);
      }
      onChange(next);
    },
    [value, onChange, order],
  );

  return { direction, onChange: handleChange };
}

export interface SheetTabPanelProps {
  activeKey: string;
  direction?: 1 | -1;
  className?: string;
  children: React.ReactNode;
}

export function SheetTabPanel({
  activeKey,
  direction = 1,
  className,
  children,
}: SheetTabPanelProps) {
  const reduced = useReducedMotion();

  return (
    <AnimatePresence mode="wait" initial={false} custom={direction}>
      <motion.div
        key={activeKey}
        role="tabpanel"
        custom={direction}
        initial={reduced ? { opacity: 0 } : { opacity: 0, x: direction * 14 }}
        animate={{ opacity: 1, x: 0 }}
        exit={reduced ? { opacity: 0 } : { opacity: 0, x: direction * -10 }}
        transition={reduced ? { duration: 0.08 } : { duration: 0.16, ease: EASE_PAPER }}
        className={cn("min-h-0 flex-1", className)}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  );
}
