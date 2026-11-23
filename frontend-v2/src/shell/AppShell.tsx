import { useQueryClient } from "@tanstack/react-query";
import {
  BookOpen,
  BookText,
  Check,
  Compass,
  LogOut,
  PanelLeft,
  PanelLeftClose,
  PanelLeftOpen,
  Search,
  Settings2,
  Shield,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  UserRound,
  UsersRound,
  type LucideIcon,
} from "lucide-react";
import * as React from "react";
import { Link, Outlet, useLocation, useNavigate } from "react-router-dom";
import { cn } from "@/lib/cn";
import { clearReaderOrigin, recordReaderOrigin } from "@/lib/readerOrigin";
import { useAuthStore } from "@/stores/auth";
import { resolvePaperTheme, usePrefsStore } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { IconButton } from "@/ui/Button";
import { Switch } from "@/ui/Controls";
import { motion, SPRING_LIQUID, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";
import { Popover, PopoverDivider, PopoverItem } from "@/ui/Popover";
import { Sheet } from "@/ui/Sheet";
import { CommandPalette, PAPER_SWATCHES } from "./CommandPalette";

const NAV_COLLAPSED_KEY = "reader.navCollapsed";

interface NavItem {
  key: string;
  label: string;
  to: string;
  icon: LucideIcon;
  match: (pathname: string, search: string) => boolean;
}

const PRIMARY_NAV: readonly NavItem[] = [
  {
    key: "shelf",
    label: "书架",
    to: "/",
    icon: BookOpen,
    match: (p) => p === "/",
  },
  {
    key: "search",
    label: "搜索",
    to: "/discover",
    icon: Search,
    match: (p) => p === "/discover" || p.startsWith("/discover/"),
  },
  {
    key: "explore",
    label: "书海",
    to: "/explore",
    icon: Compass,
    match: (p) => p === "/explore" || p.startsWith("/explore/"),
  },
  {
    key: "sources",
    label: "书源",
    to: "/studio/sources",
    icon: BookText,
    match: (p) => p === "/studio/sources" || p.startsWith("/studio/sources/"),
  },
  {
    key: "me",
    label: "我的",
    to: "/me",
    icon: UserRound,
    match: (p) => p === "/me" || p.startsWith("/me/"),
  },
];

const SECONDARY_NAV: readonly NavItem[] = [
  {
    key: "purify",
    label: "正文净化",
    to: "/studio/purify",
    icon: Sparkles,
    match: (p) => p.startsWith("/studio/purify"),
  },
  {
    key: "workbench",
    label: "规则调试",
    to: "/studio/workbench",
    icon: Terminal,
    match: (p) => p.startsWith("/studio/workbench"),
  },
  {
    key: "settings",
    label: "设置",
    to: "/settings",
    icon: Settings2,
    match: (p, s) => p === "/settings" && !s.includes("tab=account"),
  },
];

const ADMIN_NAV: NavItem = {
  key: "users",
  label: "用户管理",
  to: "/settings?tab=account",
  icon: UsersRound,
  match: (p, s) => p === "/settings" && s.includes("tab=account"),
};

function resolveRouteMeta(pathname: string, search: string): { kicker: string; title: string } {
  if (pathname === "/") return { kicker: "我的阅读空间", title: "书架" };
  if (pathname.startsWith("/discover")) return { kicker: "我的阅读空间", title: "搜索" };
  if (pathname.startsWith("/explore")) return { kicker: "我的阅读空间", title: "书海" };
  if (pathname.startsWith("/studio/sources")) return { kicker: "源中心", title: "书源" };
  if (pathname.startsWith("/studio/workbench")) return { kicker: "源中心", title: "规则调试台" };
  if (pathname.startsWith("/studio/purify")) return { kicker: "偏好与数据", title: "正文净化" };
  if (pathname.startsWith("/me")) return { kicker: "我的数据", title: "我的" };
  if (pathname.startsWith("/settings")) {
    if (search.includes("tab=account")) return { kicker: "服务器管理", title: "账号与用户" };
    return { kicker: "偏好与数据", title: "设置" };
  }
  return { kicker: "我的阅读空间", title: "阅读" };
}

function SideNavEntry({
  item,
  active,
  collapsed,
  layoutNamespace,
  onNavigate,
}: {
  item: NavItem;
  active: boolean;
  collapsed: boolean;
  layoutNamespace: string;
  onNavigate?: () => void;
}) {
  const reduced = useReducedMotion();
  const Icon = item.icon;
  return (
    <Link
      to={item.to}
      title={collapsed ? item.label : undefined}
      aria-label={collapsed ? item.label : undefined}
      aria-current={active ? "page" : undefined}
      onClick={onNavigate}
      className={cn(
        "group/nav relative z-10 flex items-center rounded-xl text-[13.5px] outline-none transition-colors duration-150 select-none focus-visible:ring-2 focus-visible:ring-accent/55",
        collapsed ? "mx-auto size-10 justify-center px-0 py-0" : "gap-3 px-3 py-2",
        active
          ? "font-medium text-accent"
          : "text-ink-muted hover:bg-subtle/80 hover:text-ink active:bg-hover/70",
      )}
    >
      {active ? (
        <motion.span
          layoutId={layoutNamespace}
          transition={reduced ? { duration: 0 } : SPRING_LIQUID}
          className="pointer-events-none absolute inset-0 -z-10 rounded-xl bg-accent-soft ring-1 ring-accent/15"
        />
      ) : null}
      {!collapsed && active ? (
        <span
          aria-hidden
          className="pointer-events-none absolute top-2 bottom-2 left-0.5 w-[2.5px] rounded-full bg-accent"
        />
      ) : null}
      <Icon
        className={cn(
          "shrink-0 transition-transform duration-150 group-hover/nav:scale-[1.05]",
          collapsed ? "size-[18px]" : "size-4",
        )}
        aria-hidden
      />
      <span
        className={cn(
          "truncate transition-[opacity,max-width] duration-200 ease-[cubic-bezier(0.22,1,0.36,1)]",
          collapsed ? "pointer-events-none max-w-0 opacity-0" : "max-w-[140px] opacity-100",
        )}
        aria-hidden={collapsed || undefined}
      >
        {item.label}
      </span>
    </Link>
  );
}

function SideNavContent({
  collapsed,
  layoutNamespace,
  onNavigate,
  onToggleCollapse,
}: {
  collapsed: boolean;
  layoutNamespace: string;
  onNavigate?: () => void;
  onToggleCollapse?: () => void;
}) {
  const { pathname, search } = useLocation();
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);
  const showAdmin = Boolean(system?.secure && user?.isAdmin);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {/* Brand Block: 砚台 · READER DESK */}
      <Link
        to="/"
        onClick={onNavigate}
        className={cn(
          "group/brand flex h-15 shrink-0 items-center border-b border-line outline-none transition-colors hover:bg-subtle/40 focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:ring-inset",
          collapsed ? "justify-center px-0" : "gap-3 px-4",
        )}
      >
        <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-accent text-accent-ink shadow-[var(--shadow-primary)] transition-transform duration-200 group-hover/brand:scale-[1.03]">
          <BookOpen className="size-4" aria-hidden />
        </span>
        {collapsed ? null : (
          <div className="min-w-0">
            <p className="font-serif text-[18px] leading-none font-semibold tracking-wide text-ink">
              砚台
            </p>
            <p className="mt-1 text-[9.5px] font-medium tracking-[0.22em] text-ink-muted">
              READER DESK
            </p>
          </div>
        )}
      </Link>

      {/* Navigation Groups */}
      <nav
        aria-label="侧栏菜单"
        className={cn(
          "flex min-h-0 flex-1 flex-col overflow-y-auto overscroll-contain",
          collapsed ? "gap-1.5 px-2 py-3" : "gap-0.5 px-3 py-3",
        )}
      >
        <p
          className={cn(
            "mb-1.5 px-3 text-[10px] font-semibold tracking-[0.18em] text-ink-muted/85 select-none",
            collapsed && "sr-only",
          )}
        >
          阅读空间
        </p>

        {PRIMARY_NAV.map((item) => (
          <SideNavEntry
            key={item.key}
            item={item}
            active={item.match(pathname, search)}
            collapsed={collapsed}
            layoutNamespace={layoutNamespace}
            onNavigate={onNavigate}
          />
        ))}

        <div className={cn("my-2.5 border-t border-line/80", collapsed ? "mx-1.5" : "mx-1")} />

        <p
          className={cn(
            "mb-1.5 px-3 text-[10px] font-semibold tracking-[0.18em] text-ink-muted/85 select-none",
            collapsed && "sr-only",
          )}
        >
          工坊与偏好
        </p>

        {SECONDARY_NAV.map((item) => (
          <SideNavEntry
            key={item.key}
            item={item}
            active={item.match(pathname, search)}
            collapsed={collapsed}
            layoutNamespace={layoutNamespace}
            onNavigate={onNavigate}
          />
        ))}

        {showAdmin ? (
          <SideNavEntry
            item={ADMIN_NAV}
            active={ADMIN_NAV.match(pathname, search)}
            collapsed={collapsed}
            layoutNamespace={layoutNamespace}
            onNavigate={onNavigate}
          />
        ) : null}
      </nav>

      {/* Collapse Button */}
      {onToggleCollapse ? (
        <div className="shrink-0 border-t border-line/70 p-2.5">
          <button
            type="button"
            onClick={onToggleCollapse}
            title={collapsed ? "展开侧边栏 (⌘\\)" : "收起侧边栏 (⌘\\)"}
            aria-label={collapsed ? "展开侧边栏" : "收起侧边栏"}
            aria-expanded={!collapsed}
            className={cn(
              "flex w-full cursor-pointer items-center rounded-xl border border-line/90 bg-elevated/70 text-xs text-ink-muted outline-none transition-colors hover:border-line-strong hover:bg-subtle hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 active:bg-hover",
              collapsed ? "h-9 justify-center px-0" : "h-9 justify-between px-3",
            )}
          >
            <span className="flex min-w-0 items-center gap-2">
              {collapsed ? (
                <PanelLeftOpen className="size-4 shrink-0" aria-hidden />
              ) : (
                <PanelLeftClose className="size-4 shrink-0" aria-hidden />
              )}
              {collapsed ? null : <span className="truncate font-medium">收起侧栏</span>}
            </span>
            {collapsed ? null : <kbd className="kbd-chip shrink-0">⌘\</kbd>}
          </button>
        </div>
      ) : null}
    </div>
  );
}

