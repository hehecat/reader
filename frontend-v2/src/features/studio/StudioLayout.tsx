import { BookText, Sparkles, Terminal } from "lucide-react";
import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { cn } from "@/lib/cn";
import { motion, PageEnter, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";

const STUDIO_TABS = [
  {
    to: "/studio/sources",
    label: "书源管理",
    eyebrow: "SOURCE CENTER",
    desc: "导入管理书源、连通性测速、Cookie 登录与批量启停。",
    icon: BookText,
    wide: false,
  },
  {
    to: "/studio/workbench",
    label: "规则调试台",
    eyebrow: "SOURCE WORKBENCH",
    desc: "双栏 IDE 级规则编辑与实时 SSE 解析追踪，支持 ⌘S / Ctrl+S 快捷保存。",
    icon: Terminal,
    wide: true,
  },
  {
    to: "/studio/purify",
    label: "正文净化",
    eyebrow: "TEXT PURIFIER",
    desc: "编写正则替换规则并在实时沙盒中对比去水印与广告净化效果。",
    icon: Sparkles,
    wide: false,
  },
] as const;

export default function StudioLayout() {
  const location = useLocation();
  const navigate = useNavigate();
  const reduced = useReducedMotion();

  const currentTab =
    STUDIO_TABS.find((t) => location.pathname.startsWith(t.to)) ?? STUDIO_TABS[0];

  return (
    <PageEnter
      className={cn(
        "mx-auto flex w-full flex-col px-4 pt-3 pb-12 transition-[max-width] duration-200 sm:px-6 md:px-8",
        currentTab.wide ? "max-w-[1440px] gap-4 md:pt-5" : "max-w-7xl gap-4 md:gap-5 md:pt-7",
      )}
    >
      <header
        className={cn(
          "flex flex-col gap-3 border-b border-line/70 md:flex-row md:items-end md:justify-between md:border-line",
          currentTab.wide ? "pb-3" : "pb-3 md:pb-4",
        )}
      >
        <div className="hidden min-w-0 md:block">
          <div className="mb-1 flex items-center gap-2">
            <span className="size-1.5 rounded-full bg-accent" aria-hidden />
            <p className="font-mono text-[11px] font-medium tracking-[0.18em] text-accent uppercase">
              {currentTab.eyebrow}
            </p>
          </div>
          <h1 className="font-serif text-2xl font-semibold tracking-wide text-ink sm:text-[26px]">
            {currentTab.label}
          </h1>
          <p className="mt-1 text-sm text-ink-muted">{currentTab.desc}</p>
        </div>

        <nav
          aria-label="源与规则子导航"
          className="inline-flex w-full shrink-0 items-center gap-5 overflow-x-auto no-scrollbar md:w-fit md:max-w-full md:gap-0.5 md:rounded-xl md:border md:border-line md:bg-subtle/80 md:p-1"
        >
          {STUDIO_TABS.map((tab) => {
            const Icon = tab.icon;
            const isActive = currentTab.to === tab.to;
            return (
              <button
                key={tab.to}
                type="button"
                onClick={() => navigate(tab.to)}
                aria-current={isActive ? "page" : undefined}
                className={cn(
                  "relative z-10 inline-flex shrink-0 cursor-pointer items-center justify-center gap-1.5 py-2 text-[13px] whitespace-nowrap outline-none transition-colors select-none focus-visible:ring-2 focus-visible:ring-accent/60 md:flex-none md:rounded-lg md:px-3 md:py-1.5 md:text-xs md:font-medium",
                  isActive
                    ? "font-semibold text-accent md:text-ink"
                    : "text-ink-muted hover:text-ink-secondary",
                )}
              >
                {isActive ? (
                  <motion.span
                    layoutId="v2-studio-tab"
                    transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                    className="pointer-events-none absolute inset-x-0 bottom-0 h-[2px] rounded-full bg-accent md:inset-0 md:-z-10 md:h-auto md:rounded-lg md:bg-elevated md:shadow-[var(--shadow-sm)] md:ring-1 md:ring-line"
                  />
                ) : null}
                <Icon
                  className={cn(
                    "size-3.5 shrink-0 transition-colors",
                    isActive ? "text-accent" : "opacity-80",
                  )}
                />
                <span>{tab.label}</span>
              </button>
            );
          })}
        </nav>
      </header>

      <div className="min-h-0 flex-1">
        <Outlet />
      </div>
    </PageEnter>
  );
}
