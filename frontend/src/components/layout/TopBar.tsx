import { Coffee, LogOut, Moon, PanelLeft, Sun, UserRound } from "lucide-react";
import { useLocation } from "react-router-dom";

import { routeMeta } from "./nav";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  IconButton,
  cn,
} from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import { useSettingsStore, type ThemeMode } from "@/stores/settings-store";

/** 顶栏主题胶囊只露三档; sepia/system 仍在设置页里选 */
const THEME_OPTIONS: { key: ThemeMode; label: string; icon: typeof Sun }[] = [
  { key: "light", label: "日间", icon: Sun },
  { key: "green", label: "护眼", icon: Coffee },
  { key: "dark", label: "夜间", icon: Moon },
];

export interface TopBarProps {
  /** 移动端(<md)汉堡按钮打开 slide-over 侧栏 */
  onOpenNav: () => void;
}

/**
 * 全局顶栏(所有断点常驻):
 * 左 = 移动汉堡 + 路由 kicker/衬线标题; 右 = 主题胶囊(写 settings-store) + 账户菜单(退出登录).
 */
export function TopBar({ onOpenNav }: TopBarProps) {
  const location = useLocation();
  const { username, logout } = useAuth();
  const theme = useSettingsStore((state) => state.theme);
  const setTheme = useSettingsStore((state) => state.setTheme);
  const meta = routeMeta(location.pathname);

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
      <div className="flex shrink-0 items-center gap-2">
        <div
          role="group"
          aria-label="主题"
          className="flex items-center gap-1 rounded-full border border-border bg-surface p-0.5"
        >
          {THEME_OPTIONS.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              type="button"
              onClick={() => setTheme(key)}
              aria-pressed={theme === key}
              title={label}
              className={cn(
                "flex cursor-pointer items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                theme === key
                  ? "bg-accent text-accent-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <Icon className="size-3.5" aria-hidden />
              <span className="hidden sm:inline">{label}</span>
            </button>
          ))}
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <IconButton variant="ghost" aria-label="账户菜单">
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
