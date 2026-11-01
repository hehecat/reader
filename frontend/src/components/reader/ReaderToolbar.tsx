import {
  ArrowLeft,
  Bot,
  AudioLines,
  Bookmark,
  BookmarkCheck,
  BookmarkPlus,
  ChevronLeft,
  ChevronRight,
  Gauge,
  Highlighter,
  LibraryBig,
  List,
  Maximize2,
  Minimize2,
  Moon,
  MoreHorizontal,
  Network,
  Settings2,
  Sun,
} from "lucide-react";
import * as React from "react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  IconButton,
  Slider,
  cn,
} from "@/components/ui";
import { useSettingsStore } from "@/stores/settings-store";
import type { BookChapter } from "@/types/api";

/** 系统深色偏好 (theme=system 时用于白天/夜间快速切换的判定) */
function useSystemDark(): boolean {
  const [dark, setDark] = React.useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  React.useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (event: MediaQueryListEvent) => {
      setDark(event.matches);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);
  return dark;
}

export interface ReaderTopBarProps {
  bookName: string;
  /** 当前章节标题 */
  chapterTitle: string;
  /** 本章阅读进度百分比 (0-100), 驱动栏底进度条 */
  percent: number;
  onBack: () => void;
  /** 本章是否已有书签 */
  bookmarked: boolean;
  onToggleBookmark: () => void;
  /** 本书是否在书架上 */
  inShelf: boolean;
  /** 书架归属未知(列表加载中)或请求中: 禁用开关, 免得误存覆盖进度 */
  shelfDisabled: boolean;
  onToggleShelf: () => void;
  /** 打开批注抽屉 */
  onOpenAnnotations: () => void;
  /** 浏览器没有 Web Speech API 时禁用朗读开关, tooltip 说明原因 */
  ttsSupported: boolean;
  /** 朗读中或暂停中 (顶栏图标显示激活态) */
  ttsActive: boolean;
  onToggleTts: () => void;
  /** 自动滚动只对滚动模式有意义, 翻页模式隐藏该开关 */
  showAutoScroll: boolean;
  autoScroll: boolean;
  onToggleAutoScroll: () => void;
  /** 沉浸阅读 (Fullscreen API, 失败回落为覆盖式全屏) */
  immersive: boolean;
  onToggleImmersive: () => void;
  onOpenToc: () => void;
  onOpenSettings: () => void;
  /** AI 阅读助手入口: 仅在设置开启时由页面传入, 未传则不渲染按钮 */
  onOpenAi?: () => void;
  /** AI 面板当前展开(按钮高亮) */
  aiOpen?: boolean;
  /** 点击正文隐藏工具条时: 顶栏向上收起 (内容随 flex 自适应扩展) */
  chromeHidden?: boolean;
}

/**
 * 阅读器顶栏 (常显): 返回 / 目录 / 书名 + 章节 / 书签 / 书架 / 更多 / 设置,
 * 栏底一条 2px 进度条随本章阅读比例增长.
 * 批注 / 朗读 / 自动滚动 / 沉浸 / AI 这些「偶尔切一次」的动作收进「更多」菜单:
 * 顶栏只留导航 + 书级写入 + 设置, 小屏不必再靠 gap 归零硬挤 10 个图标.
 */
