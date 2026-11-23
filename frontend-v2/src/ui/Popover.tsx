import * as React from "react";
import { createPortal } from "react-dom";
import { cn } from "@/lib/cn";
import { AnimatePresence, motion, SPRING_POP, useReducedMotion } from "./motion";

export interface PopoverProps {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  trigger: React.ReactNode;
  children: React.ReactNode | ((close: () => void) => React.ReactNode);
  align?: "start" | "end";
  side?: "bottom" | "top";
  className?: string;
}

/** 浮层与视口边缘的最小安全距离 */
const VIEWPORT_GUTTER = 8;
/** 浮层与触发器之间的间距（对应 mt-2 / mb-2） */
const TRIGGER_GAP = 8;

interface Placement {
  side: "bottom" | "top";
  left: number;
  top?: number;
  bottom?: number;
  /** 内容超出可用高度时的最大高度 */
  maxHeight?: number;
  ready: boolean;
}

const FOCUSABLE_ITEM = 'button:not([disabled]), [role="menuitem"]:not([aria-disabled="true"]), a[href]';

export function Popover({
  open: controlledOpen,
  onOpenChange,
  trigger,
  children,
  align = "end",
  side = "bottom",
  className,
}: PopoverProps) {
  const [innerOpen, setInnerOpen] = React.useState(false);
  const open = controlledOpen ?? innerOpen;
  const setOpen = React.useCallback(
    (next: boolean) => {
      if (controlledOpen === undefined) setInnerOpen(next);
      onOpenChange?.(next);
    },
    [controlledOpen, onOpenChange],
  );

  const rootRef = React.useRef<HTMLDivElement>(null);
  const triggerRef = React.useRef<HTMLDivElement>(null);
  const panelRef = React.useRef<HTMLDivElement>(null);
  const reduced = useReducedMotion();
  const [placement, setPlacement] = React.useState<Placement>({
    side,
    left: VIEWPORT_GUTTER,
    top: VIEWPORT_GUTTER,
    ready: false,
  });

  const restoreTriggerFocus = React.useCallback(() => {
    const el = triggerRef.current?.querySelector<HTMLElement>(
      "button, a[href], [tabindex]:not([tabindex='-1'])",
    );
    el?.focus({ preventScroll: true });
  }, []);

  // Portal fixed 视口防溢出定位：彻底脱离父级 sticky / backdrop-blur / overflow 层叠上下文
  const reposition = React.useCallback(() => {
    const root = rootRef.current;
    const panel = panelRef.current;
    if (!root || !panel) return;
    const anchor = root.getBoundingClientRect();
    const vw = document.documentElement.clientWidth || window.innerWidth;
    const vh = window.innerHeight;
    const maxWidth = Math.max(160, vw - VIEWPORT_GUTTER * 2);
    const width = Math.min(panel.offsetWidth || 200, maxWidth);
    const height = panel.scrollHeight;

    const naturalLeft = align === "end" ? anchor.right - width : anchor.left;
    const maxLeft = Math.max(VIEWPORT_GUTTER, vw - VIEWPORT_GUTTER - width);
    const left = Math.round(Math.min(Math.max(naturalLeft, VIEWPORT_GUTTER), maxLeft));

    const spaceBelow = vh - anchor.bottom - TRIGGER_GAP - VIEWPORT_GUTTER;
    const spaceAbove = anchor.top - TRIGGER_GAP - VIEWPORT_GUTTER;
    let nextSide = side;
    if (side === "bottom" && height > spaceBelow && spaceAbove > spaceBelow) nextSide = "top";
    else if (side === "top" && height > spaceAbove && spaceBelow > spaceAbove) nextSide = "bottom";

    const available = nextSide === "bottom" ? spaceBelow : spaceAbove;
    const maxHeight = height > available ? Math.max(160, Math.floor(available)) : undefined;
    const top = nextSide === "bottom" ? Math.round(anchor.bottom + TRIGGER_GAP) : undefined;
    const bottom = nextSide === "top" ? Math.round(vh - anchor.top + TRIGGER_GAP) : undefined;

    setPlacement((prev) =>
      prev.ready &&
      prev.side === nextSide &&
      prev.left === left &&
      prev.top === top &&
      prev.bottom === bottom &&
      prev.maxHeight === maxHeight
        ? prev
        : { side: nextSide, left, top, bottom, maxHeight, ready: true },
    );
  }, [align, side]);

  React.useLayoutEffect(() => {
    if (!open) {
      setPlacement((prev) => (prev.ready ? { ...prev, ready: false } : prev));
      return;
    }
    reposition();
    const raf = window.requestAnimationFrame(reposition);
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    return () => {
      window.cancelAnimationFrame(raf);
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [open, reposition]);

  React.useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!rootRef.current?.contains(target) && !panelRef.current?.contains(target)) {
        setOpen(false);
      }
    };
    // 捕获阶段拦截 Escape：只关闭最上层浮层，不连带关闭其下的 Sheet / 阅读器 HUD
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      restoreTriggerFocus();
    };
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open, setOpen, restoreTriggerFocus]);

  // 菜单内 ↑ ↓ Home End 键盘游走
  const handlePanelKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const target = e.target as HTMLElement;
    if (target.matches("input, textarea, [contenteditable='true']")) return;
    const items = Array.from(panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_ITEM) ?? []);
    if (items.length === 0) return;
    e.preventDefault();
    const current = items.indexOf(document.activeElement as HTMLElement);
    let next = 0;
    if (e.key === "ArrowDown") next = current < 0 ? 0 : (current + 1) % items.length;
    else if (e.key === "ArrowUp") next = current < 0 ? items.length - 1 : (current - 1 + items.length) % items.length;
    else if (e.key === "End") next = items.length - 1;
    items[next]?.focus();
  };

  const resolvedSide = placement.side;
  const originY = resolvedSide === "bottom" ? "top" : "bottom";
  const originX = align === "end" ? "right" : "left";

  const portalContent = (
    <AnimatePresence>
      {open ? (
        <motion.div
          ref={panelRef}
          initial={
            reduced ? { opacity: 0 } : { opacity: 0, scale: 0.96, y: resolvedSide === "bottom" ? -6 : 6 }
          }
          animate={{ opacity: placement.ready ? 1 : 0, scale: 1, y: 0 }}
          exit={
            reduced ? { opacity: 0 } : { opacity: 0, scale: 0.97, y: resolvedSide === "bottom" ? -4 : 4 }
          }
          transition={reduced ? { duration: 0.1 } : SPRING_POP}
          onClick={(e) => e.stopPropagation()}
          onKeyDown={handlePanelKeyDown}
          style={{
            position: "fixed",
            left: placement.left,
            ...(placement.top !== undefined ? { top: placement.top } : null),
            ...(placement.bottom !== undefined ? { bottom: placement.bottom } : null),
            maxWidth: `calc(100vw - ${VIEWPORT_GUTTER * 2}px)`,
            transformOrigin: `${originY} ${originX}`,
            ...(placement.maxHeight !== undefined
              ? { maxHeight: placement.maxHeight, overflowY: "auto" as const, overscrollBehavior: "contain" as const }
              : null),
          }}
          className={cn(
            "surface-popover z-[80] min-w-[176px] rounded-2xl p-1.5 text-ink",
            className,
          )}
        >
          {typeof children === "function" ? children(() => setOpen(false)) : children}
        </motion.div>
      ) : null}
    </AnimatePresence>
  );

  return (
    <div ref={rootRef} className="relative inline-flex">
      <div
        ref={triggerRef}
        onClick={(e) => {
          e.stopPropagation();
          setOpen(!open);
        }}
        onKeyDown={(e) => {
          // 触发器聚焦时 ↓ 直接展开并进入菜单
          if (e.key === "ArrowDown" && !open) {
            e.preventDefault();
            setOpen(true);
            window.requestAnimationFrame(() => {
              panelRef.current?.querySelector<HTMLElement>(FOCUSABLE_ITEM)?.focus();
            });
          }
        }}
        className="inline-flex"
      >
        {trigger}
      </div>
      {typeof document !== "undefined" ? createPortal(portalContent, document.body) : null}
    </div>
  );
}

