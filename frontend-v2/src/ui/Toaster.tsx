import { AlertCircle, CheckCircle2, Info, X } from "lucide-react";
import { cn } from "@/lib/cn";
import { useToastStore } from "@/stores/toast";
import { AnimatePresence, motion, SPRING_SNAPPY, useReducedMotion } from "./motion";

/** 横向轻扫超过阈值即关闭通知 */
const SWIPE_DISMISS = 72;

export function Toaster() {
  const items = useToastStore((s) => s.items);
  const dismiss = useToastStore((s) => s.dismiss);
  const reduced = useReducedMotion();

  return (
    <div
      aria-live="polite"
      aria-relevant="additions"
      className="pointer-events-none fixed inset-x-0 top-[max(1rem,calc(env(safe-area-inset-top)+0.5rem))] z-[100] flex flex-col items-center gap-2 px-4"
    >
      <AnimatePresence initial={false}>
        {items.map((item) => (
          <motion.div
            key={item.id}
            layout={!reduced}
            role={item.tone === "error" ? "alert" : "status"}
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: -16, scale: 0.94 }}
            animate={{ opacity: 1, y: 0, scale: 1, x: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: -10, scale: 0.95, transition: { duration: 0.16 } }}
            transition={SPRING_SNAPPY}
            drag={reduced ? false : "x"}
            dragConstraints={{ left: 0, right: 0 }}
            dragElastic={0.55}
            onDragEnd={(_, info) => {
              if (Math.abs(info.offset.x) > SWIPE_DISMISS || Math.abs(info.velocity.x) > 500) {
                dismiss(item.id);
              }
            }}
            className={cn(
              "surface-glass pointer-events-auto flex max-w-[min(24rem,calc(100vw-2rem))] cursor-grab touch-pan-y items-center gap-2.5 rounded-full py-1.5 pr-1.5 pl-3 text-[13px] text-ink active:cursor-grabbing",
              item.tone === "error" && "border-danger/30",
              item.tone === "success" && "border-success/25",
            )}
          >
            <span
              className={cn(
                "flex size-6 shrink-0 items-center justify-center rounded-full",
                item.tone === "success"
                  ? "bg-success-soft text-success"
                  : item.tone === "error"
                    ? "bg-danger-soft text-danger"
                    : "bg-accent-soft text-accent",
              )}
            >
              {item.tone === "success" ? (
                <CheckCircle2 className="size-3.5" aria-hidden />
              ) : item.tone === "error" ? (
                <AlertCircle className="size-3.5" aria-hidden />
              ) : (
                <Info className="size-3.5" aria-hidden />
              )}
            </span>
            <span className="min-w-0 flex-1 py-0.5 leading-snug select-none">{item.message}</span>
            {item.action ? (
              <button
                type="button"
                onPointerDown={(e) => e.stopPropagation()}
                onClick={() => {
                  item.action?.onClick();
                  dismiss(item.id);
                }}
                className="shrink-0 cursor-pointer rounded-full bg-accent-soft px-2.5 py-1 text-xs font-medium text-accent transition-colors hover:bg-accent hover:text-accent-ink"
              >
                {item.action.label}
              </button>
            ) : null}
            <button
              type="button"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={() => dismiss(item.id)}
              aria-label="关闭通知"
              className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-subtle hover:text-ink"
            >
              <X className="size-3.5" />
            </button>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
