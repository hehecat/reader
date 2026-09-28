import { useQuery } from "@tanstack/react-query";

import { getSystemInfo } from "@/services/auth";

export interface AppModeInfo {
  /** 首次请求尚未返回 */
  loading: boolean;
  /** 多用户模式(需登录); false = 单用户模式, 直接进入应用 */
  secure: boolean;
  /** 当前请求者是否管理员(未登录时为 false) */
  isAdmin: boolean;
  /** 注册是否需要邀请码 */
  inviteRequired: boolean;
}

/** localStorage 里是否已有登录态(单用户模式可能从未登录过) */
function hasToken(): boolean {
  try {
    return (window.localStorage.getItem("reader.accessToken") ?? "").length > 0;
  } catch {
    return false;
  }
}

/**
 * 应用运行模式与当前角色: 取自 /getSystemInfo(匿名亦可调用, 返回裁剪后的开关).
 * 单用户模式(secure=false)下后端不校验登录(resolve_namespace 恒 default),
 * 前端据此跳过登录页; 多用户模式按 isAdmin 决定是否显示管理入口.
 */
export function useAppMode(): AppModeInfo {
  const query = useQuery({
    queryKey: ["systemInfo"],
    queryFn: () => getSystemInfo(),
    staleTime: 5 * 60_000,
  });
  const data = query.data;
  // 响应缺失(离线/后端老版本) → 按多用户处理(保守: 要求登录), 除非本地已有 token
  const secure = data?.secure ?? true;
  return {
    loading: query.isLoading && !hasToken(),
    secure,
    isAdmin: data?.isAdmin ?? false,
    inviteRequired: data?.inviteRequired ?? false,
  };
}
