import {
  ArrowLeft,
  ArrowLeftRight,
  Bookmark,
  BookOpen,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Columns2,
  Headphones,
  Highlighter,
  Keyboard,
  List,
  Maximize2,
  Minimize2,
  Minus,
  MoreHorizontal,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  ScrollText,
  Square,
  X,
} from "lucide-react";
import * as React from "react";
import type { BookChapter, CustomFont } from "@/api/types";
import { cn } from "@/lib/cn";
import { cleanChapterTitle } from "@/lib/format";
import { ensureReaderFontLoaded, READER_FONT_PRESETS } from "@/lib/readerFonts";
import {
  resolvePaperTheme,
  usePrefsStore,
  type PageTurnMode,
  type PaperTheme,
  type ReadLayout,
  type SpreadMode,
} from "@/stores/prefs";
import { IconButton } from "@/ui/Button";
import { InlineConfirm, Segmented, Slider, Switch } from "@/ui/Controls";
import { Popover, PopoverDivider, PopoverItem } from "@/ui/Popover";
import {
  AnimatePresence,
  motion,
  SPRING_POP,
  SPRING_SHEET,
  SPRING_SNAPPY,
  useReducedMotion,
} from "@/ui/motion";

export interface ReaderHudProps {
  hudOpen: boolean;
  workbenchOpen: boolean;
  onToggleWorkbench: () => void;
  onCloseWorkbench: () => void;
  onBack: () => void;
  backLabel?: string;
  bookName: string;
  chapterTitle: string;
  chapterIndex: number;
  chapters: BookChapter[];
  onJumpChapter: (index: number) => void;
  onPrevChapter: () => void;
  onNextChapter: () => void;

  // Progress readout
  page: number;
  totalPages: number;
  chapterRatio: number;

  // Shelf & Bookmark
  isInShelf: boolean;
  savingShelf: boolean;
  onToggleShelf: () => void;
  isCurrentBookmarked: boolean;
  onQuickBookmark: () => void;

  // Drawers & Modes
  notesCount: number;
  ttsActive: boolean;
  autoScrollActive: boolean;
  isFullscreen: boolean;
  customFonts: CustomFont[];
  shortcutsOpen?: boolean;
  onOpenShortcuts?: () => void;
  onCloseShortcuts?: () => void;
  onOpenToc: () => void;
  onOpenNotes: () => void;
  onToggleTts: () => void;
  onOpenInspector: () => void;
  onRefreshChapter: () => void;
  onToggleAutoScroll: () => void;
  onToggleFullscreen: () => void;
}

const PAPER_SWATCHES: readonly {
  id: Exclude<PaperTheme, "system">;
  label: string;
  bg: string;
  ring: string;
}[] = [
  { id: "paper", label: "日间", bg: "#FCFAF6", ring: "border-[#dad4c8]" },
  { id: "parchment", label: "羊皮纸", bg: "#F6EEDA", ring: "border-[#d4c2a0]" },
  { id: "celadon", label: "护眼绿", bg: "#EDF4EE", ring: "border-[#bdd6c3]" },
  { id: "dark", label: "夜间", bg: "#171513", ring: "border-[#433e38]" },
];

const SHORTCUT_ITEMS: readonly { keys: readonly string[]; label: string }[] = [
  { keys: ["←", "→"], label: "上一页 / 下一页" },
  { keys: ["Space"], label: "向后翻页 / 滚屏" },
  { keys: ["["], label: "上一章" },
  { keys: ["]"], label: "下一章" },
  { keys: ["C", "M"], label: "章节目录与全书检索" },
  { keys: ["N"], label: "书签与划线批注" },
  { keys: ["B"], label: "快速存一枚书签" },
  { keys: ["A"], label: "版式与阅读偏好" },
  { keys: ["T"], label: "开启 / 退出听书" },
  { keys: ["F"], label: "切换全屏沉浸" },
  { keys: ["H"], label: "显隐顶底控制栏" },
  { keys: ["Esc"], label: "逐层收起浮层与面板" },
];

