import { BookOpen, PanelLeft } from "lucide-react";
import { NavLink } from "react-router-dom";

import { useAppMode } from "@/hooks/useAppMode";
import { ADMIN_NAV_ITEM, NAV_ITEMS, SETTINGS_NAV_ITEM, type NavItem } from "./nav";
import { Tooltip, cn } from "@/components/ui";

export interface SideNavProps {
  /** 折叠时为 64px 图标栏, 项标签改为悬浮提示 */
  collapsed: boolean;
  onToggleCollapse: () => void;
}

export interface SideNavContentProps {
  collapsed: boolean;
  /** 移动端 slide-over 里点完导航项收起抽屉; 桌面侧栏不传 */
  onNavigate?: () => void;
  /** 桌面底部折叠按钮; 不传则不渲染(移动抽屉里没有折叠概念) */
  onToggleCollapse?: () => void;
}

const itemClass =
  "flex items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60";

function SideNavItem({
  item,
  collapsed,
  onNavigate,
}: {
  item: NavItem;
  collapsed: boolean;
  onNavigate?: () => void;
}) {
  const link = (
    <NavLink
      to={item.to}
      end={item.end}
      onClick={onNavigate}
      className={({ isActive }) =>
        cn(
          itemClass,
          collapsed && "justify-center px-0",
          isActive
            ? "bg-accent/10 font-medium text-accent"
            : "text-muted-foreground hover:bg-surface-muted hover:text-foreground",
        )
      }
    >
      <item.icon className="size-4 shrink-0" aria-hidden />
      {collapsed ? null : <span className="truncate">{item.label}</span>}
    </NavLink>
  );

  if (!collapsed) {
    return link;
  }
  return (
    <Tooltip content={item.label} side="right">
      {link}
    </Tooltip>
  );
}

/** 侧栏本体: 品牌块 + 分组导航 + 分隔线 + 设置项 + 折叠按钮, 桌面 aside 与移动 slide-over 共用. */
export function SideNavContent({ collapsed, onNavigate, onToggleCollapse }: SideNavContentProps) {
  // 用户管理仅管理员可见(后端同样校验, 这里只是不暴露入口)
  const { isAdmin } = useAppMode();
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        className={cn(
          "flex h-16 shrink-0 items-center gap-3 border-b border-border/60",
          collapsed ? "justify-center" : "px-4",
        )}
      >
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent text-accent-foreground">
          <BookOpen className="size-4" aria-hidden />
        </span>
        {collapsed ? null : (
          <div className="min-w-0">
            <p className="font-display text-lg leading-none font-semibold">砚台</p>
            <p className="mt-1 text-[10px] tracking-[0.2em] text-muted-foreground">READER DESK</p>
          </div>
        )}
      </div>
      <nav className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-3">
        <p
          className={cn(
            "mb-2 px-3 text-[10px] font-semibold tracking-[0.18em] text-muted-foreground",
            collapsed && "sr-only",
          )}
        >
          阅读空间
        </p>
        {NAV_ITEMS.map((item) => (
          <SideNavItem key={item.to} item={item} collapsed={collapsed} onNavigate={onNavigate} />
        ))}
        <div className="my-3 border-t border-border/60" />
        <SideNavItem item={SETTINGS_NAV_ITEM} collapsed={collapsed} onNavigate={onNavigate} />
        {isAdmin ? (
          <SideNavItem item={ADMIN_NAV_ITEM} collapsed={collapsed} onNavigate={onNavigate} />
        ) : null}
      </nav>
      {onToggleCollapse ? (
        <button
          type="button"
          onClick={onToggleCollapse}
          aria-label={collapsed ? "展开侧边栏" : "收起侧边栏"}
          className="m-3 flex cursor-pointer items-center justify-center rounded-lg border border-border py-2 text-muted-foreground outline-none transition-colors hover:bg-surface-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <PanelLeft className="size-4" aria-hidden />
        </button>
      ) : null}
    </div>
  );
}

/** 桌面侧边导航(>=md): w-56 品牌 + 导航, 可折叠为 w-16 图标栏. */
export function SideNav({ collapsed, onToggleCollapse }: SideNavProps) {
  return (
    <aside
      aria-label="主导航"
      className={cn(
        "ui-safe-x hidden shrink-0 border-r border-border/70 bg-surface/40 transition-all md:flex md:flex-col",
        collapsed ? "w-16" : "w-56",
      )}
    >
      <SideNavContent collapsed={collapsed} onToggleCollapse={onToggleCollapse} />
    </aside>
  );
}