function ThemeMenuPopover() {
  const theme = usePrefsStore((s) => s.theme);
  const setTheme = usePrefsStore((s) => s.setTheme);
  const [resolved, setResolved] = React.useState(() => resolvePaperTheme(theme));

  React.useEffect(() => {
    const update = () => setResolved(resolvePaperTheme(theme));
    update();
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, [theme]);

  const currentSwatch =
    PAPER_SWATCHES.find((sw) => sw.id === resolved) ?? PAPER_SWATCHES[0]!;

  return (
    <Popover
      align="end"
      className="w-[244px] p-3"
      trigger={
        <button
          type="button"
          aria-label={`主题：${theme === "system" ? "跟随系统" : currentSwatch.name}`}
          className="inline-flex h-8.5 cursor-pointer items-center gap-2 rounded-full border border-line bg-elevated px-2.5 text-xs font-medium text-ink-secondary shadow-[var(--shadow-sm)] transition-[border-color,color,background-color] duration-150 hover:border-line-strong hover:bg-subtle/60 hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:outline-none"
        >
          <span
            style={{ backgroundColor: currentSwatch.bg, borderColor: currentSwatch.border }}
            className="flex size-3.5 items-center justify-center rounded-full border shadow-[inset_0_0_0_0.5px_rgba(0,0,0,0.06)]"
          >
            <span
              style={{ backgroundColor: currentSwatch.accent }}
              className="size-1.5 rounded-full"
            />
          </span>
          <span className="hidden sm:inline">
            {theme === "system" ? "自动" : currentSwatch.name}
          </span>
        </button>
      }
    >
      <div className="mb-2.5 flex items-center justify-between px-0.5">
        <span className="text-[11px] font-semibold tracking-[0.08em] text-ink-muted">纸墨外观</span>
        <span className="text-[11px] font-medium text-ink-secondary">
          {theme === "system" ? `自动 · ${currentSwatch.name}` : currentSwatch.name}
        </span>
      </div>

      <div className="grid grid-cols-2 gap-2">
        {PAPER_SWATCHES.map((sw) => {
          const selected = theme === sw.id;
          return (
            <button
              key={sw.id}
              type="button"
              onClick={() => setTheme(sw.id)}
              aria-pressed={selected}
              style={{
                backgroundColor: sw.bg,
                borderColor: selected ? sw.accent : sw.border,
                color: sw.ink,
              }}
              className={cn(
                "group/swatch relative flex cursor-pointer flex-col items-start gap-1.5 rounded-xl border p-2.5 text-left transition-[transform,box-shadow] duration-150 hover:scale-[1.02] active:scale-[0.98]",
                selected && "ring-2 ring-accent/35",
              )}
            >
              <div className="flex w-full items-center justify-between">
                <span className="font-serif text-[13px] font-semibold leading-none">{sw.name}</span>
                {selected ? (
                  <span
                    style={{ backgroundColor: sw.accent }}
                    className="flex size-4 items-center justify-center rounded-full text-white"
                  >
                    <Check className="size-2.5" strokeWidth={3} />
                  </span>
                ) : (
                  <span
                    style={{ backgroundColor: sw.accent }}
                    className="size-2 rounded-full opacity-75"
                  />
                )}
              </div>
              {/* 迷你纸面墨行预览 */}
              <div className="flex w-full flex-col gap-1 opacity-45">
                <span style={{ backgroundColor: sw.ink }} className="h-[2px] w-4/5 rounded-full" />
                <span style={{ backgroundColor: sw.ink }} className="h-[2px] w-3/5 rounded-full" />
              </div>
              <span className="text-[10px] leading-none opacity-65">{sw.desc}</span>
            </button>
          );
        })}
      </div>

      <PopoverDivider />

      <div className="flex items-center justify-between gap-2 px-1 py-0.5">
        <div className="min-w-0">
          <p className="text-xs font-medium text-ink">跟随系统浅深色</p>
          <p className="text-[10.5px] text-ink-muted">日夜自动切换宣纸与玄墨</p>
        </div>
        <Switch
          size="sm"
          checked={theme === "system"}
          ariaLabel="跟随系统外观"
          onChange={(checked) => {
            if (checked) setTheme("system");
            else setTheme(resolvePaperTheme("system"));
          }}
        />
      </div>
    </Popover>
  );
}

function AccountPopover() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const user = useAuthStore((s) => s.user);
  const system = useAuthStore((s) => s.system);
  const logout = useAuthStore((s) => s.logout);

  const isMultiUser = system?.secure === true;
  const displayName = isMultiUser && user ? user.username : "本地模式";
  const initial = isMultiUser && user?.username ? user.username.slice(0, 1).toUpperCase() : "";

  const handleLogout = async (close: () => void) => {
    close();
    clearReaderOrigin();
    await logout();
    queryClient.clear();
    notify.info("已退出登录");
    navigate("/login", { replace: true });
  };

  return (
    <Popover
      align="end"
      className="w-[220px] p-1.5"
      trigger={
        <IconButton
          size="sm"
          aria-label="账户菜单"
          className="border border-line bg-elevated shadow-[var(--shadow-sm)] hover:border-line-strong"
        >
          {initial ? (
            <span className="font-serif text-xs font-semibold text-ink">{initial}</span>
          ) : (
            <UserRound className="size-4" />
          )}
        </IconButton>
      }
    >
      {(close) => (
        <>
          <div className="flex items-center gap-2.5 px-2.5 py-2">
            <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-accent-soft font-serif text-xs font-semibold text-accent">
              {initial || <UserRound className="size-4" />}
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5">
                <p className="truncate text-[13px] font-medium text-ink">{displayName}</p>
                {isMultiUser && user?.isAdmin ? (
                  <span className="inline-flex shrink-0 items-center gap-0.5 rounded-full bg-accent-soft px-1.5 py-0.5 text-[10px] font-medium text-accent">
                    <Shield className="size-2.5" />
                    管理
                  </span>
                ) : null}
              </div>
              <p className="mt-0.5 truncate text-[11px] text-ink-muted">
                {isMultiUser ? "多用户空间" : "单用户本地模式"}
                {system?.version ? ` · v${system.version}` : ""}
              </p>
            </div>
          </div>

          <PopoverDivider />

          <PopoverItem
            icon={<SlidersHorizontal className="size-4" />}
            onClick={() => {
              close();
              navigate("/settings");
            }}
          >
            偏好设置
          </PopoverItem>

          <PopoverItem
            icon={<UserRound className="size-4" />}
            onClick={() => {
              close();
              navigate("/me");
            }}
          >
            我的数据
          </PopoverItem>

          {isMultiUser && user ? (
            <>
              <PopoverDivider />
              <PopoverItem
                danger
                icon={<LogOut className="size-4" />}
                onClick={() => void handleLogout(close)}
              >
                退出登录
              </PopoverItem>
            </>
          ) : null}
        </>
      )}
    </Popover>
  );
}

