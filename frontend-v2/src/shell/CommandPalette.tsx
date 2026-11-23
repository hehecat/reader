import { useQuery } from "@tanstack/react-query";
import {
  BookOpen,
  Check,
  Clock,
  Compass,
  CornerDownLeft,
  Filter,
  Palette,
  Search,
  Settings,
  Sparkles,
  Terminal,
  Upload,
  Wrench,
  X,
} from "lucide-react";
import * as React from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router-dom";
import { booksApi } from "@/api/endpoints";
import type { Book } from "@/api/types";
import { cn } from "@/lib/cn";
import { buildReaderUrl, cleanChapterTitle, formatRelativeTime } from "@/lib/format";
import { usePrefsStore, type PaperTheme } from "@/stores/prefs";
import { BookCover } from "@/ui/BookCover";
import { AnimatePresence, motion, SPRING_POP, useReducedMotion } from "@/ui/motion";

export interface PaperSwatch {
  id: Exclude<PaperTheme, "system">;
  name: string;
  desc: string;
  bg: string;
  surface: string;
  ink: string;
  accent: string;
  border: string;
}

export const PAPER_SWATCHES: readonly PaperSwatch[] = [
  {
    id: "paper",
    name: "日间",
    desc: "暖白宣纸",
    bg: "#fcfaf6",
    surface: "#ffffff",
    ink: "#26221e",
    accent: "#8c3b28",
    border: "rgba(38,34,30,0.14)",
  },
  {
    id: "parchment",
    name: "羊皮纸",
    desc: "柔和暖调",
    bg: "#f6eeda",
    surface: "#fbf5e6",
    ink: "#433422",
    accent: "#8a5a2b",
    border: "rgba(67,52,34,0.18)",
  },
  {
    id: "celadon",
    name: "护眼绿",
    desc: "青瓷豆沙",
    bg: "#edf4ee",
    surface: "#f6fbf7",
    ink: "#223529",
    accent: "#2e6b4c",
    border: "rgba(34,53,41,0.16)",
  },
  {
    id: "dark",
    name: "夜间",
    desc: "玄墨护眼",
    bg: "#171513",
    surface: "#201d1a",
    ink: "#e8e4dc",
    accent: "#df7d5e",
    border: "rgba(255,255,255,0.16)",
  },
];

interface PaletteItem {
  id: string;
  section: string;
  run: () => void;
  render: (active: boolean) => React.ReactNode;
}

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
}

interface QuickCommand {
  id: string;
  label: string;
  hint: string;
  keywords: string;
  shortcut?: string;
  icon: React.ReactNode;
  to: string;
}

const QUICK_COMMANDS: readonly QuickCommand[] = [
  {
    id: "cmd:import",
    label: "导入本地书",
    hint: "上传 TXT / EPUB 到书房",
    keywords: "import local txt epub 上传 导入 本地",
    icon: <Upload className="size-4" />,
    to: "/?import=1",
  },
  {
    id: "cmd:explore",
    label: "逛逛书海",
    hint: "按书源发现分类与榜单",
    keywords: "explore 发现 书海 分类 榜单",
    icon: <Compass className="size-4" />,
    to: "/explore",
  },
  {
    id: "cmd:sources",
    label: "书源工坊",
    hint: "管理书源、分组、订阅与连通率",
    keywords: "sources 书源 订阅 分组 管理",
    icon: <Wrench className="size-4" />,
    to: "/studio/sources",
  },
  {
    id: "cmd:workbench",
    label: "规则调试台",
    hint: "实时 SSE 追踪书源规则解析",
    keywords: "workbench debug 规则 调试 抓包",
    icon: <Terminal className="size-4" />,
    to: "/studio/workbench",
  },
  {
    id: "cmd:purify",
    label: "正文净化",
    hint: "替换规则与实时正则沙箱",
    keywords: "purify replace regex 净化 替换 正则",
    icon: <Filter className="size-4" />,
    to: "/studio/purify",
  },
  {
    id: "cmd:settings",
    label: "偏好设置",
    hint: "阅读排版、自定义字体、语音与备份",
    keywords: "settings prefs 设置 排版 字体 备份 语音",
    icon: <Settings className="size-4" />,
    to: "/settings",
  },
];

