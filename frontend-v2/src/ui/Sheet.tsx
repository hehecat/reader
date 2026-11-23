import { X } from "lucide-react";
import * as React from "react";
import { createPortal } from "react-dom";
import { cn } from "@/lib/cn";
import { useDragControls, type PanInfo } from "motion/react";
import {
  AnimatePresence,
  motion,
  SPRING_SHEET,
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
  /** 极淡柔焦背景点击关闭；设为 false 可实现完全轻盈的非阻塞边栏 */
  scrim?: boolean;
  children: React.ReactNode;
  bodyClassName?: string;
}

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