export function AppShell() {
  const { pathname, search } = useLocation();
  const reduced = useReducedMotion();
  const [paletteOpen, setPaletteOpen] = React.useState(false);
  const [mobileNavOpen, setMobileNavOpen] = React.useState(false);
  const [scrolled, setScrolled] = React.useState(false);
  const mainRef = React.useRef<HTMLElement>(null);

  React.useEffect(() => {
    recordReaderOrigin(pathname, search);
  }, [pathname, search]);

  // 路由切换时归位滚动状态，避免旧页滚动阴影残留
  React.useEffect(() => {
    setScrolled(false);
    if (mainRef.current) {
      mainRef.current.scrollTop = 0;
    }
  }, [pathname, search]);

  const [collapsed, setCollapsed] = React.useState<boolean>(() => {
    try {
      return localStorage.getItem(NAV_COLLAPSED_KEY) === "true";
    } catch {
      return false;
    }
  });

  const toggleCollapsed = React.useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(NAV_COLLAPSED_KEY, String(next));
      } catch {
        // ignore
      }
      return next;
    });
  }, []);

  // 全局 ⌘K 搜索面板 & ⌘\ 折叠侧边栏
  React.useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((prev) => !prev);
      } else if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key === "\\") {
        e.preventDefault();
        toggleCollapsed();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [toggleCollapsed]);

  const meta = resolveRouteMeta(pathname, search);

  return (
    <div
      data-vaul-canvas="true"
      className="flex h-dvh min-h-0 overflow-hidden bg-bg text-ink"
    >
      {/* Desktop Collapsible Left SideNav (>=md) */}
      <aside
        aria-label="主导航"
        className={cn(
          "hidden shrink-0 border-r border-line bg-bg transition-[width] duration-250 ease-[cubic-bezier(0.22,1,0.36,1)] md:flex md:flex-col",
          collapsed ? "w-16" : "w-56",
        )}
      >
        <SideNavContent
          collapsed={collapsed}
          layoutNamespace="v2-desktop-sidenav"
          onToggleCollapse={toggleCollapsed}
        />
      </aside>

      {/* Right Main Column: Sticky TopBar + Independent Scrollable Content */}
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        <header
          className={cn(
            "safe-t sticky top-0 z-30 shrink-0 border-b backdrop-blur-md transition-[background-color,border-color,box-shadow] duration-200",
            scrolled
              ? "border-line bg-bg/92 shadow-[0_6px_18px_-14px_rgba(0,0,0,0.28)]"
              : "border-line/75 bg-bg/85",
          )}
        >
          <div className="flex h-14 items-center justify-between gap-2 px-3 sm:h-15 sm:gap-3 sm:px-7">
            <div className="flex min-w-0 items-center gap-2.5 sm:gap-3">
              <IconButton
                size="sm"
                aria-label="打开导航菜单"
                className="md:hidden"
                onClick={() => setMobileNavOpen(true)}
              >
                <PanelLeft className="size-4" />
              </IconButton>
              <div className="min-w-0">
                <p className="truncate text-[10.5px] font-medium tracking-[0.14em] text-ink-muted">
                  {meta.kicker}
                </p>
                <h1 className="truncate font-serif text-[16px] leading-snug font-semibold tracking-tight text-ink sm:text-[17px]">
                  {meta.title}
                </h1>
              </div>
            </div>

            <div className="flex shrink-0 items-center gap-1.5 sm:gap-2">
              <button
                type="button"
                onClick={() => setPaletteOpen(true)}
                aria-label="打开快捷搜索（⌘K）"
                className="group/cmd inline-flex size-8.5 cursor-pointer items-center justify-center gap-2 rounded-full border border-line bg-elevated text-xs text-ink-muted shadow-[var(--shadow-sm)] transition-[border-color,color,background-color] duration-150 hover:border-line-strong hover:bg-subtle/60 hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/55 focus-visible:outline-none sm:w-auto sm:px-3"
              >
                <Search className="size-3.5 shrink-0 transition-colors group-hover/cmd:text-accent" />
                <span className="hidden lg:inline">快速跳转…</span>
                <kbd className="kbd-chip ml-0.5 hidden sm:inline-flex">⌘K</kbd>
              </button>

              <ThemeMenuPopover />
              <AccountPopover />
            </div>
          </div>
        </header>

        <main
          ref={mainRef}
          id="main"
          onScroll={(e) => {
            const next = e.currentTarget.scrollTop > 8;
            if (next !== scrolled) setScrolled(next);
          }}
          className="min-h-0 flex-1 overflow-y-auto overscroll-contain pb-[calc(3.75rem+env(safe-area-inset-bottom,0px))] md:pb-0"
        >
          <Outlet />
        </main>
      </div>

      {/* Mobile Slide-Over SideNav Sheet */}
      <Sheet
        open={mobileNavOpen}
        onClose={() => setMobileNavOpen(false)}
        side="left"
        size="sm"
        bodyClassName="p-0"
      >
        <div className="flex h-full flex-col">
          <SideNavContent
            collapsed={false}
            layoutNamespace="v2-mobile-drawer-nav"
            onNavigate={() => setMobileNavOpen(false)}
          />
        </div>
      </Sheet>

      {/* Mobile Bottom 5-Tab Bar (<md) — 内层固定 h-14，外层承接底部安全区，避免 iPhone 底部横条挤压图标 */}
      <nav
        aria-label="底部导航"
        className="safe-b fixed inset-x-0 bottom-0 z-30 border-t border-line bg-bg/92 shadow-[0_-6px_20px_-14px_rgba(0,0,0,0.2)] backdrop-blur-lg md:hidden"
      >
        <div className="flex h-14 items-stretch px-1.5">
          {PRIMARY_NAV.map((item) => {
            const active = item.match(pathname, search);
            const Icon = item.icon;
            return (
              <Link
                key={item.key}
                to={item.to}
                aria-current={active ? "page" : undefined}
                className={cn(
                  "relative flex flex-1 flex-col items-center justify-center gap-0.5 rounded-xl text-[10.5px] transition-colors select-none active:scale-95",
                  active ? "font-semibold text-accent" : "text-ink-muted hover:text-ink-secondary",
                )}
              >
                <span className="relative flex h-7 w-12 items-center justify-center rounded-full">
                  {active ? (
                    <motion.span
                      layoutId="v2-mobile-bottom-tab"
                      transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                      className="pointer-events-none absolute inset-0 rounded-full bg-accent-soft"
                    />
                  ) : null}
                  <Icon className="relative z-10 size-[19px]" strokeWidth={active ? 2.2 : 1.8} aria-hidden />
                </span>
                <span className="leading-none">{item.label}</span>
              </Link>
            );
          })}
        </div>
      </nav>

      {/* Global ⌘K Spotlight */}
      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />
    </div>
  );
}
