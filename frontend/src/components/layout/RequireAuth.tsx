import type { ReactElement } from "react";
import { Navigate, useLocation } from "react-router-dom";
import { useAppMode } from "@/hooks/useAppMode";
import { loginRedirect, resolveRedirect } from "@/hooks/useAuth";
import { useAuthStore } from "@/stores/auth-store";

interface RouteGuardProps {
  children: ReactElement;
}

/**
 * 登录守卫: 未认证访问受保护路由 → /login?redirect={原路径}.
 * 单用户模式(secure=false)后端不校验登录(resolve_namespace 恒 default), 直接放行 —— 
 * 该模式下不应把用户挡在登录页外。
 */
export function RequireAuth({ children }: RouteGuardProps): ReactElement {
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const { secure, loading } = useAppMode();
  const location = useLocation();
  if (loading) {
    return <div className="min-h-dvh" />;
  }
  if (!secure) {
    return children;
  }
  if (!isAuthenticated) {
    return <Navigate to={loginRedirect(location.pathname, location.search)} replace />;
  }
  return children;
}

/** 访客专属守卫: 已登录用户访问 /login → 回到 redirect 目标或 "/" */
export function GuestOnly({ children }: RouteGuardProps): ReactElement {
  const isAuthenticated = useAuthStore((state) => state.isAuthenticated);
  const { secure, loading } = useAppMode();
  const location = useLocation();
  // 单用户模式没有登录概念: 直接进应用
  if (!loading && !secure) {
    return <Navigate to={resolveRedirect(new URLSearchParams(location.search).get("redirect"))} replace />;
  }
  if (isAuthenticated) {
    const redirect = resolveRedirect(new URLSearchParams(location.search).get("redirect"));
    return <Navigate to={redirect} replace />;
  }
  return children;
}
