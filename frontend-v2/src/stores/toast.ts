import { create } from "zustand";

export type ToastTone = "info" | "success" | "error";

export interface ToastItem {
  id: string;
  tone: ToastTone;
  message: string;
  action?: { label: string; onClick: () => void };
}

interface ToastState {
  items: ToastItem[];
  push: (tone: ToastTone, message: string, action?: ToastItem["action"], durationMs?: number) => void;
  dismiss: (id: string) => void;
}

export const useToastStore = create<ToastState>((set) => ({
  items: [],
  push: (tone, message, action, durationMs) => {
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    set((s) => ({ items: [...s.items.slice(-2), { id, tone, message, action }] }));
    const ttl = durationMs ?? (tone === "error" ? 5500 : 3200);
    window.setTimeout(() => {
      set((s) => ({ items: s.items.filter((t) => t.id !== id) }));
    }, ttl);
  },
  dismiss: (id) => set((s) => ({ items: s.items.filter((t) => t.id !== id) })),
}));

export const notify = {
  info: (msg: string, action?: ToastItem["action"]) => useToastStore.getState().push("info", msg, action),
  success: (msg: string, action?: ToastItem["action"]) => useToastStore.getState().push("success", msg, action),
  error: (msg: string, action?: ToastItem["action"]) => useToastStore.getState().push("error", msg, action),
};