export function ReaderTopBar({
  bookName,
  chapterTitle,
  percent,
  onBack,
  bookmarked,
  onToggleBookmark,
  inShelf,
  shelfDisabled,
  onToggleShelf,
  onOpenAnnotations,
  ttsSupported,
  ttsActive,
  onToggleTts,
  showAutoScroll,
  autoScroll,
  onToggleAutoScroll,
  immersive,
  onToggleImmersive,
  onOpenToc,
  onOpenSettings,
  onOpenAi,
  aiOpen = false,
  chromeHidden = false,
}: ReaderTopBarProps) {
  return (
    <header
      aria-hidden={chromeHidden}
      inert={chromeHidden}
      className={cn(
        "ui-safe-t ui-safe-x sticky top-0 z-30 shrink-0 overflow-hidden border-b border-border/70 bg-background/85 backdrop-blur",
        // 点正文收起/展开: 高度与位移同时过渡, 内容区随之占满全屏(遵守系统减动效偏好)
        "transition-[height,transform,opacity] duration-300 ease-out motion-reduce:transition-none",
        chromeHidden ? "h-0 -translate-y-full border-transparent opacity-0" : "h-14",
      )}
    >
      {/* 390px: 6 个按钮 + 书名, 常规间距下已不溢出; 小屏仍收紧一档间距, 把余量留给书名 */}
      <div className="mx-auto flex h-14 max-w-3xl items-center gap-0 px-1 sm:gap-2 sm:px-4">
        <IconButton variant="ghost" tooltip="返回" tooltipSide="bottom" onClick={onBack}>
          <ArrowLeft />
        </IconButton>

        {/* 目录: 抽屉从左侧滑出, 触发器就放在同侧(挨着返回) —— 高频动作, 不跟设置挤在最右.
            图标数没变, 小屏 368px 的宽度预算不变 */}
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="目录"
          tooltipSide="bottom"
          onClick={onOpenToc}
        >
          <List />
        </IconButton>

        {/* 小屏 gap 归零后, 书名和右侧第一个图标会贴死, 这里留 4px 呼吸 */}
        <div className="min-w-0 flex-1 pr-1">
          <p className="truncate font-display text-sm font-semibold">{bookName}</p>
          <p className="truncate text-xs text-muted-foreground">{chapterTitle}</p>
        </div>

        <IconButton
          variant="ghost"
          size="sm"
          tooltip={bookmarked ? "移除本章书签" : "加入书签"}
          tooltipSide="bottom"
          className={cn(bookmarked && "text-accent")}
          onClick={onToggleBookmark}
        >
          {bookmarked ? <BookmarkCheck /> : <BookmarkPlus />}
        </IconButton>
        <IconButton
          variant="ghost"
          size="sm"
          tooltip={inShelf ? "移出书架" : "加入书架"}
          tooltipSide="bottom"
          disabled={shelfDisabled}
          className={cn(inShelf && "text-accent")}
          onClick={onToggleShelf}
        >
          <LibraryBig />
        </IconButton>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <IconButton
              variant="ghost"
              size="sm"
              aria-label="更多操作"
              className={cn(
                (ttsActive || autoScroll || immersive || aiOpen) && "text-accent",
              )}
            >
              <MoreHorizontal />
            </IconButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-44">
            <DropdownMenuItem onSelect={onOpenAnnotations}>
              <Highlighter />
              批注
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!ttsSupported} onSelect={onToggleTts}>
              <AudioLines />
              {ttsSupported ? (ttsActive ? "停止朗读" : "朗读") : "朗读(浏览器不支持)"}
            </DropdownMenuItem>
            {showAutoScroll ? (
              <DropdownMenuItem onSelect={onToggleAutoScroll}>
                <Gauge />
                {autoScroll ? "停止自动滚动" : "自动滚动"}
              </DropdownMenuItem>
            ) : null}
            <DropdownMenuItem onSelect={onToggleImmersive}>
              {immersive ? <Minimize2 /> : <Maximize2 />}
              {immersive ? "退出沉浸阅读" : "沉浸阅读"}
            </DropdownMenuItem>
            {onOpenAi === undefined ? null : (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={onOpenAi}>
                  <Bot />
                  AI 阅读助手
                </DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="阅读设置"
          tooltipSide="bottom"
          onClick={onOpenSettings}
        >
          <Settings2 />
        </IconButton>
      </div>

      {/* 栏底进度条: 滚动模式 = 本章滚动比例, 翻页模式 = 当前页占比 */}
      <div
        role="progressbar"
        aria-label="本章阅读进度"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        className="h-0.5"
      >
        <div
          className="h-full bg-accent transition-[width] duration-150 ease-out"
          style={{ width: `${percent}%` }}
        />
      </div>
    </header>
  );
}

export interface ReaderChapterBarProps {
  chapters: BookChapter[];
  /** 当前章节索引 */
  index: number;
  /** 上一章/下一章/滑块拖动切章 (越界 toast 由进度 hook 处理) */
  onGoToChapter: (index: number) => void;
  /** 打开书签抽屉 */
  onOpenBookmarks: () => void;
  /** 打开「关系图谱」生成弹窗 (整本批量生成入口, 见 GraphGenerateDialog) */
  onOpenGraph: () => void;
  /** 点击正文隐藏工具条时: 底栏向下收起 */
  chromeHidden?: boolean;
}

/**
 * 阅读器底栏 (常显): 上一章 / 章节滑块 / 下一章 / 书签列表 / 关系图谱 / 日夜切换.
 * 拖动滑块时在栏上方浮出目标章节名, 松手才真正切章.
 */
export function ReaderChapterBar({
  chapters,
  index,
  onGoToChapter,
  onOpenBookmarks,
  onOpenGraph,
  chromeHidden = false,
}: ReaderChapterBarProps) {
  const theme = useSettingsStore((state) => state.theme);
  const setTheme = useSettingsStore((state) => state.setTheme);
  const systemDark = useSystemDark();
  const isDark = theme === "dark" || (theme === "system" && systemDark);
  // tooltip 与无障碍名称同文: IconButton 会用文本 tooltip 兜底 aria-label, 不必两处各写一遍
  const themeLabel = isDark ? "切换到白天模式" : "切换到夜间模式";

  // 滑块拖动中的草稿索引 (松手才真正切章)
  const [dragIndex, setDragIndex] = React.useState<number | null>(null);

  const chapterCount = chapters.length;
  const activeChapter =
    dragIndex !== null && dragIndex >= 0 && dragIndex < chapterCount
      ? chapters[dragIndex]
      : undefined;

  const handleSliderChange = React.useCallback((values: number[]) => {
    const next = values[0];
    if (next !== undefined) {
      setDragIndex(next);
    }
  }, []);

  const handleSliderCommit = React.useCallback(
    (values: number[]) => {
      const next = values[0];
      setDragIndex(null);
      if (next !== undefined) {
        onGoToChapter(next);
      }
    },
    [onGoToChapter],
  );

  return (
    <footer
      aria-hidden={chromeHidden}
      inert={chromeHidden}
      className={cn(
        "ui-safe-b ui-safe-x relative z-30 shrink-0 border-t border-border/70 bg-background/85 backdrop-blur",
        chromeHidden && "border-transparent",
      )}
    >
      {activeChapter === undefined ? null : (
        <p className="pointer-events-none absolute bottom-full left-1/2 mb-2 w-max max-w-[80vw] -translate-x-1/2 truncate rounded-md bg-foreground px-2.5 py-1 text-xs text-background shadow-lg">
          {activeChapter.title}
        </p>
      )}
      <div
        className={cn(
          "overflow-hidden transition-[max-height,opacity] duration-300 ease-out motion-reduce:transition-none",
          chromeHidden ? "max-h-0 opacity-0" : "max-h-32",
        )}
      >
        <div className="mx-auto flex max-w-3xl items-center gap-1 px-2 py-1.5">
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="上一章"
          onClick={() => onGoToChapter(index - 1)}
        >
          <ChevronLeft />
        </IconButton>
        <Slider
          className="flex-1"
          aria-label="章节进度"
          min={0}
          max={Math.max(0, chapterCount - 1)}
          step={1}
          disabled={chapterCount <= 1}
          showValue={false}
          value={[Math.min(Math.max(index, 0), Math.max(0, chapterCount - 1))]}
          onValueChange={handleSliderChange}
          onValueCommit={handleSliderCommit}
        />
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="下一章"
          onClick={() => onGoToChapter(index + 1)}
        >
          <ChevronRight />
        </IconButton>
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="书签列表"
          onClick={onOpenBookmarks}
        >
          <Bookmark />
        </IconButton>
        {/* 关系图谱入口: 顶栏图标已排满(390px 就到边了), 底栏还有横向余量, 放这里不挤书名 */}
        <IconButton
          variant="ghost"
          size="sm"
          tooltip="关系图谱"
          onClick={onOpenGraph}
        >
          <Network />
        </IconButton>
        <IconButton
          variant="ghost"
          size="sm"
          tooltip={themeLabel}
          onClick={() => setTheme(isDark ? "light" : "dark")}
        >
          {isDark ? <Sun /> : <Moon />}
          </IconButton>
        </div>
      </div>
    </footer>
  );
}
