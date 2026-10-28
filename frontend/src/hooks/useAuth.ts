import { useCallback, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { UNAUTHORIZED_EVENT } from "@/lib/api-client";
import { logout as logoutRequest } from "@/services/auth";
import { useAuthStore } from "@/stores/auth-store";
import type { LoginResult } from "@/types/api";

export interface UseAuthResult {
  isAuthenticated: boolean;
  username: string | null;
  /** 写入登录态(内存 + localStorage) */
  login: (result: LoginResult) => void;
  /** 通知后端登出并清除本地登录态, 跳转 /login */
  logout: () => Promise<void>;
}

/** 无需登录即可访问的公开页面: 全局 unauthorized 兜底不把访客从这里踢去 /login */
const PUBLIC_PATHS = new Set<string>(["/graph-demo"]);

/** 构造 `/login?redirect={原路径}` 跳转地址(守卫与 unauthorized 监听共用, 保持格式一致) */
export function loginRedirect(pathname: string, search: string): string {
  return `/login?redirect=${encodeURIComponent(pathname + search)}`;
}

/** 校验并归一化 redirect 参数: 只接受单个 "/" 开头的站内路径, 防开放重定向 */
export function resolveRedirect(raw: string | null): string {
  if (raw === null || !raw.startsWith("/") || raw.startsWith("//")) {
    return "/";
  }
  return raw;
}

/**
 * 认证状态与全局登出监听.
 *
 * 在 App 顶层调用一次: 监听 api-client 广播的 `reader:unauthorized` 事件
 * (token 失效 / NEED_LOGIN / 401), 清登录态并跳 `/login?redirect=当前路径`.
 * 组件内可再用返回值读取登录态或主动登出.
 */
export function useAuth(): UseAuthResult {
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const username = useAuthStore((state) => state.username);
  const navigate = useNavigate();

  useEffect(() => {
    const onUnauthorized = (): void => {
      useAuthStore.getState().logout();
      const { pathname, search } = window.location;
      // 公开页面(如 /graph-demo 这类不读登录态的 demo)不踢去登录页:
      // 应用根的 useCustomFonts 会打一次 /reader3 字体接口, 未登录时 401 不该把访客弹走
      if (pathname !== "/login" && !PUBLIC_PATHS.has(pathname)) {
        void navigate(loginRedirect(pathname, search), { replace: true });
      }
    };
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => {
      window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    };
  }, [navigate]);

  const login = useCallback((result: LoginResult): void => {
    useAuthStore.getState().login(result);
  }, []);

  const logout = useCallback(async (): Promise<void> => {
    try {
      await logoutRequest();
    } finally {
      useAuthStore.getState().logout();
      void navigate("/login", { replace: true });
    }
  }, [navigate]);

  return { isAuthenticated, username, login, logout };
}
