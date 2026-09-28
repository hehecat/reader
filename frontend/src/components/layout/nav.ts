import {
  BookOpen,
  BookText,
  Compass,
  Rss,
  Search,
  Settings2,
  Sparkles,
  UserRound,
  UsersRound,
  type LucideIcon,
} from "lucide-react";

export interface NavItem {
  /** 目标路由路径 */
  to: string;
  /** 导航文案 */
  label: string;
  icon: LucideIcon;
  /** 根路径 "/" 需精确匹配, 否则所有路由都会高亮书架 */
  end?: boolean;
}

/**
 * 主导航项(桌面侧边栏 / 移动端 slide-over 共用):
 * 书架 /, 搜索 /search, 书海 /library, 书源 /sources, 订阅 /rss, 净化 /purify, 我的 /me
 */
export const NAV_ITEMS: NavItem[] = [
  { to: "/", label: "书架", icon: BookOpen, end: true },
  { to: "/search", label: "搜索", icon: Search },
  { to: "/library", label: "书海", icon: Compass },
  { to: "/sources", label: "书源", icon: BookText },
  { to: "/rss", label: "订阅", icon: Rss },
  { to: "/purify", label: "净化", icon: Sparkles },
  { to: "/me", label: "我的", icon: UserRound },
];

/** 分隔线之后的独立导航项 */
export const SETTINGS_NAV_ITEM: NavItem = { to: "/settings", label: "设置", icon: Settings2 };

/** 仅管理员可见的导航项(用户管理) */
export const ADMIN_NAV_ITEM: NavItem = { to: "/users", label: "用户", icon: UsersRound };

export interface RouteMeta {
  /** 顶栏第一行小标 */
  kicker: string;
  /** 顶栏衬线大标题 */
  title: string;
}

/** 路由 → 顶栏 kicker/标题 映射 */
export const ROUTE_META: Record<string, RouteMeta> = {
  "/": { kicker: "我的阅读空间", title: "书架" },
  "/search": { kicker: "我的阅读空间", title: "搜索" },
  "/library": { kicker: "我的阅读空间", title: "书海" },
  "/sources": { kicker: "源中心", title: "书源" },
  "/rss": { kicker: "我的阅读空间", title: "RSS 订阅" },
  "/purify": { kicker: "偏好与数据", title: "正文净化" },
  "/settings": { kicker: "偏好与数据", title: "设置" },
  "/me": { kicker: "我的数据", title: "我的" },
  "/users": { kicker: "服务器管理", title: "用户管理" },
};

const FALLBACK_META: RouteMeta = { kicker: "我的阅读空间", title: "阅读" };

/** 按路径取顶栏文案, 未收录的路由回落到通用标题 */
export function routeMeta(pathname: string): RouteMeta {
  return ROUTE_META[pathname] ?? FALLBACK_META;
}