function highlightMatch(text: string, query: string): React.ReactNode {
  if (!query) return text;
  const idx = text.toLowerCase().indexOf(query.toLowerCase());
  if (idx < 0) return text;
  return (
    <>
      {text.slice(0, idx)}
      <mark className="rounded-[3px] bg-accent-soft px-0.5 text-accent font-semibold">
        {text.slice(idx, idx + query.length)}
      </mark>
      {text.slice(idx + query.length)}
    </>
  );
}

function formatBookProgress(book: Book): string {
  const chapterTitle = cleanChapterTitle(book.durChapterTitle);
  const pct =
    book.totalChapterNum > 0
      ? `${Math.min(100, Math.max(0, Math.round(((book.durChapterIndex + 1) / book.totalChapterNum) * 100)))}%`
      : null;

  if (chapterTitle && pct) return `${chapterTitle} · ${pct}`;
  if (chapterTitle) return chapterTitle;
  if (book.durChapterIndex > 0) {
    return pct ? `第 ${book.durChapterIndex + 1} 章 · ${pct}` : `第 ${book.durChapterIndex + 1} 章`;
  }
  return pct ? `尚未阅读 · ${pct}` : "尚未阅读";
}

function renderSectionIcon(section: string): React.ReactNode {
  switch (section) {
    case "最近阅读":
      return <Clock className="size-3 opacity-75" aria-hidden />;
    case "书架藏书":
      return <BookOpen className="size-3 opacity-75" aria-hidden />;
    case "快捷命令":
      return <Sparkles className="size-3 opacity-75" aria-hidden />;
    case "纸张色调":
      return <Palette className="size-3 opacity-75" aria-hidden />;
    case "全网寻书":
      return <Search className="size-3 opacity-75" aria-hidden />;
    default:
      return null;
  }
}

function renderBookRow(
  book: Book,
  active: boolean,
  showTime = false,
  highlight = "",
): React.ReactNode {
  const coverSrc = book.customCoverUrl || book.displayCover || book.coverUrl;
  const relTime = showTime ? formatRelativeTime(book.durChapterTime) : "";
  return (
    <div className="flex min-w-0 flex-1 items-center gap-3">
      <BookCover
        src={coverSrc}
        referer={book.origin}
        title={book.name}
        author={book.author}
        interactive={false}
        pageEdges={false}
        className="w-8 shrink-0"
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="truncate font-serif text-[14px] font-medium text-ink">
            {highlightMatch(book.name, highlight)}
          </span>
          {book.author ? (
            <span className="shrink-0 truncate text-xs text-ink-muted">
              {highlightMatch(book.author, highlight)}
            </span>
          ) : null}
        </div>
        <p className="mt-0.5 truncate text-xs text-ink-secondary">
          {formatBookProgress(book)}
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-2">
        {relTime ? (
          <span className="hidden text-[11px] text-ink-muted tabular-nums sm:inline">{relTime}</span>
        ) : null}
        {active ? (
          <span className="inline-flex items-center gap-1 rounded-full bg-accent-soft px-2 py-0.5 text-[11px] font-medium text-accent">
            续读
            <CornerDownLeft className="size-3" aria-hidden />
          </span>
        ) : null}
      </div>
    </div>
  );
}