export function PopoverItem({
  icon,
  children,
  danger = false,
  disabled = false,
  onClick,
  shortcut,
  trailing,
}: {
  icon?: React.ReactNode;
  children: React.ReactNode;
  danger?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  /** 右侧快捷键角标，如 "⌘K" */
  shortcut?: string;
  /** 右侧自定义尾部内容（如勾选标记） */
  trailing?: React.ReactNode;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cn(
        "group/item flex w-full cursor-pointer items-center gap-2.5 rounded-xl px-3 py-2 text-left text-[13px] outline-none transition-colors duration-100 select-none disabled:pointer-events-none disabled:opacity-40",
        danger
          ? "text-danger hover:bg-danger-soft focus-visible:bg-danger-soft"
          : "text-ink hover:bg-subtle focus-visible:bg-subtle active:bg-hover",
      )}
    >
      {icon ? (
        <span
          className={cn(
            "flex shrink-0 items-center transition-opacity",
            danger ? "opacity-90" : "text-ink-secondary opacity-85 group-hover/item:opacity-100",
          )}
        >
          {icon}
        </span>
      ) : null}
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {trailing}
      {shortcut ? <kbd className="kbd-chip shrink-0">{shortcut}</kbd> : null}
    </button>
  );
}

export function PopoverDivider() {
  return <div role="separator" className="mx-1 my-1 h-px bg-line" />;
}

/** 浮层内分组小标题 */
export function PopoverLabel({ children }: { children: React.ReactNode }) {
  return (
    <p className="px-3 pt-1.5 pb-1 text-[10.5px] font-medium tracking-[0.14em] text-ink-muted select-none">
      {children}
    </p>
  );
}
