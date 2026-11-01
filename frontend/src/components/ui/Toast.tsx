import { CircleAlert, CircleCheck, Info, X } from "lucide-react";
import * as React from "react";
import { create } from "zustand";

import { cn } from "./cn";

import "./motion.css";

export type ToastType = "success" | "error" | "info";

export interface ToastItem {
  id: number;
  message: string;
  type: ToastType;
  /** 自动消失毫秒数, <= 0 表示常驻(需手动 dismiss) */
  duration: number;
  /** 是否正在播放退场动效 */
  leaving: boolean;
}

export interface ToastState {
  toasts: ToastItem[];
  /** 追加一条 toast 并返回 id; duration 默认 3000ms(error 6000ms), 传 0 表示不自动消失 */
  push: (message: string, type?: ToastType, duration?: number) => number;
  /** 关闭指定 toast(先播放退场动效再移除) */
  dismiss: (id: number) => void;
  /** 立即清空全部 toast */
  clear: () => void;
}

const DEFAULT_DURATION = 3000;
/** 错误提示: 6 秒起步(失败信息要读得完), 且单独走 role="alert" 立即播报 */
const ERROR_DURATION = 6000;
const LEAVE_DURATION = 180;
const MAX_TOASTS = 3;

let nextToastId = 0;

/** toast 队列 store(自建, 不依赖 Radix): 同屏最多 3 条, 超出丢弃最早的. */
export const useToastStore = create<ToastState>()((set, get) => ({
  toasts: [],
  push: (
    message,
    type = "info",
    duration = type === "error" ? ERROR_DURATION : DEFAULT_DURATION,
  ) => {
    nextToastId += 1;
    const item: ToastItem = {
      id: nextToastId,
      message,
      type,
      duration,
      leaving: false,
    };
    const next = [...get().toasts.filter((toast) => !toast.leaving), item];
    set({ toasts: next.slice(-MAX_TOASTS) });
    return item.id;
  },
  dismiss: (id) => {
    const target = get().toasts.find((toast) => toast.id === id);
    if (!target || target.leaving) return;
    set({
      toasts: get().toasts.map((toast) => (toast.id === id ? { ...toast, leaving: true } : toast)),
    });
    window.setTimeout(() => {
      set({ toasts: get().toasts.filter((toast) => toast.id !== id) });
    }, LEAVE_DURATION);
  },
  clear: () => set({ toasts: [] }),
}));

/** 命令式入口: toast.success("已加入书架") / toast.error(msg) / toast.info(msg) / toast.dismiss(id). */
export const toast = {
  success: (message: string, duration?: number) =>
    useToastStore.getState().push(message, "success", duration),
  error: (message: string, duration?: number) =>
    useToastStore.getState().push(message, "error", duration ?? ERROR_DURATION),
  info: (message: string, duration?: number) =>
    useToastStore.getState().push(message, "info", duration),
  push: (message: string, type: ToastType = "info", duration?: number) =>
    useToastStore.getState().push(message, type, duration),
  dismiss: (id: number) => useToastStore.getState().dismiss(id),
  clear: () => useToastStore.getState().clear(),
};

/** 计时器: 页面隐藏或鼠标悬停/聚焦时暂停, 恢复后按剩余时间继续. */
function useAutoDismiss(id: number, duration: number) {
  const dismiss = useToastStore((state) => state.dismiss);
  const timer = React.useRef<number | null>(null);
  const remaining = React.useRef(duration);
  const startedAt = React.useRef(0);

  const clearTimer = React.useCallback(() => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  const pause = React.useCallback(() => {
    if (timer.current === null) return;
    clearTimer();
    remaining.current = Math.max(0, remaining.current - (Date.now() - startedAt.current));
  }, [clearTimer]);

  const resume = React.useCallback(() => {
    if (duration <= 0 || timer.current !== null) return;
    startedAt.current = Date.now();
    timer.current = window.setTimeout(() => {
      timer.current = null;
      dismiss(id);
    }, remaining.current);
  }, [dismiss, duration, id]);

  React.useEffect(() => {
    if (!document.hidden) resume();
    const onVisibilityChange = () => {
      if (document.hidden) pause();
      else resume();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      clearTimer();
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [clearTimer, pause, resume]);

  return { pause, resume };
}

const iconByType = {
  success: CircleCheck,
  error: CircleAlert,
  info: Info,
} satisfies Record<ToastType, typeof CircleCheck>;

const toneByType: Record<ToastType, string> = {
  success: "text-accent",
  error: "text-danger",
  info: "text-muted-foreground",
};

interface ToastCardProps {
  item: ToastItem;
}

function ToastCard({ item }: ToastCardProps) {
  const dismiss = useToastStore((state) => state.dismiss);
  const { pause, resume } = useAutoDismiss(item.id, item.duration);
  const Icon = iconByType[item.type];

  return (
    <div
      data-leaving={item.leaving || undefined}
      role={item.type === "error" ? "alert" : undefined}
      onMouseEnter={pause}
      onMouseLeave={resume}
      onFocus={pause}
      onBlur={resume}
      className="ui-toast pointer-events-auto flex w-full max-w-80 items-start gap-2.5 rounded-xl border border-border bg-surface px-3.5 py-2.5 text-sm text-foreground shadow-xl"
    >
      <Icon aria-hidden className={cn("mt-0.5 size-4 shrink-0", toneByType[item.type])} />
      <p className="min-w-0 flex-1 leading-5 break-words">{item.message}</p>
      <button
        type="button"
        aria-label="关闭通知"
        onClick={() => dismiss(item.id)}
        className="-mt-0.5 -mr-1 inline-flex size-5 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground outline-none transition duration-150 ease-out hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}

export interface ToastViewportProps extends React.ComponentProps<"div"> {
  /** 停靠位置, 默认 bottom(自动避开安全区) */
  position?: "bottom" | "top";
}

/** toast 视口: 在 App 根节点挂一次即可, 队列来自 useToastStore, 无 toast 时不渲染. */
export function ToastViewport({ position = "bottom", className, ...props }: ToastViewportProps) {
  const toasts = useToastStore((state) => state.toasts);

  if (toasts.length === 0) return null;

  return (
    <div
      role="region"
      aria-label="通知"
      aria-live="polite"
      className={cn(
        "pointer-events-none fixed inset-x-0 z-100 flex flex-col items-center",
        position === "bottom" ? "ui-safe-b bottom-0" : "ui-safe-t top-0",
        className,
      )}
      {...props}
    >
      <div className="flex w-full flex-col items-center gap-2 p-4">
        {toasts.map((item) => (
          <ToastCard key={item.id} item={item} />
        ))}
      </div>
    </div>
  );
}
