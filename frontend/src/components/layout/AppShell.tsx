import { useState } from "react";
import { NavLink, Outlet } from "react-router-dom";

import { SideNav, SideNavContent } from "./SideNav";
import { TopBar } from "./TopBar";
import { MOBILE_NAV_ITEMS } from "./nav";
import { Drawer, DrawerContent, DrawerTitle, cn } from "@/components/ui";
import { getJSON, setJSON } from "@/lib/storage";

const NAV_COLLAPSED_KEY = "reader.navCollapsed";

/**
 * 应用主壳(已登录路由的布局路由):
 * - 桌面(>=md): 侧边导航(可折叠) + 顶栏 + 内容区
 * - 移动端(<md): 顶栏 + 内容区 + 底部标签栏(五个高频入口), 汉堡按钮打开 slide-over 侧栏放其余入口
 *
 * /reader 不经过本壳(沉浸式全屏, 自行管理布局).
 */
export default function AppShell() {
  const [collapsed, setCollapsed] = useState<boolean>(
    () => getJSON<boolean>(NAV_COLLAPSED_KEY) ?? false,
  );
  const [navOpen, setNavOpen] = useState(false);

  const toggleCollapsed = (): void => {
    const next = !collapsed;
    setCollapsed(next);
    setJSON(NAV_COLLAPSED_KEY, next);
  };

  return (
    <div className="flex h-full min-h-0 bg-background text-foreground">
      <SideNav collapsed={collapsed} onToggleCollapse={toggleCollapsed} />
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar onOpenNav={() => setNavOpen(true)} />
        {/* pb-14: 让出底部标签栏的高度, 否则每页最后一行被它盖住 */}
        <main className="min-h-0 flex-1 overflow-y-auto pb-14 md:pb-0">
          <Outlet />
        </main>
      </div>
      {/* 移动端 slide-over 侧栏: fixed 面板 + 遮罩, 点完导航项即收起 */}
      <Drawer side="left" open={navOpen} onOpenChange={setNavOpen}>
        <DrawerContent className="w-64 md:w-64">
          <DrawerTitle className="sr-only">主导航</DrawerTitle>
          <SideNavContent collapsed={false} onNavigate={() => setNavOpen(false)} />
        </DrawerContent>
      </Drawer>
      {/* 移动端底部标签栏: 大拇指够得着的高频入口, 不用先点汉堡再选 */}
      <nav
        aria-label="主导航"
        className="ui-safe-b ui-safe-x fixed inset-x-0 bottom-0 z-30 flex h-14 items-stretch border-t border-border/70 bg-background/95 backdrop-blur md:hidden"
      >
        {MOBILE_NAV_ITEMS.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.end}
            className={({ isActive }) =>
              cn(
                "flex flex-1 flex-col items-center justify-center gap-0.5 text-[11px] transition-colors duration-150 ease-out",
                isActive ? "text-accent" : "text-muted-foreground",
              )
            }
          >
            <item.icon aria-hidden className="size-5" />
            {item.label}
          </NavLink>
        ))}
      </nav>
    </div>
  );
}
