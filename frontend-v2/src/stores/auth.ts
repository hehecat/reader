import { create } from "zustand";
import { getToken, setToken } from "@/api/client";
import { authApi } from "@/api/endpoints";
import type { FormatUser, SystemInfo } from "@/api/types";

export interface AuthState {
  initialized: boolean;
  system: SystemInfo | null;
  user: FormatUser | null;
  token: string;
  bootstrap: () => Promise<void>;
  setSession: (user: FormatUser) => void;
  clearSession: () => void;
  logout: () => Promise<void>;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  initialized: false,
  system: null,
  user: null,
  token: getToken(),

  bootstrap: async () => {
    try {
      const [sys, info] = await Promise.all([
        authApi.getSystemInfo(),
        authApi.getUserInfo().catch(() => null),
      ]);
      const user = info?.userInfo ?? null;
      if (sys.secure && !user) {
        setToken(null);
      }
      set({
        initialized: true,
        system: sys,
        user,
        token: user?.accessToken ?? (sys.secure ? "" : getToken()),
      });
    } catch {
      set({ initialized: true });
    }
  },

  setSession: (user) => {
    setToken(user.accessToken);
    set({ user, token: user.accessToken });
  },

  clearSession: () => {
    setToken(null);
    set({ user: null, token: "" });
  },

  logout: async () => {
    const sys = get().system;
    if (sys?.secure) {
      try {
        await authApi.logout();
      } catch {
        // ignore network error on logout
      }
    }
    setToken(null);
    set({ user: null, token: "" });
  },
}));