export function ReaderHud({
  hudOpen,
  workbenchOpen,
  onToggleWorkbench,
  onCloseWorkbench,
  onBack,
  backLabel = "书架",
  bookName,
  chapterTitle,
  chapterIndex,
  chapters,
  onJumpChapter,
  onPrevChapter,
  onNextChapter,
  page,
  totalPages,
  chapterRatio,
  isInShelf,
  savingShelf,
  onToggleShelf,
  isCurrentBookmarked,
  onQuickBookmark,
  notesCount,
  ttsActive,
  autoScrollActive,
  isFullscreen,
  customFonts,
  shortcutsOpen = false,
  onOpenShortcuts,
  onCloseShortcuts,
  onOpenToc,
  onOpenNotes,
  onToggleTts,
  onOpenInspector,
  onRefreshChapter,
  onToggleAutoScroll,
  onToggleFullscreen,
}: ReaderHudProps) {
  const theme = usePrefsStore((s) => s.theme);
  const fontFamily = usePrefsStore((s) => s.fontFamily);
  const customFontFamily = usePrefsStore((s) => s.customFontFamily);
  const fontSize = usePrefsStore((s) => s.fontSize);
  const lineHeight = usePrefsStore((s) => s.lineHeight);
  const paragraphGap = usePrefsStore((s) => s.paragraphGap);
  const contentWidth = usePrefsStore((s) => s.contentWidth);
  const indentParagraph = usePrefsStore((s) => s.indentParagraph);
  const readLayout = usePrefsStore((s) => s.readLayout);
  const spreadMode = usePrefsStore((s) => s.spreadMode);
  const pageTurnMode = usePrefsStore((s) => s.pageTurnMode);
  const autoScrollSpeed = usePrefsStore((s) => s.autoScrollSpeed);

  const setTheme = usePrefsStore((s) => s.setTheme);
  const setFontFamily = usePrefsStore((s) => s.setFontFamily);
  const setCustomFontFamily = usePrefsStore((s) => s.setCustomFontFamily);
  const setFontSize = usePrefsStore((s) => s.setFontSize);
  const setLineHeight = usePrefsStore((s) => s.setLineHeight);
  const setParagraphGap = usePrefsStore((s) => s.setParagraphGap);
  const setContentWidth = usePrefsStore((s) => s.setContentWidth);
  const setIndentParagraph = usePrefsStore((s) => s.setIndentParagraph);
  const setReadLayout = usePrefsStore((s) => s.setReadLayout);
  const setSpreadMode = usePrefsStore((s) => s.setSpreadMode);
  const setPageTurnMode = usePrefsStore((s) => s.setPageTurnMode);
  const setAutoScrollSpeed = usePrefsStore((s) => s.setAutoScrollSpeed);
  const resetTypography = usePrefsStore((s) => s.resetTypography);

  const reduced = useReducedMotion();
  const activePaper = resolvePaperTheme(theme);

  // Scrubber preview state while dragging the chapter slider (commits ONLY on release)
  const [scrubIndex, setScrubIndex] = React.useState<number | null>(null);
  const [isScrubbing, setIsScrubbing] = React.useState(false);

  React.useEffect(() => {
    if (!hudOpen) {
      setScrubIndex(null);
      setIsScrubbing(false);
    }
  }, [hudOpen]);

  const effectiveSliderIdx = scrubIndex ?? chapterIndex;
  const maxChapterIdx = Math.max(0, chapters.length - 1);
  const scrubPct =
    maxChapterIdx > 0 ? Math.min(100, Math.max(0, (effectiveSliderIdx / maxChapterIdx) * 100)) : 0;
  const currentPct =
    maxChapterIdx > 0 ? Math.min(100, Math.max(0, (chapterIndex / maxChapterIdx) * 100)) : 0;

  const previewChapter = scrubIndex !== null ? chapters[scrubIndex] : undefined;
  const previewTitle = previewChapter
    ? cleanChapterTitle(previewChapter.title) || previewChapter.title || `第 ${(scrubIndex ?? 0) + 1} 章`
    : "";
  const scrubDelta = scrubIndex !== null ? scrubIndex - chapterIndex : 0;
  const clampedBubblePct = Math.min(82, Math.max(18, scrubPct));
  const bubbleCaretShiftPct = scrubPct - clampedBubblePct;

  const totalChapters = Math.max(1, chapters.length);
  const overallRatio = Math.min(
    1,
    Math.max(0, (chapterIndex + Math.min(1, Math.max(0, chapterRatio))) / totalChapters),
  );

  // Subtle reading clock (HH:mm) for desktop ambient footer
  const [clockText, setClockText] = React.useState(() => {
    const d = new Date();
    return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  });
  React.useEffect(() => {
    const tick = () => {
      const d = new Date();
      setClockText(
        `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`,
      );
    };
    const timer = window.setInterval(tick, 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const commitScrub = (nextVal: number) => {
    const clamped = Math.min(maxChapterIdx, Math.max(0, Math.round(nextVal)));
    setScrubIndex(null);
    setIsScrubbing(false);
    if (clamped !== chapterIndex) {
      onJumpChapter(clamped);
    }
  };

  const cancelScrub = () => {
    setScrubIndex(null);
    setIsScrubbing(false);
  };

  return (
    <>
      {/* ================================================================
          1. Ambient Immersion Micro-Chrome (Desktop 天头地脚 · Mobile 纯底脚极简)
         ================================================================ */}
      {/* Desktop-only subtle 天头 running header; hidden on mobile so text starts right near the top */}
      <div
        aria-hidden={hudOpen}
        className={cn(
          "pointer-events-none fixed inset-x-0 top-0 z-20 hidden items-center justify-between gap-4 px-7 pt-2.5 text-[11px] tracking-wide text-ink-muted/65 transition-opacity duration-250 safe-t md:flex",
          hudOpen ? "opacity-0" : "opacity-100",
        )}
      >
        <span className="min-w-0 truncate font-serif">{chapterTitle}</span>
        <span className="shrink-0 truncate font-serif md:max-w-[36%]">
          {bookName ? `《${bookName}》` : ""}
        </span>
      </div>

      {/* Bottom running footer: mobile shows chapterTitle + page/%, desktop shows chapter count + page/% + clock */}
      <div
        aria-hidden={hudOpen}
        className={cn(
          "pointer-events-none fixed inset-x-0 bottom-0 z-20 flex items-center justify-between gap-3 px-3.5 pb-1.5 text-[10.5px] tabular-nums text-ink-muted/65 transition-opacity duration-250 safe-b md:px-7 md:pb-2 md:text-[11px]",
          hudOpen ? "opacity-0" : "opacity-100",
        )}
      >
        <span className="min-w-0 truncate font-serif md:hidden">
          {chapterTitle || (chapters.length > 0 ? `第 ${chapterIndex + 1} / ${chapters.length} 章` : "")}
        </span>
        <span className="hidden truncate md:inline">
          {chapters.length > 0 ? `第 ${chapterIndex + 1} / ${chapters.length} 章` : ""}
        </span>
        <span className="shrink-0">
          {readLayout === "paged"
            ? `${Math.min(page + 1, Math.max(1, totalPages))}/${Math.max(1, totalPages)} · ${Math.round(overallRatio * 100)}%`
            : `本章 ${Math.round(chapterRatio * 100)}%`}
          <span className="hidden md:inline"> · {clockText}</span>
        </span>
      </div>

      {/* Bottom 1.5px Vermilion Hairline Progress */}
      <div
        aria-hidden
        className="pointer-events-none fixed inset-x-0 bottom-0 z-20 h-[1.5px] bg-line/40"
      >
        <div
          className="h-full bg-accent/85 transition-[width] duration-300 ease-out"
          style={{ width: `${Math.max(1.5, overallRatio * 100)}%` }}
        />
      </div>

      {/* ================================================================
          2. Auto-Scroll Floating Micro-Pill (when HUD closed & auto-scroll on)
         ================================================================ */}
      <AnimatePresence>
        {autoScrollActive && !hudOpen && readLayout === "scroll" ? (
          <motion.div
            key="autoscroll-pill"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 12, scale: 0.94 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: 8, scale: 0.94 }}
            transition={SPRING_POP}
            onClick={(e) => e.stopPropagation()}
            className="surface-glass fixed right-4 bottom-8 z-30 flex items-center gap-1 rounded-full py-1 pr-1.5 pl-2.5 text-xs safe-b sm:right-6"
          >
            <span className="size-1.5 animate-pulse rounded-full bg-accent" />
            <span className="mr-1 text-[11px] text-ink-secondary">自动滚屏</span>
            <button
              type="button"
              aria-label="减慢滚屏速度"
              onClick={() => setAutoScrollSpeed(autoScrollSpeed - 10)}
              className="flex size-6 cursor-pointer items-center justify-center rounded-full text-ink-secondary hover:bg-subtle hover:text-ink"
            >
              <Minus className="size-3" />
            </button>
            <span className="w-7 text-center font-mono text-[11px] tabular-nums text-ink">
              {autoScrollSpeed}
            </span>
            <button
              type="button"
              aria-label="加快滚屏速度"
              onClick={() => setAutoScrollSpeed(autoScrollSpeed + 10)}
              className="flex size-6 cursor-pointer items-center justify-center rounded-full text-ink-secondary hover:bg-subtle hover:text-ink"
            >
              <Plus className="size-3" />
            </button>
            <button
              type="button"
              aria-label="停止自动滚屏"
              onClick={onToggleAutoScroll}
              className="ml-0.5 flex size-6 cursor-pointer items-center justify-center rounded-full bg-accent-soft text-accent hover:bg-accent hover:text-accent-ink"
            >
              <Square className="size-2.5 fill-current" />
            </button>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* ================================================================
          3. HUD Open — Full-Width Frosted Book Header (Edge-to-Edge on Mobile & Desktop)
         ================================================================ */}
      <AnimatePresence>
        {hudOpen ? (
          <motion.header
            key="reader-top-bar"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: -12 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: -10 }}
            transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
            onClick={(e) => e.stopPropagation()}
            className="fixed inset-x-0 top-0 z-30 border-b border-line/80 bg-bg/92 shadow-[0_6px_24px_-16px_rgba(0,0,0,0.22)] backdrop-blur-xl safe-t"
          >
            <div className="mx-auto flex h-12 max-w-[1440px] items-center justify-between gap-2 px-3 sm:px-5 md:px-7">
              <div className="flex min-w-0 items-center gap-2 md:flex-1">
                <button
                  type="button"
                  onClick={onBack}
                  className="inline-flex h-8 shrink-0 cursor-pointer items-center gap-1 rounded-full px-2 text-xs font-medium text-ink transition-colors hover:bg-subtle sm:px-2.5"
                >
                  <ArrowLeft className="size-3.5" />
                  <span>{backLabel}</span>
                </button>

                <span aria-hidden className="hidden h-4 w-px shrink-0 bg-line-strong md:block" />

                {/* Desktop inline book + chapter title */}
                <button
                  type="button"
                  onClick={onOpenInspector}
                  title="查看书籍详情与换源"
                  className="hidden min-w-0 max-w-[460px] cursor-pointer items-baseline gap-2 rounded-full px-2.5 py-1 text-left transition-colors hover:bg-subtle md:inline-flex"
                >
                  <span className="shrink-0 truncate font-serif text-[13px] font-semibold text-ink">
                    {bookName || "未命名书籍"}
                  </span>
                  {chapterTitle ? (
                    <span className="min-w-0 truncate text-[11.5px] text-ink-muted">
                      {chapterTitle}
                    </span>
                  ) : null}
                </button>
              </div>

              {/* Mobile centered 2-line title */}
              <button
                type="button"
                onClick={onOpenInspector}
                title="查看书籍详情与换源"
                className="min-w-0 flex-1 cursor-pointer rounded-full px-1.5 py-0.5 text-center transition-colors hover:bg-subtle md:hidden"
              >
                <p className="truncate font-serif text-xs font-semibold leading-tight text-ink">
                  {bookName || "未命名书籍"}
                </p>
                {chapterTitle ? (
                  <p className="truncate text-[10px] leading-tight text-ink-muted">
                    {chapterTitle}
                  </p>
                ) : null}
              </button>

              {/* Right actions: bookmark + shelf on all screens; direct tools on desktop */}
              <div className="flex shrink-0 items-center gap-1 md:gap-1.5">
                <IconButton
                  size="sm"
                  active={isCurrentBookmarked}
                  aria-label={isCurrentBookmarked ? "本章已有书签，点击再存一枚" : "快速存书签"}
                  title={isCurrentBookmarked ? "本章已有书签 (B)" : "快速添加书签 (B)"}
                  onClick={onQuickBookmark}
                >
                  <Bookmark
                    className={cn("size-3.5", isCurrentBookmarked && "fill-current")}
                  />
                </IconButton>

                <button
                  type="button"
                  disabled={savingShelf}
                  onClick={isInShelf ? onOpenInspector : onToggleShelf}
                  className={cn(
                    "inline-flex h-7 shrink-0 cursor-pointer items-center gap-1 rounded-full px-2 text-[11px] font-medium transition-all active:scale-95 disabled:opacity-50 sm:px-2.5",
                    isInShelf
                      ? "bg-accent-soft text-accent hover:bg-accent/18"
                      : "bg-accent text-accent-ink shadow-xs hover:brightness-110",
                  )}
                >
                  {isInShelf ? (
                    <>
                      <Check className="size-3" />
                      <span className="sm:hidden">在架</span>
                      <span className="hidden sm:inline">已在书架</span>
                    </>
                  ) : (
                    <>
                      <Plus className="size-3" />
                      <span className="sm:hidden">入架</span>
                      <span className="hidden sm:inline">加入书架</span>
                    </>
                  )}
                </button>

                {/* Desktop direct toolbar buttons (no burying in More menu) */}
                <div className="hidden items-center gap-0.5 pl-1 md:flex">
                  <span aria-hidden className="mr-1 h-4 w-px bg-line-strong" />
                  <IconButton
                    size="sm"
                    aria-label="换源与书籍详情"
                    title="换源与书籍详情"
                    onClick={onOpenInspector}
                  >
                    <ArrowLeftRight className="size-3.5" />
                  </IconButton>
                  <IconButton
                    size="sm"
                    aria-label="重新拉取本章"
                    title="重新拉取本章"
                    onClick={onRefreshChapter}
                  >
                    <RefreshCw className="size-3.5" />
                  </IconButton>
                  <IconButton
                    size="sm"
                    active={isFullscreen}
                    aria-label={isFullscreen ? "退出全屏" : "全屏沉浸"}
                    title={isFullscreen ? "退出全屏 (F)" : "全屏沉浸 (F)"}
                    onClick={onToggleFullscreen}
                  >
                    {isFullscreen ? (
                      <Minimize2 className="size-3.5" />
                    ) : (
                      <Maximize2 className="size-3.5" />
                    )}
                  </IconButton>
                  {onOpenShortcuts ? (
                    <IconButton
                      size="sm"
                      active={shortcutsOpen}
                      aria-label="键盘快捷键一览"
                      title="键盘快捷键 (?)"
                      onClick={onOpenShortcuts}
                    >
                      <Keyboard className="size-3.5" />
                    </IconButton>
                  ) : null}
                </div>
              </div>
            </div>
          </motion.header>
        ) : null}
      </AnimatePresence>

      {/* ================================================================
          4. HUD Open — Responsive Bottom Control Dock
             - Mobile (<md): Full-width edge-docked sheet (safe-b)
             - Desktop (>=md): Centered floating glass island (580px)
         ================================================================ */}
      <AnimatePresence>
        {hudOpen ? (
          <motion.div
            key="reader-bottom-island-wrap"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 20 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: 16 }}
            transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
            onClick={(e) => e.stopPropagation()}
            className="fixed inset-x-0 bottom-0 z-30 md:inset-x-auto md:bottom-5 md:left-1/2 md:w-[min(92vw,580px)] md:-translate-x-1/2"
          >
            {/* Floating Chapter Preview Capsule when dragging slider (commits on release) */}
            <AnimatePresence>
              {isScrubbing && scrubIndex !== null && previewChapter ? (
                <motion.div
                  key="scrub-bubble"
                  initial={reduced ? { opacity: 0 } : { opacity: 0, y: 8, scale: 0.94 }}
                  animate={{ opacity: 1, y: 0, scale: 1 }}
                  exit={reduced ? { opacity: 0 } : { opacity: 0, y: 5, scale: 0.95 }}
                  transition={SPRING_POP}
                  style={{
                    left: `${clampedBubblePct}%`,
                  }}
                  className="surface-glass pointer-events-none absolute -top-17 z-40 w-[min(86vw,296px)] -translate-x-1/2 rounded-2xl px-3.5 py-2"
                >
                  <div className="flex items-center justify-between gap-2 text-[10px] tabular-nums">
                    <span className="inline-flex items-center rounded-full bg-accent-soft px-2 py-0.5 font-mono font-semibold text-accent">
                      第 {scrubIndex + 1} / {chapters.length} 章
                    </span>
                    <span className="font-mono text-ink-muted">
                      {scrubDelta === 0
                        ? "当前章"
                        : `${scrubDelta > 0 ? `+${scrubDelta}` : scrubDelta} 章 · 松手跳转`}
                    </span>
                  </div>
                  <p className="mt-1 truncate font-serif text-[13px] font-semibold text-ink">
                    {previewTitle}
                  </p>
                  {/* Precision downward pointer caret tracking slider thumb */}
                  <span
                    aria-hidden
                    style={{
                      left: `calc(50% + ${Math.max(-36, Math.min(36, bubbleCaretShiftPct * 1.6))}%)`,
                    }}
                    className="absolute -bottom-1.5 size-3 -translate-x-1/2 rotate-45 border-r border-b border-[var(--glass-border)] bg-elevated"
                  />
                </motion.div>
              ) : null}
            </AnimatePresence>

            {/* Morphing Glass Island (Mobile edge-docked sheet / Desktop floating island) */}
            <motion.footer
              layout={!reduced}
              transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
              className="rounded-t-[24px] border-t border-line/85 bg-bg/95 shadow-[0_-10px_32px_-12px_rgba(0,0,0,0.2)] backdrop-blur-xl safe-b md:surface-glass md:rounded-[26px] md:border-t-0 md:pb-0"
            >
              {/* Expanded In-Place Aa Typography Workbench (No dark scrim!) */}
              <AnimatePresence initial={false}>
                {workbenchOpen ? (
                  <motion.div
                    key="aa-workbench"
                    initial={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                    animate={{ opacity: 1, height: "auto" }}
                    exit={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                    transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
                    className="surface-workbench overflow-hidden rounded-t-[26px] border-b border-line"
                  >
                    <div className="max-h-[56vh] space-y-4 overflow-y-auto px-4 pt-3.5 pb-4 sm:px-5">
                      {/* Workbench Header */}
                      <div className="flex items-center justify-between gap-2">
                        <span className="font-serif text-sm font-semibold text-ink">
                          版式与阅读偏好
                        </span>
                        <div className="flex items-center gap-1.5">
                          <InlineConfirm
                            confirmLabel="恢复默认"
                            onConfirm={resetTypography}
                            trigger={(arm) => (
                              <button
                                type="button"
                                onClick={arm}
                                className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2.5 py-1 text-[11px] text-ink-muted transition-colors hover:bg-subtle hover:text-ink"
                              >
                                <RotateCcw className="size-3" />
                                <span>恢复默认</span>
                              </button>
                            )}
                          />
                          <IconButton
                            size="sm"
                            aria-label="收起版式工作台"
                            title="收起面板 (Esc)"
                            onClick={onCloseWorkbench}
                          >
                            <ChevronDown className="size-4" />
                          </IconButton>
                        </div>
                      </div>

                      {/* Layout Mode & Spread Mode */}
                      <div className="flex flex-wrap items-center justify-between gap-2.5">
                        <Segmented<ReadLayout>
                          layoutId="reader-layout-mode"
                          size="sm"
                          value={readLayout}
                          onChange={setReadLayout}
                          options={[
                            {
                              value: "paged",
                              label: "书卷翻页",
                              icon: <BookOpen className="size-3.5" />,
                            },
                            {
                              value: "scroll",
                              label: "长卷滚动",
                              icon: <ScrollText className="size-3.5" />,
                            },
                          ]}
                        />

                        {readLayout === "paged" ? (
                          <div className="flex flex-wrap items-center gap-2">
                            <Segmented<PageTurnMode>
                              layoutId="reader-pageturn-mode"
                              size="sm"
                              value={pageTurnMode}
                              onChange={setPageTurnMode}
                              options={[
                                { value: "curl", label: "仿真" },
                                { value: "none", label: "无动画" },
                              ]}
                            />
                            <div className="hidden lg:inline-flex">
                              <Segmented<SpreadMode>
                                layoutId="reader-spread-mode"
                                size="sm"
                                value={spreadMode}
                                onChange={setSpreadMode}
                                options={[
                                  {
                                    value: "auto",
                                    label: "双页对开",
                                    icon: <Columns2 className="size-3.5" />,
                                  },
                                  {
                                    value: "single",
                                    label: "单页专注",
                                  },
                                ]}
                              />
                            </div>
                          </div>
                        ) : null}
                      </div>

                      {/* Font Family Picker (Curated Builtins + Uploaded Custom Fonts) */}
                      <div>
                        <p className="mb-1.5 text-[11px] font-medium text-ink-muted">正文书体</p>
                        <div className="flex flex-wrap gap-1.5">
                          {READER_FONT_PRESETS.map((f) => {
                            const active = fontFamily === f.id;
                            return (
                              <button
                                key={f.id}
                                type="button"
                                title={f.description}
                                style={{ fontFamily: f.cssVar }}
                                onClick={() => {
                                  ensureReaderFontLoaded(f.id);
                                  setFontFamily(f.id);
                                }}
                                className={cn(
                                  "cursor-pointer rounded-full border px-3 py-1 text-xs transition-colors",
                                  active
                                    ? "border-accent/45 bg-accent-soft font-medium text-accent"
                                    : "border-line bg-elevated/60 text-ink-secondary hover:bg-subtle hover:text-ink",
                                )}
                              >
                                {f.label}
                              </button>
                            );
                          })}
                          {customFonts.map((cf) => {
                            const active =
                              fontFamily === "custom" && customFontFamily === cf.family;
                            return (
                              <button
                                key={cf.id}
                                type="button"
                                style={{ fontFamily: `"${cf.family}", var(--font-serif)` }}
                                onClick={() => {
                                  setCustomFontFamily(cf.family);
                                  setFontFamily("custom");
                                }}
                                className={cn(
                                  "cursor-pointer rounded-full border px-3 py-1 text-xs transition-colors",
                                  active
                                    ? "border-accent/45 bg-accent-soft font-medium text-accent"
                                    : "border-line bg-elevated/60 text-ink-secondary hover:bg-subtle hover:text-ink",
                                )}
                              >
                                {cf.name.replace(/\.[^.]+$/, "")}
                              </button>
                            );
                          })}
                        </div>
                      </div>

                      {/* Continuous Sliders Grid */}
                      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                        <MetricSliderRow
                          label="字号"
                          readout={`${fontSize}px`}
                          value={fontSize}
                          min={14}
                          max={32}
                          step={1}
                          onChange={setFontSize}
                        />
                        <MetricSliderRow
                          label="行距"
                          readout={lineHeight.toFixed(2)}
                          value={lineHeight}
                          min={1.4}
                          max={2.4}
                          step={0.05}
                          onChange={setLineHeight}
                        />
                        <MetricSliderRow
                          label="段距"
                          readout={`${paragraphGap.toFixed(1)}em`}
                          value={paragraphGap}
                          min={0.2}
                          max={1.8}
                          step={0.1}
                          onChange={setParagraphGap}
                        />
                        <div className="hidden sm:block">
                          <MetricSliderRow
                            label="版心宽度"
                            readout={`${contentWidth}em`}
                            value={contentWidth}
                            min={32}
                            max={62}
                            step={1}
                            onChange={setContentWidth}
                          />
                        </div>
                      </div>

                      {/* Paragraph Indent & Scroll Speed */}
                      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line/70 pt-3">
                        <div className="flex items-center gap-2.5">
                          <span className="text-xs text-ink-secondary">首行两字缩进</span>
                          <Switch
                            checked={indentParagraph}
                            onChange={setIndentParagraph}
                            ariaLabel="首行两字缩进"
                          />
                        </div>

                        {readLayout === "scroll" ? (
                          <div className="flex min-w-[180px] flex-1 items-center gap-2 sm:max-w-[220px]">
                            <span className="shrink-0 text-[11px] text-ink-muted">滚速</span>
                            <Slider
                              ariaLabel="自动滚屏速度"
                              value={autoScrollSpeed}
                              min={12}
                              max={140}
                              step={4}
                              onChange={setAutoScrollSpeed}
                              className="flex-1"
                            />
                            <span className="w-8 shrink-0 text-right font-mono text-[11px] tabular-nums text-ink-secondary">
                              {autoScrollSpeed}
                            </span>
                          </div>
                        ) : null}
                      </div>

                      {/* Subtle Desktop Keyboard Hint Strip */}
                      <div className="hidden items-center justify-between border-t border-line/55 pt-2.5 text-[11px] text-ink-muted sm:flex">
                        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                          <span className="inline-flex items-center gap-1">
                            <kbd className="kbd-chip">←</kbd>
                            <kbd className="kbd-chip">→</kbd>
                            <span>翻页</span>
                          </span>
                          <span className="inline-flex items-center gap-1">
                            <kbd className="kbd-chip">[</kbd>
                            <kbd className="kbd-chip">]</kbd>
                            <span>切章</span>
                          </span>
                          <span className="inline-flex items-center gap-1">
                            <kbd className="kbd-chip">C</kbd>
                            <span>目录</span>
                          </span>
                          <span className="inline-flex items-center gap-1">
                            <kbd className="kbd-chip">Esc</kbd>
                            <span>收起</span>
                          </span>
                        </div>
                        {onOpenShortcuts ? (
                          <button
                            type="button"
                            onClick={onOpenShortcuts}
                            className="inline-flex cursor-pointer items-center gap-1 rounded-full px-2 py-0.5 text-[11px] text-ink-secondary transition-colors hover:bg-subtle hover:text-ink"
                          >
                            <Keyboard className="size-3" />
                            <span>全部快捷键</span>
                          </button>
                        ) : null}
                      </div>
                    </div>
                  </motion.div>
                ) : null}
              </AnimatePresence>

              {/* Standard State: Upper Chapter Scrubber Rail */}
              <div className="flex items-center gap-2 px-3 pt-2.5 pb-1.5 sm:px-4">
                <button
                  type="button"
                  aria-label="上一章"
                  title="上一章 ([)"
                  disabled={chapterIndex <= 0}
                  onClick={onPrevChapter}
                  className="inline-flex h-7 shrink-0 cursor-pointer items-center gap-0.5 rounded-full px-2 text-xs text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:pointer-events-none disabled:opacity-35"
                >
                  <ChevronLeft className="size-3.5" />
                  <span className="hidden sm:inline">上一章</span>
                </button>

                {/* Tactile Chapter Scrubber with Drag-Preview & Release-to-Commit */}
                <div className="relative flex h-7 flex-1 items-center">
                  <div className="pointer-events-none relative h-1.5 w-full overflow-hidden rounded-full bg-line-strong/60">
                    <div
                      className="h-full rounded-full bg-accent transition-[width] duration-75"
                      style={{ width: `${scrubPct}%` }}
                    />
                  </div>
                  {/* Origin marker showing where user started before dragging */}
                  {isScrubbing && scrubIndex !== null && scrubIndex !== chapterIndex ? (
                    <span
                      aria-hidden
                      style={{ left: `${currentPct}%` }}
                      className="pointer-events-none absolute top-1/2 size-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-ink-muted/55"
                    />
                  ) : null}
                  <div
                    aria-hidden
                    style={{ left: `${scrubPct}%` }}
                    className={cn(
                      "pointer-events-none absolute top-1/2 size-4 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-accent bg-elevated shadow-sm transition-transform duration-150",
                      isScrubbing && "scale-125 ring-4 ring-accent/20",
                    )}
                  />
                  <input
                    type="range"
                    aria-label="章节进度滑块"
                    disabled={maxChapterIdx <= 0}
                    min={0}
                    max={maxChapterIdx}
                    step={1}
                    value={effectiveSliderIdx}
                    onPointerDown={() => {
                      if (maxChapterIdx <= 0) return;
                      setIsScrubbing(true);
                      setScrubIndex((prev) => prev ?? chapterIndex);
                    }}
                    onChange={(e) => {
                      const next = Number(e.target.value);
                      setIsScrubbing(true);
                      setScrubIndex(next);
                    }}
                    onPointerUp={(e) => commitScrub(Number((e.target as HTMLInputElement).value))}
                    onPointerCancel={cancelScrub}
                    onBlur={cancelScrub}
                    onKeyUp={(e) => {
                      if (e.key === "Escape") {
                        cancelScrub();
                        return;
                      }
                      if (
                        e.key === "ArrowLeft" ||
                        e.key === "ArrowRight" ||
                        e.key === "ArrowUp" ||
                        e.key === "ArrowDown" ||
                        e.key === "Home" ||
                        e.key === "End"
                      ) {
                        commitScrub(Number((e.target as HTMLInputElement).value));
                      }
                    }}
                    className="absolute inset-0 h-full w-full cursor-pointer opacity-0 disabled:cursor-default"
                  />
                </div>

                {chapters.length > 0 ? (
                  <span className="shrink-0 font-mono text-[10px] tabular-nums text-ink-muted">
                    {effectiveSliderIdx + 1}/{chapters.length}
                  </span>
                ) : null}

                <button
                  type="button"
                  aria-label="下一章"
                  title="下一章 (])"
                  disabled={chapterIndex >= maxChapterIdx}
                  onClick={onNextChapter}
                  className="inline-flex h-7 shrink-0 cursor-pointer items-center gap-0.5 rounded-full px-2 text-xs text-ink-secondary transition-colors hover:bg-subtle hover:text-ink disabled:pointer-events-none disabled:opacity-35"
                >
                  <span className="hidden sm:inline">下一章</span>
                  <ChevronRight className="size-3.5" />
                </button>
              </div>

              {/* Standard State: Lower Action Dock */}
              <div className="flex items-center justify-between gap-1 px-2 pt-0.5 pb-2 sm:px-4 sm:pt-1 sm:pb-2.5">
                <div className="flex items-center gap-0.5 sm:gap-1">
                  <DockButton
                    icon={<List className="size-4" />}
                    label="目录"
                    shortcut="C"
                    onClick={onOpenToc}
                  />
                  <DockButton
                    icon={<Highlighter className="size-4" />}
                    label="笔记"
                    shortcut="N"
                    badge={notesCount > 0 ? (notesCount > 99 ? "99+" : String(notesCount)) : undefined}
                    onClick={onOpenNotes}
                  />
                  <DockButton
                    icon={<Headphones className="size-4" />}
                    label="听书"
                    shortcut="T"
                    active={ttsActive}
                    pulse={ttsActive}
                    onClick={onToggleTts}
                  />
                  <DockButton
                    icon={<span className="font-serif text-[14px] leading-none font-semibold sm:text-[15px]">Aa</span>}
                    label="版式"
                    shortcut="A"
                    active={workbenchOpen}
                    onClick={onToggleWorkbench}
                  />
                </div>

                <span aria-hidden className="hidden h-5 w-px bg-line-strong sm:block" />

                {/* 4 Physical Paper Theme Swatch Dots with spring layoutId ring */}
                <div className="flex items-center gap-1 sm:gap-2">
                  {PAPER_SWATCHES.map((swatch) => {
                    const selected = activePaper === swatch.id;
                    return (
                      <button
                        key={swatch.id}
                        type="button"
                        title={`${swatch.label}纸色`}
                        aria-label={`切换${swatch.label}纸色`}
                        aria-pressed={selected}
                        onClick={() => setTheme(swatch.id)}
                        className="relative flex size-6 cursor-pointer items-center justify-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-accent/60 sm:size-7"
                      >
                        {selected ? (
                          <motion.span
                            layoutId="reader-paper-ring"
                            transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                            className="pointer-events-none absolute inset-0 rounded-full ring-2 ring-accent"
                          />
                        ) : null}
                        <span
                          style={{ backgroundColor: swatch.bg }}
                          className={cn(
                            "size-4.5 rounded-full border shadow-inner transition-transform hover:scale-105 sm:size-5",
                            swatch.ring,
                          )}
                        />
                      </button>
                    );
                  })}
                </div>

                {/* More Menu Popover */}
                <Popover
                  side="top"
                  align="end"
                  trigger={
                    <IconButton size="sm" aria-label="更多阅读选项" title="更多阅读选项">
                      <MoreHorizontal className="size-4" />
                    </IconButton>
                  }
                >
                  {(close) => (
                    <>
                      <PopoverItem
                        icon={<ArrowLeftRight className="size-4" />}
                        onClick={() => {
                          close();
                          onOpenInspector();
                        }}
                      >
                        换源与书籍详情
                      </PopoverItem>
                      <PopoverItem
                        icon={<RefreshCw className="size-4" />}
                        onClick={() => {
                          close();
                          onRefreshChapter();
                        }}
                      >
                        重新拉取本章
                      </PopoverItem>
                      <PopoverDivider />
                      <PopoverItem
                        icon={autoScrollActive ? <Square className="size-3.5" /> : <Play className="size-4" />}
                        onClick={() => {
                          close();
                          onToggleAutoScroll();
                        }}
                      >
                        {autoScrollActive ? "停止自动滚屏" : "开启自动滚屏"}
                      </PopoverItem>
                      <PopoverItem
                        icon={
                          isFullscreen ? (
                            <Minimize2 className="size-4" />
                          ) : (
                            <Maximize2 className="size-4" />
                          )
                        }
                        onClick={() => {
                          close();
                          onToggleFullscreen();
                        }}
                      >
                        <span className="flex w-full items-center justify-between gap-4">
                          <span>{isFullscreen ? "退出全屏" : "全屏沉浸"}</span>
                          <kbd className="kbd-chip hidden sm:inline-flex">F</kbd>
                        </span>
                      </PopoverItem>
                      {onOpenShortcuts ? (
                        <PopoverItem
                          icon={<Keyboard className="size-4" />}
                          onClick={() => {
                            close();
                            onOpenShortcuts();
                          }}
                        >
                          <span className="flex w-full items-center justify-between gap-4">
                            <span>键盘快捷键</span>
                            <kbd className="kbd-chip hidden sm:inline-flex">?</kbd>
                          </span>
                        </PopoverItem>
                      ) : null}
                    </>
                  )}
                </Popover>
              </div>
            </motion.footer>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* ================================================================
          5. Restrained Floating Keyboard Shortcuts Reference Card
         ================================================================ */}
      <AnimatePresence>
        {shortcutsOpen && onCloseShortcuts ? (
          <motion.div
            key="reader-shortcuts-overlay"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16 }}
            onClick={(e) => {
              e.stopPropagation();
              onCloseShortcuts();
            }}
            className="fixed inset-0 z-45 flex items-center justify-center bg-black/18 px-4 backdrop-blur-[2px] dark:bg-black/45"
          >
            <motion.div
              initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.94, y: 10 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.96, y: 6 }}
              transition={SPRING_POP}
              onClick={(e) => e.stopPropagation()}
              className="surface-glass w-full max-w-[390px] rounded-3xl p-5"
            >
              <div className="flex items-center justify-between gap-3 border-b border-line pb-3">
                <div className="flex items-center gap-2">
                  <Keyboard className="size-4 text-accent" />
                  <h3 className="font-serif text-sm font-semibold text-ink">阅读键盘快捷键</h3>
                </div>
                <button
                  type="button"
                  aria-label="关闭快捷键提示"
                  onClick={onCloseShortcuts}
                  className="flex size-7 cursor-pointer items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-subtle hover:text-ink"
                >
                  <X className="size-3.5" />
                </button>
              </div>

              <div className="mt-3.5 space-y-2">
                {SHORTCUT_ITEMS.map((item) => (
                  <div
                    key={item.label}
                    className="flex items-center justify-between gap-3 rounded-xl px-2 py-1 text-xs transition-colors hover:bg-subtle/60"
                  >
                    <span className="text-ink-secondary">{item.label}</span>
                    <span className="flex shrink-0 items-center gap-1">
                      {item.keys.map((k) => (
                        <kbd key={k} className="kbd-chip">
                          {k}
                        </kbd>
                      ))}
                    </span>
                  </div>
                ))}
              </div>

              <p className="mt-3.5 border-t border-line/60 pt-2.5 text-center text-[11px] text-ink-muted">
                按 <kbd className="kbd-chip mx-0.5">?</kbd> 随时唤出 ·{" "}
                <kbd className="kbd-chip mx-0.5">Esc</kbd> 关闭面板
              </p>
            </motion.div>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </>
  );
}

