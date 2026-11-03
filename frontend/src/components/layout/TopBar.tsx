import { LogOut, PanelLeft, UserRound } from "lucide-react";
import { useLocation } from "react-router-dom";

import { routeMeta } from "./nav";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  IconButton,
} from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import { THEME_OPTIONS, themeOption } from "@/lib/theme";
import { useSettingsStore, type ThemeMode } from "@/stores/settings-store";

export interface TopBarProps {
  /** 移动端(<md)汉堡按钮打开 slide-over 侧栏 */
  onOpenNav: () => void;
}

/**
 * 全局顶栏(所有断点常驻):
 * 左 = 移动汉堡 + 路由 kicker/衬线标题; 右 = 主题菜单(五档, 写 settings-store) + 账户菜单(退出登录).
 */
export function TopBar({ onOpenNav }: TopBarProps) {
  const location = useLocation();
  const { username, logout } = useAuth();
  const theme = useSettingsStore((state) => state.theme);
  const setTheme = useSettingsStore((state) => state.setTheme);
  const meta = routeMeta(location.pathname);
  const currentTheme = themeOption(theme);

  return (
    <header className="sticky top-0 z-20 flex h-16 shrink-0 items-center justify-between border-b border-border/70 bg-background/90 px-4 backdrop-blur sm:px-8">
      <div className="flex min-w-0 items-center gap-3">
        <IconButton
          variant="ghost"
          aria-label="打开导航"
          className="md:hidden"
          onClick={onOpenNav}
        >
          <PanelLeft aria-hidden />
        </IconButton>
        <div className="min-w-0">
          <p className="truncate text-xs text-muted-foreground">{meta.kicker}</p>
          {/* 顶栏只做面包屑: 页面唯一 h1 在 PageIntro, 这里再放一个大标题会与之抢层级 */}
          <p className="truncate text-sm font-medium">{meta.title}</p>
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-3">
        {/* 五档全露: 只给三档时选了羊皮纸/跟随系统的人看不到自己选了什么; 移动端隐藏(触控目标太小) */}
        <div className="hidden sm:block">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label={`主题: ${currentTheme.label}`}
                className="flex cursor-pointer items-center gap-1.5 rounded-full border border-border bg-surface px-2.5 py-1.5 text-xs font-medium outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <currentTheme.icon className="size-3.5" aria-hidden />
                <span className="hidden sm:inline">{currentTheme.label}</span>
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-40">
              <DropdownMenuRadioGroup value={theme} onValueChange={(value) => setTheme(value as ThemeMode)}>
                {THEME_OPTIONS.map((option) => (
                  <DropdownMenuRadioItem key={option.value} value={option.value}>
                    <option.icon aria-hidden />
                    {option.label}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            {/* 触控热区: 视觉仍 36px(md), after 伪元素把命中区外扩到 44px, 桌面视觉不变 */}
            <IconButton
              variant="ghost"
              aria-label="账户菜单"
              className="relative after:absolute after:-inset-1 after:content-['']"
            >
              <UserRound aria-hidden />
            </IconButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuLabel className="max-w-48 truncate">
              {username ?? "未登录"}
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="danger" onSelect={() => void logout()}>
              <LogOut aria-hidden />
              退出登录
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </header>
  );
}