export function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const navigate = useNavigate();
  const reduced = useReducedMotion();
  const theme = usePrefsStore((s) => s.theme);
  const setTheme = usePrefsStore((s) => s.setTheme);

  const [query, setQuery] = React.useState("");
  const [activeIndex, setActiveIndex] = React.useState(0);
  const inputRef = React.useRef<HTMLInputElement>(null);
  const listRef = React.useRef<HTMLDivElement>(null);
  /** 区分键盘导航与鼠标悬停，避免 scrollIntoView 时静止鼠标抢回 activeIndex */
  const keyboardNavRef = React.useRef(false);
  const listId = React.useId();

  const { data: shelf = [] } = useQuery<Book[]>({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(false),
    enabled: open,
    staleTime: 60_000,
  });

  React.useEffect(() => {
    if (!open) return;
    const prevFocused = document.activeElement as HTMLElement | null;
    setQuery("");
    setActiveIndex(0);
    keyboardNavRef.current = false;

    const id = window.requestAnimationFrame(() => {
      inputRef.current?.focus();
    });
    const onGlobalKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.isComposing) {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onGlobalKey, true);
    return () => {
      window.cancelAnimationFrame(id);
      window.removeEventListener("keydown", onGlobalKey, true);
      if (prevFocused && document.contains(prevFocused)) {
        prevFocused.focus({ preventScroll: true });
      }
    };
  }, [open, onClose]);

  const trimmed = query.trim();

  const recentBooks = React.useMemo(() => {
    return [...shelf]
      .sort(
        (a, b) =>
          (b.durChapterTime || 0) - (a.durChapterTime || 0) ||
          (b.createdAt || 0) - (a.createdAt || 0),
      )
      .slice(0, 3);
  }, [shelf]);

  const matchedBooks = React.useMemo(() => {
    if (!trimmed) return [];
    const q = trimmed.toLowerCase();
    return shelf
      .filter((b) => {
        const hay = `${b.name} ${b.author ?? ""} ${b.originName ?? ""} ${b.kind ?? ""} ${b.durChapterTitle ?? ""}`.toLowerCase();
        return hay.includes(q);
      })
      .sort((a, b) => {
        const aExact = a.name.toLowerCase().startsWith(q) ? 1 : 0;
        const bExact = b.name.toLowerCase().startsWith(q) ? 1 : 0;
        if (aExact !== bExact) return bExact - aExact;
        return (b.durChapterTime || 0) - (a.durChapterTime || 0);
      })
      .slice(0, 6);
  }, [shelf, trimmed]);

  const matchedCommands = React.useMemo(() => {
    if (!trimmed) return QUICK_COMMANDS;
    const q = trimmed.toLowerCase();
    return QUICK_COMMANDS.filter((cmd) =>
      `${cmd.label} ${cmd.hint} ${cmd.keywords}`.toLowerCase().includes(q),
    );
  }, [trimmed]);

  const matchedSwatches = React.useMemo(() => {
    if (!trimmed) return PAPER_SWATCHES;
    const q = trimmed.toLowerCase();
    return PAPER_SWATCHES.filter((sw) =>
      `${sw.name} ${sw.desc} ${sw.id} 主题 纸色 纸张`.toLowerCase().includes(q),
    );
  }, [trimmed]);

  const go = React.useCallback(
    (to: string) => {
      onClose();
      navigate(to);
    },
    [navigate, onClose],
  );

  const items = React.useMemo<PaletteItem[]>(() => {
    const list: PaletteItem[] = [];

    if (trimmed) {
      for (const book of matchedBooks) {
        list.push({
          id: `book:${book.bookUrl}`,
          section: "书架藏书",
          run: () => go(buildReaderUrl(book.bookUrl, book.durChapterIndex, book.origin)),
          render: (active) => renderBookRow(book, active, false, trimmed),
        });
      }

      list.push({
        id: `discover:${trimmed}`,
        section: "全网寻书",
        run: () => go(`/discover?q=${encodeURIComponent(trimmed)}`),
        render: (active) => (
          <div className="flex min-w-0 flex-1 items-center gap-3 py-0.5">
            <span className="flex size-8 shrink-0 items-center justify-center rounded-xl bg-accent-soft text-accent">
              <Search className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <p className="truncate text-[13px] font-medium text-ink">
                在全网书源中搜寻「<span className="text-accent">{trimmed}</span>」
              </p>
              <p className="text-[11px] text-ink-muted">流式并发聚合所有已启用书源</p>
            </div>
            <kbd
              className={cn(
                "kbd-chip shrink-0 transition-colors",
                active && "border-accent/35 bg-accent-soft text-accent",
              )}
            >
              ↵
            </kbd>
          </div>
        ),
      });

      for (const cmd of matchedCommands) {
        list.push({
          id: cmd.id,
          section: "快捷命令",
          run: () => go(cmd.to),
          render: (active) => (
            <div className="flex min-w-0 flex-1 items-center gap-3 py-0.5">
              <span
                className={cn(
                  "flex size-8 shrink-0 items-center justify-center rounded-xl transition-colors",
                  active ? "bg-accent-soft text-accent" : "bg-subtle text-ink-secondary",
                )}
              >
                {cmd.icon}
              </span>
              <div className="min-w-0 flex-1">
                <p className="truncate text-[13px] font-medium text-ink">
                  {highlightMatch(cmd.label, trimmed)}
                </p>
                <p className="truncate text-[11px] text-ink-muted">{cmd.hint}</p>
              </div>
              {active ? (
                <kbd className="kbd-chip shrink-0 border-accent/35 bg-accent-soft text-accent">
                  ↵
                </kbd>
              ) : null}
            </div>
          ),
        });
      }

      for (const sw of matchedSwatches) {
        const isCurrent = theme === sw.id;
        list.push({
          id: `paper:${sw.id}`,
          section: "纸张色调",
          run: () => {
            setTheme(sw.id);
            onClose();
          },
          render: (active) => (
            <div className="flex min-w-0 flex-1 items-center gap-3 py-0.5">
              <span
                style={{
                  backgroundColor: sw.bg,
                  borderColor: sw.border,
                  color: sw.ink,
                }}
                className="flex size-8 shrink-0 items-center justify-center rounded-xl border font-serif text-xs font-semibold shadow-xs"
              >
                纸
              </span>
              <div className="min-w-0 flex-1">
                <p className="truncate text-[13px] font-medium text-ink">
                  切换至「{highlightMatch(sw.name, trimmed)}」
                </p>
                <p className="truncate text-[11px] text-ink-muted">{sw.desc}</p>
              </div>
              {isCurrent ? (
                <span className="inline-flex items-center gap-1 rounded-full bg-accent-soft px-2 py-0.5 text-[11px] font-medium text-accent">
                  <Check className="size-3" />
                  当前
                </span>
              ) : active ? (
                <kbd className="kbd-chip shrink-0 border-accent/35 bg-accent-soft text-accent">
                  ↵
                </kbd>
              ) : null}
            </div>
          ),
        });
      }

      return list;
    }

    // 空输入：最近阅读
    for (const book of recentBooks) {
      list.push({
        id: `recent:${book.bookUrl}`,
        section: "最近阅读",
        run: () => go(buildReaderUrl(book.bookUrl, book.durChapterIndex, book.origin)),
        render: (active) => renderBookRow(book, active, true),
      });
    }

    // 快捷命令
    for (const cmd of QUICK_COMMANDS) {
      list.push({
        id: cmd.id,
        section: "快捷命令",
        run: () => go(cmd.to),
        render: (active) => (
          <div className="flex min-w-0 flex-1 items-center gap-3 py-0.5">
            <span
              className={cn(
                "flex size-8 shrink-0 items-center justify-center rounded-xl transition-colors",
                active ? "bg-accent-soft text-accent" : "bg-subtle text-ink-secondary",
              )}
            >
              {cmd.icon}
            </span>
            <div className="min-w-0 flex-1">
              <p className="truncate text-[13px] font-medium text-ink">{cmd.label}</p>
              <p className="truncate text-[11px] text-ink-muted">{cmd.hint}</p>
            </div>
            {active ? (
              <kbd className="kbd-chip shrink-0 border-accent/35 bg-accent-soft text-accent">
                ↵
              </kbd>
            ) : cmd.shortcut ? (
              <kbd className="kbd-chip shrink-0">{cmd.shortcut}</kbd>
            ) : null}
          </div>
        ),
      });
    }

    // 4 款纸色一键切换
    for (const sw of PAPER_SWATCHES) {
      const isCurrent = theme === sw.id;
      list.push({
        id: `paper:${sw.id}`,
        section: "纸张色调",
        run: () => {
          setTheme(sw.id);
          onClose();
        },
        render: (active) => (
          <div className="flex min-w-0 flex-1 items-center gap-3 py-0.5">
            <span
              style={{
                backgroundColor: sw.bg,
                borderColor: sw.border,
                color: sw.ink,
              }}
              className="flex size-8 shrink-0 items-center justify-center rounded-xl border font-serif text-xs font-semibold shadow-xs"
            >
              纸
            </span>
            <div className="min-w-0 flex-1">
              <p className="truncate text-[13px] font-medium text-ink">
                切换至「{sw.name}」
              </p>
              <p className="truncate text-[11px] text-ink-muted">{sw.desc}</p>
            </div>
            {isCurrent ? (
              <span className="inline-flex items-center gap-1 rounded-full bg-accent-soft px-2 py-0.5 text-[11px] font-medium text-accent">
                <Check className="size-3" />
                当前
              </span>
            ) : active ? (
              <kbd className="kbd-chip shrink-0 border-accent/35 bg-accent-soft text-accent">
                ↵
              </kbd>
            ) : null}
          </div>
        ),
      });
    }

    return list;
  }, [trimmed, matchedBooks, matchedCommands, matchedSwatches, recentBooks, go, theme, setTheme, onClose]);

  React.useEffect(() => {
    setActiveIndex(0);
    if (listRef.current) {
      listRef.current.scrollTop = 0;
    }
  }, [trimmed, items.length]);

  // 键盘导航时自动滚入可视区；回到首项时同时露出首个分组标题
  React.useEffect(() => {
    if (!open || !keyboardNavRef.current) return;
    const container = listRef.current;
    if (!container) return;
    if (activeIndex === 0) {
      container.scrollTo({ top: 0, behavior: reduced ? "auto" : "smooth" });
      return;
    }
    const activeEl = container.querySelector<HTMLElement>(`[data-palette-idx="${activeIndex}"]`);
    activeEl?.scrollIntoView({
      block: "nearest",
      behavior: reduced ? "auto" : "smooth",
    });
  }, [activeIndex, open, reduced]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.nativeEvent.isComposing || items.length === 0) return;

    const isNext = e.key === "ArrowDown" || (e.ctrlKey && e.key.toLowerCase() === "n");
    const isPrev = e.key === "ArrowUp" || (e.ctrlKey && e.key.toLowerCase() === "p");

    if (isNext) {
      e.preventDefault();
      keyboardNavRef.current = true;
      setActiveIndex((prev) => (prev + 1) % items.length);
    } else if (isPrev) {
      e.preventDefault();
      keyboardNavRef.current = true;
      setActiveIndex((prev) => (prev - 1 + items.length) % items.length);
    } else if (e.key === "Home" && !e.shiftKey) {
      e.preventDefault();
      keyboardNavRef.current = true;
      setActiveIndex(0);
    } else if (e.key === "End" && !e.shiftKey) {
      e.preventDefault();
      keyboardNavRef.current = true;
      setActiveIndex(items.length - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target = items[activeIndex];
      target?.run();
    }
  };

  if (typeof document === "undefined") return null;

  // 按 section 分组保留全局索引
  const groupedSections: Array<{
    section: string;
    entries: Array<{ item: PaletteItem; idx: number }>;
  }> = [];
  items.forEach((item, idx) => {
    const last = groupedSections[groupedSections.length - 1];
    if (last && last.section === item.section) {
      last.entries.push({ item, idx });
    } else {
      groupedSections.push({ section: item.section, entries: [{ item, idx }] });
    }
  });

  const activeItem = items[activeIndex];
  const activeOptionId = activeItem ? `${listId}-opt-${activeIndex}` : undefined;

  return createPortal(
    <AnimatePresence>
      {open ? (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="快捷聚光灯搜索"
          className="safe-t fixed inset-0 z-50 flex items-start justify-center px-3 pt-[10vh] sm:px-6 sm:pt-[13vh]"
        >
          {/* Backdrop scrim */}
          <motion.div
            aria-hidden
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.18 }}
            onClick={onClose}
            className="fixed inset-0 bg-[var(--scrim)] backdrop-blur-[3px]"
          />

          {/* Spotlight Panel */}
          <motion.div
            initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.96, y: -12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.97, y: -8 }}
            transition={reduced ? { duration: 0.12 } : SPRING_POP}
            onClick={(e) => e.stopPropagation()}
            className="surface-glass relative z-10 flex w-full max-w-[590px] flex-col overflow-hidden rounded-2xl"
          >
            {/* Frameless large search input */}
            <div className="flex items-center gap-3 border-b border-line px-4 py-3.5 sm:px-5">
              <Search className="size-4.5 shrink-0 text-accent/80" aria-hidden />
              <input
                ref={inputRef}
                type="text"
                role="combobox"
                aria-expanded="true"
                aria-controls={listId}
                aria-activedescendant={activeOptionId}
                aria-autocomplete="list"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder="搜索书架藏书、全网书源、命令或纸色…"
                aria-label="搜索书架藏书、全网书源、命令或纸色"
                className="min-w-0 flex-1 bg-transparent text-[15px] text-ink outline-none placeholder:text-ink-muted/70"
              />
              {query ? (
                <button
                  type="button"
                  onClick={() => {
                    setQuery("");
                    inputRef.current?.focus();
                  }}
                  aria-label="清空输入"
                  className="flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-full bg-subtle text-ink-muted transition-colors hover:bg-hover hover:text-ink"
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
              <kbd className="kbd-chip hidden shrink-0 sm:inline-flex">ESC</kbd>
            </div>

            {/* Results & Commands List */}
            <div
              ref={listRef}
              id={listId}
              role="listbox"
              aria-label="快捷搜索结果"
              className="max-h-[min(60dvh,430px)] overflow-y-auto overscroll-contain scroll-py-2 px-2 py-2"
            >
              {groupedSections.map((group, groupIdx) => (
                <div
                  key={group.section}
                  role="group"
                  aria-label={group.section}
                  className={cn(groupIdx > 0 && "mt-2 border-t border-line/70 pt-2")}
                >
                  <div className="flex items-center justify-between px-3 pt-1 pb-1.5 text-[10.5px] font-semibold tracking-[0.14em] text-ink-muted select-none">
                    <span className="inline-flex items-center gap-1.5">
                      {renderSectionIcon(group.section)}
                      <span>{group.section}</span>
                    </span>
                    <span className="font-mono text-[10px] font-normal tabular-nums opacity-65">
                      {group.entries.length}
                    </span>
                  </div>
                  <div className="flex flex-col gap-0.5">
                    {group.entries.map(({ item, idx }) => {
                      const active = idx === activeIndex;
                      return (
                        <button
                          key={item.id}
                          id={`${listId}-opt-${idx}`}
                          type="button"
                          role="option"
                          aria-selected={active}
                          data-palette-idx={idx}
                          onMouseMove={() => {
                            if (activeIndex !== idx) {
                              keyboardNavRef.current = false;
                              setActiveIndex(idx);
                            }
                          }}
                          onClick={() => item.run()}
                          className={cn(
                            "relative flex w-full cursor-pointer items-center rounded-xl px-3 py-2 text-left outline-none transition-colors duration-100 select-none",
                            active
                              ? "bg-subtle text-ink ring-1 ring-line-strong/65"
                              : "text-ink-secondary hover:bg-subtle/55",
                          )}
                        >
                          {/* 左侧朱砂选中指示条 */}
                          <span
                            aria-hidden
                            className={cn(
                              "pointer-events-none absolute top-2 bottom-2 left-1 w-[2.5px] rounded-full bg-accent transition-opacity duration-150",
                              active ? "opacity-100" : "opacity-0",
                            )}
                          />
                          {item.render(active)}
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>

            {/* Subtle keyboard hint footer */}
            <div className="hidden items-center justify-between border-t border-line bg-elevated/50 px-4 py-2 text-[11px] text-ink-muted sm:flex">
              <div className="flex items-center gap-3.5">
                <span className="inline-flex items-center gap-1">
                  <kbd className="kbd-chip">↑</kbd>
                  <kbd className="kbd-chip">↓</kbd>
                  <span className="ml-0.5">选择</span>
                </span>
                <span className="inline-flex items-center gap-1">
                  <kbd className="kbd-chip">↵</kbd>
                  <span className="ml-0.5">直达</span>
                </span>
                <span className="inline-flex items-center gap-1">
                  <kbd className="kbd-chip">ESC</kbd>
                  <span className="ml-0.5">关闭</span>
                </span>
              </div>
              <span className="inline-flex items-center gap-1.5">
                <kbd className="kbd-chip">⌘K</kbd>
                <span className="opacity-50">/</span>
                <kbd className="kbd-chip">Ctrl K</kbd>
                <span>随时唤出</span>
              </span>
            </div>
          </motion.div>
        </div>
      ) : null}
    </AnimatePresence>,
    document.body,
  );
}