function DockButton({
  icon,
  label,
  shortcut,
  active = false,
  pulse = false,
  badge,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  shortcut?: string;
  active?: boolean;
  pulse?: boolean;
  badge?: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={badge ? `${label}（${badge}）` : label}
      title={shortcut ? `${label} (${shortcut})` : label}
      onClick={onClick}
      className={cn(
        "relative inline-flex cursor-pointer flex-col items-center justify-center gap-0.5 rounded-2xl px-2 py-1 text-[10px] font-medium transition-colors sm:h-9 sm:flex-row sm:gap-1.5 sm:rounded-full sm:px-3 sm:py-0 sm:text-xs",
        active
          ? "bg-accent-soft text-accent"
          : "text-ink-secondary hover:bg-subtle hover:text-ink",
      )}
    >
      <span className="relative flex h-4 items-center justify-center">
        {icon}
        {pulse ? (
          <span className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-accent">
            <span className="absolute inset-0 animate-ping rounded-full bg-accent opacity-75" />
          </span>
        ) : null}
        {badge ? (
          <span className="absolute -top-1.5 -right-2.5 rounded-full bg-accent px-1 py-[1px] text-[9px] leading-none tabular-nums text-accent-ink sm:hidden">
            {badge}
          </span>
        ) : null}
      </span>
      <span className="leading-none sm:leading-normal">{label}</span>
      {badge ? (
        <span className="hidden rounded-full bg-accent-soft px-1.5 py-0.5 text-[10px] leading-none tabular-nums text-accent sm:inline">
          {badge}
        </span>
      ) : null}
    </button>
  );
}

function MetricSliderRow({
  label,
  readout,
  value,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  readout: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between text-[11px]">
        <span className="font-medium text-ink-muted">{label}</span>
        <span className="font-mono tabular-nums text-ink-secondary">{readout}</span>
      </div>
      <div className="flex items-center gap-2">
        <button
          type="button"
          aria-label={`减小${label}`}
          onClick={() => onChange(Math.max(min, Number((value - step).toFixed(2))))}
          className="flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-full bg-subtle text-ink-secondary hover:bg-hover hover:text-ink"
        >
          <Minus className="size-3" />
        </button>
        <Slider
          ariaLabel={label}
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={onChange}
          className="flex-1"
        />
        <button
          type="button"
          aria-label={`增大${label}`}
          onClick={() => onChange(Math.min(max, Number((value + step).toFixed(2))))}
          className="flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-full bg-subtle text-ink-secondary hover:bg-hover hover:text-ink"
        >
          <Plus className="size-3" />
        </button>
      </div>
    </div>
  );
}
