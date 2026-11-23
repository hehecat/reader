import { useQuery } from "@tanstack/react-query";
import {
  BookCheck,
  BookMarked,
  BookOpen,
  BookText,
  ChevronDown,
  ChevronRight,
  Clock3,
  Footprints,
  HardDrive,
  Layers,
  ListOrdered,
  Play,
  Settings2,
  Sparkles,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { booksApi } from "@/api/endpoints";
import type { Book } from "@/api/types";
import { BookInspectorSheet } from "@/features/book/BookInspectorSheet";
import { cn } from "@/lib/cn";
import {
  buildReaderUrl,
  cleanChapterTitle,
  formatRelativeTime,
  formatUptime,
} from "@/lib/format";
import { useAuthStore } from "@/stores/auth";
import { BookCover } from "@/ui/BookCover";
import { Button } from "@/ui/Button";
import { Badge, EmptyView, Segmented } from "@/ui/Controls";
import { motion, PageEnter, SPRING_SILK } from "@/ui/motion";

function getBookCover(book: Book): string | null | undefined {
  return book.customCoverUrl || book.displayCover || book.coverUrl;
}

function isFinished(book: Book): boolean {
  return book.totalChapterNum > 0 && book.durChapterIndex >= book.totalChapterNum - 1;
}

function isStarted(book: Book): boolean {
  return book.durChapterTime > 0 || book.durChapterIndex > 0;
}

function isReading(book: Book): boolean {
  return !isFinished(book) && isStarted(book);
}

function progressPercent(book: Book): number {
  if (book.totalChapterNum <= 0) return 0;
  return Math.min(
    100,
    Math.max(0, Math.floor(((book.durChapterIndex + 1) / book.totalChapterNum) * 100)),
  );
}

function startOfDayMs(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

function formatShortDate(ms: number): string {
  const d = new Date(ms);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

type FootprintFilter = "all" | "reading" | "finished";
type TimeBucket = "today" | "week" | "earlier";

const BUCKET_META: Record<TimeBucket, { label: string; dot: string }> = {
  today: { label: "今天", dot: "bg-accent" },
  week: { label: "近 7 天", dot: "bg-warn" },
  earlier: { label: "更早足迹", dot: "bg-ink-muted/50" },
};

const ONE_DAY_MS = 86_400_000;

export default function MePage() {
  const navigate = useNavigate();
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);

  const [footprintFilter, setFootprintFilter] = React.useState<FootprintFilter>("all");
  const [footprintExpanded, setFootprintExpanded] = React.useState(false);
  const [inspected, setInspected] = React.useState<{
    book: Book;
    tab: "overview" | "toc" | "sources";
  } | null>(null);

  const { data: shelf = [], isLoading } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(false),
    staleTime: 30_000,
  });

  const stats = React.useMemo(() => {
    const now = Date.now();
    const weekAgo = now - 7 * ONE_DAY_MS;
    let reading = 0;
    let finished = 0;
    let notStarted = 0;
    let unread = 0;
    let readChapters = 0;
    let activeThisWeek = 0;

    for (const b of shelf) {
      if (isFinished(b)) finished += 1;
      else if (isReading(b)) reading += 1;
      else notStarted += 1;

      if (b.totalChapterNum > 0) {
        unread += Math.max(0, b.totalChapterNum - 1 - b.durChapterIndex);
      }
      if (isStarted(b)) {
        readChapters += Math.max(1, b.durChapterIndex + 1);
      }
      if (b.durChapterTime >= weekAgo) {
        activeThisWeek += 1;
      }
    }

    const finishedRate = shelf.length > 0 ? Math.round((finished / shelf.length) * 100) : 0;
    return {
      total: shelf.length,
      reading,
      finished,
      notStarted,
      unread,
      readChapters,
      activeThisWeek,
      finishedRate,
    };
  }, [shelf]);

  // 按最近阅读时间排序
  const sortedByRecent = React.useMemo(() => {
    return [...shelf].sort(
      (a, b) => (b.durChapterTime || b.createdAt || 0) - (a.durChapterTime || a.createdAt || 0),
    );
  }, [shelf]);

  // 快捷续读：优先选最近正在阅读的书作为主推，其余展示为快捷续读胶囊
  const { heroBook, quickResumeBooks } = React.useMemo(() => {
    if (sortedByRecent.length === 0) {
      return { heroBook: null as Book | null, quickResumeBooks: [] as Book[] };
    }
    const primary = sortedByRecent.find((b) => isReading(b)) ?? sortedByRecent[0] ?? null;
    const others = sortedByRecent
      .filter((b) => b.bookUrl !== primary?.bookUrl)
      .slice(0, 3);
    return { heroBook: primary, quickResumeBooks: others };
  }, [sortedByRecent]);

  // 近 14 天每日阅读足迹活跃度
  const activityDays = React.useMemo(() => {
    const todayStart = startOfDayMs(new Date());
    const countByDay = new Map<number, number>();
    for (const b of shelf) {
      if (!b.durChapterTime || b.durChapterTime <= 0) continue;
      const dayStart = startOfDayMs(new Date(b.durChapterTime));
      countByDay.set(dayStart, (countByDay.get(dayStart) ?? 0) + 1);
    }

    const days: Array<{ dayStart: number; label: string; count: number; isToday: boolean }> = [];
    for (let offset = 13; offset >= 0; offset -= 1) {
      const dayStart = todayStart - offset * ONE_DAY_MS;
      days.push({
        dayStart,
        label: formatShortDate(dayStart),
        count: countByDay.get(dayStart) ?? 0,
        isToday: offset === 0,
      });
    }
    const activeDays14 = days.filter((d) => d.count > 0).length;
    const activeDays7 = days.slice(-7).filter((d) => d.count > 0).length;
    return { days, activeDays14, activeDays7 };
  }, [shelf]);

  // 时间轴阅读足迹（按状态筛选 + 时间分段）
  const footprintGroups = React.useMemo(() => {
    const todayStart = startOfDayMs(new Date());
    const weekStart = todayStart - 6 * ONE_DAY_MS;

    const touched = sortedByRecent.filter((b) => isStarted(b));
    const source = touched.length > 0 ? touched : sortedByRecent;
    const filtered = source.filter((b) => {
      if (footprintFilter === "reading") return isReading(b);
      if (footprintFilter === "finished") return isFinished(b);
      return true;
    });

    const limit = footprintExpanded ? filtered.length : 6;
    const visible = filtered.slice(0, limit);

    const map: Record<TimeBucket, Book[]> = { today: [], week: [], earlier: [] };
    for (const b of visible) {
      const ts = b.durChapterTime || b.createdAt || 0;
      if (ts >= todayStart) map.today.push(b);
      else if (ts >= weekStart) map.week.push(b);
      else map.earlier.push(b);
    }

    const buckets = (["today", "week", "earlier"] as const)
      .filter((key) => map[key].length > 0)
      .map((key) => ({ key, books: map[key] }));

    return {
      buckets,
      totalCount: filtered.length,
      hasMore: filtered.length > 6,
    };
  }, [sortedByRecent, footprintFilter, footprintExpanded]);

  const openReader = (book: Book) => {
    navigate(buildReaderUrl(book.bookUrl, book.durChapterIndex));
  };

  const heroPct = heroBook ? progressPercent(heroBook) : 0;
  const heroRemaining =
    heroBook && heroBook.totalChapterNum > 0
      ? Math.max(0, heroBook.totalChapterNum - 1 - heroBook.durChapterIndex)
      : 0;
  const heroStarted = heroBook ? isStarted(heroBook) : false;

  return (
    <PageEnter className="mx-auto flex w-full max-w-5xl flex-col gap-5 px-4 pt-3 pb-12 sm:px-6 md:gap-6 md:px-8 md:pt-7">
      {/* PageIntro (Mobile hides redundant title already shown in AppShell top bar) */}
      <section className="flex items-center justify-between gap-3 border-b border-line/70 pb-3 md:flex-row md:items-end md:border-line md:pb-4">
        <div className="min-w-0">
          <p className="mb-1.5 hidden text-xs font-medium tracking-[0.18em] text-accent uppercase md:block">
            MY READING SPACE
          </p>
          <div className="flex flex-wrap items-center gap-2.5">
            <h1 className="hidden font-serif text-2xl font-semibold tracking-wide text-ink md:block md:text-[26px]">
              我的
            </h1>
            <Badge tone={system?.secure && user ? "accent" : "default"}>
              {system?.secure && user ? user.username : "本地书房"}
            </Badge>
          </div>
          <p className="mt-1.5 hidden text-sm text-ink-muted md:block">
            快捷续读最近书卷、回顾阅读足迹与藏书进度，以及缓存与工具入口。
          </p>
        </div>
        <Button variant="secondary" size="sm" onClick={() => navigate("/settings")}>
          <Settings2 className="size-4" />
          偏好设置
        </Button>
      </section>

      {/* Quick Continue Reading Spotlight */}
      <section className="space-y-3 border-b border-line/70 pb-5 md:border-b-0 md:pb-0">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="h-3.5 w-[3px] rounded-full bg-accent" aria-hidden />
            <h2 className="font-serif text-[15px] font-semibold tracking-wide text-ink">
              快捷续读
            </h2>
            {heroBook && isReading(heroBook) ? (
              <span className="hidden text-xs text-ink-muted sm:inline">一键回到上次停下的书页</span>
            ) : null}
          </div>
          <Button variant="ghost" size="xs" onClick={() => navigate("/")}>
            全部书架
            <ChevronRight className="size-3.5" />
          </Button>
        </div>

        {heroBook ? (
          <div className="md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated md:shadow-[var(--shadow-sm)]">
            {/* Primary Spotlight (borderless editorial row on mobile, elevated card on desktop) */}
            <div className="relative overflow-hidden py-1 md:bg-gradient-to-br md:from-accent-soft/55 md:via-elevated md:to-elevated md:p-5">
              <div
                aria-hidden
                className="pointer-events-none absolute -top-20 -right-16 hidden size-56 rounded-full bg-accent/10 blur-3xl md:block"
              />
              <div className="relative flex items-start gap-4 md:items-center md:gap-5">
                <button
                  type="button"
                  onClick={() => openReader(heroBook)}
                  className="shrink-0 cursor-pointer"
                  title={`继续阅读《${heroBook.name}》`}
                >
                  <BookCover
                    src={getBookCover(heroBook)}
                    referer={heroBook.origin}
                    title={heroBook.name}
                    author={heroBook.author}
                    pageEdges
                    className="w-[74px] sm:w-[92px]"
                  />
                </button>

                <div className="min-w-0 flex-1 space-y-2 text-left md:space-y-2.5">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <Badge tone={isFinished(heroBook) ? "success" : "accent"}>
                      {isFinished(heroBook)
                        ? "已读完"
                        : heroStarted
                          ? "最近在读"
                          : "待开卷"}
                    </Badge>
                    {heroBook.durChapterTime ? (
                      <span className="inline-flex items-center gap-1 text-[11px] text-ink-muted">
                        <Clock3 className="size-3" />
                        {formatRelativeTime(heroBook.durChapterTime)}
                      </span>
                    ) : null}
                    {heroBook.originName ? (
                      <span className="hidden text-[11px] text-ink-muted sm:inline">
                        · {heroBook.originName}
                      </span>
                    ) : null}
                  </div>

                  <div>
                    <h3 className="truncate font-serif text-xl font-semibold tracking-tight text-ink sm:text-[22px]">
                      {heroBook.name}
                    </h3>
                    <p className="mt-0.5 truncate text-xs text-ink-secondary">
                      {heroBook.author || "佚名"}
                      <span className="mx-1.5 text-ink-muted/60">·</span>
                      <span className="text-ink-muted">
                        {cleanChapterTitle(heroBook.durChapterTitle) ||
                          (heroBook.durChapterIndex > 0
                            ? `第 ${heroBook.durChapterIndex + 1} 章`
                            : "尚未开始阅读")}
                      </span>
                    </p>
                  </div>

                  {/* Progress */}
                  <div className="space-y-1.5 pt-0.5">
                    <div className="h-1.5 w-full overflow-hidden rounded-full bg-subtle">
                      <motion.div
                        className={cn(
                          "h-full rounded-full",
                          isFinished(heroBook)
                            ? "bg-success"
                            : "bg-gradient-to-r from-accent/70 to-accent",
                        )}
                        initial={{ width: 0 }}
                        animate={{ width: `${heroStarted ? Math.max(2, heroPct) : 0}%` }}
                        transition={SPRING_SILK}
                      />
                    </div>
                    <div className="flex items-center justify-between gap-2 text-[11px] text-ink-muted tabular-nums">
                      <span>
                        {heroBook.totalChapterNum > 0
                          ? `第 ${Math.min(heroBook.durChapterIndex + 1, heroBook.totalChapterNum)} / ${heroBook.totalChapterNum} 章`
                          : "章节进度同步中"}
                        {heroRemaining > 0 ? ` · 剩余 ${heroRemaining} 章` : ""}
                      </span>
                      <span className="font-mono font-medium text-accent">
                        {heroStarted ? `${heroPct}%` : "0%"}
                      </span>
                    </div>
                  </div>

                  {/* Action Row */}
                  <div className="flex flex-wrap items-center gap-2 pt-0.5 md:pt-1">
                    <Button
                      variant="primary"
                      size="sm"
                      onClick={() => openReader(heroBook)}
                    >
                      <Play className="size-3.5 fill-current" />
                      {heroStarted ? "继续阅读" : "立即开卷"}
                    </Button>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => setInspected({ book: heroBook, tab: "toc" })}
                    >
                      <ListOrdered className="size-3.5" />
                      目录
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setInspected({ book: heroBook, tab: "overview" })}
                    >
                      详情
                    </Button>
                  </div>
                </div>
              </div>
            </div>

            {/* Secondary Quick-Resume Strip */}
            {quickResumeBooks.length > 0 ? (
              <div className="mt-3 grid grid-cols-1 divide-y divide-line/70 border-t border-line/70 md:mt-0 md:grid-cols-3 md:divide-x md:divide-y-0 md:divide-line md:border-line md:bg-subtle/25">
                {quickResumeBooks.map((book) => {
                  const pct = progressPercent(book);
                  const started = isStarted(book);
                  return (
                    <button
                      key={book.bookUrl}
                      type="button"
                      onClick={() => openReader(book)}
                      className="group flex min-w-0 cursor-pointer items-center gap-3 px-0.5 py-3 text-left transition-colors hover:bg-subtle/40 md:px-4 md:hover:bg-elevated/90"
                    >
                      <BookCover
                        src={getBookCover(book)}
                        referer={book.origin}
                        title={book.name}
                        author={book.author}
                        interactive={false}
                        className="w-9 shrink-0"
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center justify-between gap-1.5">
                          <p className="truncate font-serif text-[13px] font-semibold text-ink transition-colors group-hover:text-accent">
                            {book.name}
                          </p>
                          <span className="shrink-0 font-mono text-[10.5px] text-ink-muted tabular-nums">
                            {started ? `${pct}%` : "未读"}
                          </span>
                        </div>
                        <p className="mt-0.5 truncate text-[11px] text-ink-muted">
                          {cleanChapterTitle(book.durChapterTitle) ||
                            (book.durChapterIndex > 0
                              ? `第 ${book.durChapterIndex + 1} 章`
                              : "尚未开始阅读")}
                        </p>
                        <div className="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-subtle">
                          <div
                            className={cn(
                              "h-full rounded-full transition-[width]",
                              isFinished(book) ? "bg-success" : "bg-accent/80",
                            )}
                            style={{ width: `${started ? Math.max(3, pct) : 0}%` }}
                          />
                        </div>
                      </div>
                      <span className="flex size-7 shrink-0 items-center justify-center rounded-full bg-subtle text-ink-muted transition-colors group-hover:bg-accent-soft group-hover:text-accent">
                        <Play className="size-3 fill-current" />
                      </span>
                    </button>
                  );
                })}
              </div>
            ) : null}
          </div>
        ) : (
          <div className="md:rounded-2xl md:border md:border-line md:bg-elevated md:shadow-[var(--shadow-sm)]">
            <EmptyView
              compact
              icon={<BookOpen className="size-5" />}
              title="暂无在读书籍"
              description="从书架或发现页挑选一本书，即可在此一键续读"
              action={
                <Button size="sm" variant="primary" onClick={() => navigate("/discover")}>
                  去发现好书
                </Button>
              }
            />
          </div>
        )}
      </section>

      {/* Reading Overview Stats (borderless typographic grid on mobile, elevated cards on desktop) */}
      <section className="space-y-3 border-b border-line/70 pb-5 md:border-b-0 md:pb-0">
        <div className="grid grid-cols-2 gap-x-5 gap-y-4 md:gap-3 lg:grid-cols-4">
          {/* Primary KPI: Currently Reading */}
          <div className="relative overflow-hidden py-1 md:rounded-2xl md:border md:border-accent/25 md:bg-gradient-to-br md:from-accent-soft/55 md:via-elevated md:to-elevated md:p-4 md:shadow-[var(--shadow-sm)]">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs font-medium text-accent">正在阅读</span>
              <span className="hidden size-7 items-center justify-center rounded-lg bg-accent-soft text-accent md:flex">
                <Clock3 className="size-3.5" />
              </span>
            </div>
            <p className="mt-1.5 font-mono text-[26px] leading-none font-semibold text-ink tabular-nums md:mt-2.5 md:text-[28px]">
              {isLoading ? "—" : stats.reading}
              <span className="ml-1 font-sans text-xs font-normal text-ink-muted">本</span>
            </p>
            <p className="mt-1.5 text-[11px] text-ink-muted tabular-nums md:mt-2">
              {isLoading
                ? "统计中…"
                : stats.activeThisWeek > 0
                  ? `近 7 天活跃 ${stats.activeThisWeek} 本`
                  : "保持每日翻阅习惯"}
            </p>
          </div>

          {/* Total Shelf */}
          <div className="py-1 md:rounded-2xl md:border md:border-line md:bg-elevated md:p-4 md:shadow-[var(--shadow-sm)]">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-ink-secondary">书架藏书</span>
              <span className="hidden size-7 items-center justify-center rounded-lg bg-subtle text-ink-secondary md:flex">
                <BookOpen className="size-3.5" />
              </span>
            </div>
            <p className="mt-1.5 font-mono text-[26px] leading-none font-semibold text-ink tabular-nums md:mt-2.5 md:text-[28px]">
              {isLoading ? "—" : stats.total}
              <span className="ml-1 font-sans text-xs font-normal text-ink-muted">本</span>
            </p>
            <p className="mt-1.5 text-[11px] text-ink-muted tabular-nums md:mt-2">
              {isLoading ? "—" : `待开卷 ${stats.notStarted} 本`}
            </p>
          </div>

          {/* Finished */}
          <div className="py-1 md:rounded-2xl md:border md:border-line md:bg-elevated md:p-4 md:shadow-[var(--shadow-sm)]">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-ink-secondary">已读完</span>
              <span className="hidden size-7 items-center justify-center rounded-lg bg-success-soft text-success md:flex">
                <BookCheck className="size-3.5" />
              </span>
            </div>
            <p className="mt-1.5 font-mono text-[26px] leading-none font-semibold text-ink tabular-nums md:mt-2.5 md:text-[28px]">
              {isLoading ? "—" : stats.finished}
              <span className="ml-1 font-sans text-xs font-normal text-ink-muted">本</span>
            </p>
            <p className="mt-1.5 text-[11px] text-ink-muted tabular-nums md:mt-2">
              {isLoading ? "—" : `完读率 ${stats.finishedRate}%`}
            </p>
          </div>

          {/* Unread Chapters + Cumulative Read */}
          <div className="py-1 md:rounded-2xl md:border md:border-line md:bg-elevated md:p-4 md:shadow-[var(--shadow-sm)]">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-ink-secondary">待读章节</span>
              <span className="hidden size-7 items-center justify-center rounded-lg bg-warn-soft text-warn md:flex">
                <Layers className="size-3.5" />
              </span>
            </div>
            <p className="mt-1.5 font-mono text-[26px] leading-none font-semibold text-ink tabular-nums md:mt-2.5 md:text-[28px]">
              {isLoading ? "—" : stats.unread.toLocaleString()}
              <span className="ml-1 font-sans text-xs font-normal text-ink-muted">章</span>
            </p>
            <p className="mt-1.5 text-[11px] text-ink-muted tabular-nums md:mt-2">
              {isLoading ? "—" : `已读 ${stats.readChapters.toLocaleString()} 章`}
            </p>
          </div>
        </div>

        {/* Proportional Shelf Composition Strip */}
        {stats.total > 0 ? (
          <div className="pt-1 md:rounded-xl md:border md:border-line md:bg-elevated/80 md:px-4 md:py-2.5">
            <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-ink-muted">
              <span className="font-medium text-ink-secondary">藏书进度分布</span>
              <div className="flex items-center gap-3 tabular-nums">
                <span className="inline-flex items-center gap-1.5">
                  <span className="size-2 rounded-full bg-accent" />
                  在读 {stats.reading}
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <span className="size-2 rounded-full bg-success" />
                  已读完 {stats.finished}
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <span className="size-2 rounded-full bg-line-strong" />
                  待开卷 {stats.notStarted}
                </span>
              </div>
            </div>
            <div className="mt-2 flex h-1.5 w-full overflow-hidden rounded-full bg-subtle">
              {stats.reading > 0 ? (
                <div
                  className="h-full bg-accent transition-[width]"
                  style={{ width: `${(stats.reading / stats.total) * 100}%` }}
                />
              ) : null}
              {stats.finished > 0 ? (
                <div
                  className="h-full bg-success transition-[width]"
                  style={{ width: `${(stats.finished / stats.total) * 100}%` }}
                />
              ) : null}
              {stats.notStarted > 0 ? (
                <div
                  className="h-full bg-line-strong/70 transition-[width]"
                  style={{ width: `${(stats.notStarted / stats.total) * 100}%` }}
                />
              ) : null}
            </div>
          </div>
        ) : null}
      </section>

      {/* Reading Footprint (14-Day Activity + Chronological Timeline) */}
      <section className="border-b border-line/70 pb-4 md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated md:pb-0 md:shadow-[var(--shadow-sm)]">
        <header className="flex flex-wrap items-center justify-between gap-3 pb-3 md:border-b md:border-line md:px-5 md:py-3.5">
          <div className="flex items-center gap-2.5">
            <span className="hidden size-8 items-center justify-center rounded-xl bg-accent-soft text-accent md:flex">
              <Footprints className="size-4" />
            </span>
            <div>
              <h2 className="font-serif text-[15px] font-semibold text-ink md:font-sans md:text-sm">阅读足迹</h2>
              <p className="text-xs text-ink-muted">
                近 14 天活跃 {activityDays.activeDays14} 天 · 累计翻阅{" "}
                {stats.readChapters.toLocaleString()} 章
              </p>
            </div>
          </div>

          <Segmented
            layoutId="me-footprint-filter"
            size="sm"
            value={footprintFilter}
            onChange={(val) => {
              setFootprintFilter(val);
              setFootprintExpanded(false);
            }}
            options={[
              { value: "all", label: "全部" },
              { value: "reading", label: "在读" },
              { value: "finished", label: "已读完" },
            ]}
          />
        </header>

        {/* 14-day Reading Rhythm Strip */}
        <div className="flex flex-wrap items-center justify-between gap-3 border-y border-line/70 py-2.5 md:border-t-0 md:bg-subtle/30 md:px-5 md:py-3">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-medium text-ink-secondary">近 14 日翻阅节奏</span>
            <span className="text-[11px] text-ink-muted tabular-nums">
              近 7 天 {activityDays.activeDays7} / 7 天
            </span>
          </div>
          <div className="flex items-center gap-1" aria-label="近14天阅读活跃记录">
            {activityDays.days.map((day) => (
              <span
                key={day.dayStart}
                title={`${day.label}${day.isToday ? "（今天）" : ""} · ${day.count > 0 ? `阅读 ${day.count} 本` : "无阅读记录"}`}
                className={cn(
                  "h-4 w-3 rounded-[4px] border transition-transform hover:scale-110 sm:w-3.5",
                  day.count === 0
                    ? "border-line bg-elevated"
                    : day.count === 1
                      ? "border-accent/30 bg-accent/45"
                      : "border-accent/60 bg-accent",
                  day.isToday && "ring-1 ring-accent/50 ring-offset-1 ring-offset-elevated",
                )}
              />
            ))}
          </div>
        </div>

        {/* Chronological Footprint Timeline */}
        {footprintGroups.buckets.length === 0 ? (
          <EmptyView
            compact
            icon={<BookMarked className="size-5" />}
            title={
              footprintFilter === "finished"
                ? "还没有已读完的作品"
                : footprintFilter === "reading"
                  ? "当前没有正在阅读的书籍"
                  : "暂无阅读足迹"
            }
            description="翻开任意一本书阅读后，足迹将按时间自动沉淀在这里"
          />
        ) : (
          <div className="divide-y divide-line/70">
            {footprintGroups.buckets.map((bucket) => (
              <div key={bucket.key} className="py-3.5 md:px-5">
                <div className="mb-2.5 flex items-center gap-2">
                  <span
                    className={cn("size-2 rounded-full", BUCKET_META[bucket.key].dot)}
                    aria-hidden
                  />
                  <span className="text-[11px] font-semibold tracking-wider text-ink-secondary uppercase">
                    {BUCKET_META[bucket.key].label}
                  </span>
                  <span className="text-[11px] text-ink-muted tabular-nums">
                    {bucket.books.length} 本
                  </span>
                </div>

                <div className="relative space-y-1 pl-3 before:absolute before:inset-y-2 before:left-[3px] before:w-px before:bg-line">
                  {bucket.books.map((book) => {
                    const pct = progressPercent(book);
                    const started = isStarted(book);
                    const done = isFinished(book);
                    return (
                      <div
                        key={book.bookUrl}
                        className="group relative flex items-center gap-3 rounded-xl px-2.5 py-2 transition-colors hover:bg-subtle/60"
                      >
                        <button
                          type="button"
                          onClick={() => openReader(book)}
                          className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 text-left"
                        >
                          <BookCover
                            src={getBookCover(book)}
                            referer={book.origin}
                            title={book.name}
                            author={book.author}
                            interactive={false}
                            className="w-10 shrink-0"
                          />
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap items-center gap-1.5">
                              <span className="truncate font-serif text-sm font-semibold text-ink transition-colors group-hover:text-accent">
                                {book.name}
                              </span>
                              {done ? (
                                <Badge tone="success">已读完</Badge>
                              ) : started ? (
                                <Badge tone="accent">{pct}%</Badge>
                              ) : (
                                <Badge>待开卷</Badge>
                              )}
                              {book.durChapterTime ? (
                                <span className="text-[11px] text-ink-muted">
                                  {formatRelativeTime(book.durChapterTime)}
                                </span>
                              ) : null}
                            </div>
                            <p className="mt-0.5 truncate text-xs text-ink-muted">
                              {cleanChapterTitle(book.durChapterTitle) ||
                                (book.durChapterIndex > 0
                                  ? `第 ${book.durChapterIndex + 1} 章`
                                  : "尚未开始阅读")}
                              {book.totalChapterNum > 0
                                ? ` · ${Math.min(book.durChapterIndex + 1, book.totalChapterNum)} / ${book.totalChapterNum} 章`
                                : ""}
                            </p>
                            <div className="mt-1.5 h-1 w-full max-w-xs overflow-hidden rounded-full bg-subtle">
                              <div
                                className={cn(
                                  "h-full rounded-full transition-[width]",
                                  done ? "bg-success" : "bg-accent",
                                )}
                                style={{ width: `${started ? Math.max(2, pct) : 0}%` }}
                              />
                            </div>
                          </div>
                        </button>

                        <div className="flex shrink-0 items-center gap-1">
                          <Button
                            size="xs"
                            variant="subtle"
                            onClick={() => openReader(book)}
                            className="opacity-85 transition-opacity group-hover:opacity-100"
                          >
                            <Play className="size-3 fill-current text-accent" />
                            <span className="hidden sm:inline">{started ? "续读" : "开卷"}</span>
                          </Button>
                          <Button
                            size="xs"
                            variant="ghost"
                            onClick={() => setInspected({ book, tab: "overview" })}
                            aria-label={`查看《${book.name}》详情`}
                            title="书籍详情"
                            className="px-1.5 text-ink-muted"
                          >
                            <ChevronRight className="size-3.5" />
                          </Button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}

            {footprintGroups.hasMore ? (
              <div className="flex justify-center bg-subtle/20 py-2.5">
                <button
                  type="button"
                  onClick={() => setFootprintExpanded((v) => !v)}
                  className="inline-flex cursor-pointer items-center gap-1 rounded-full px-3 py-1 text-xs font-medium text-ink-secondary transition-colors hover:bg-subtle hover:text-ink"
                >
                  {footprintExpanded
                    ? "收起更多足迹"
                    : `展开全部 ${footprintGroups.totalCount} 条足迹`}
                  <ChevronDown
                    className={cn(
                      "size-3.5 transition-transform",
                      footprintExpanded && "rotate-180",
                    )}
                  />
                </button>
              </div>
            ) : null}
          </div>
        )}
      </section>

      {/* Quick Shortcuts & Server Info */}
      <div className="grid grid-cols-1 gap-5 md:grid-cols-2 md:gap-4">
        <section className="divide-y divide-line/70 border-b border-line/70 pb-2 md:divide-line md:rounded-2xl md:border md:border-line md:bg-elevated md:pb-0 md:shadow-[var(--shadow-sm)]">
          <div className="pb-2.5 md:px-5 md:py-3.5">
            <h2 className="font-serif text-[15px] font-semibold text-ink md:font-sans md:text-sm">数据与工具</h2>
          </div>
          {[
            {
              title: "备份与缓存管理",
              desc: "WebDAV 云端备份恢复、章节与封面缓存清理",
              icon: HardDrive,
              to: "/settings?tab=storage",
            },
            {
              title: "书源管理与调试",
              desc: "导入订阅书源、测速筛选与 SSE 规则调试台",
              icon: BookText,
              to: "/studio/sources",
            },
            {
              title: "正文净化规则",
              desc: "管理正则替换规则并实时预览去广告效果",
              icon: Sparkles,
              to: "/studio/purify",
            },
          ].map((row) => {
            const Icon = row.icon;
            return (
              <button
                key={row.to}
                type="button"
                onClick={() => navigate(row.to)}
                className="group flex w-full cursor-pointer items-center justify-between gap-3 py-3.5 text-left transition-colors hover:bg-subtle/60 md:px-5"
              >
                <div className="flex min-w-0 items-center gap-3">
                  <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-subtle text-ink-secondary transition-colors group-hover:bg-accent-soft group-hover:text-accent">
                    <Icon className="size-4" />
                  </span>
                  <div className="min-w-0">
                    <p className="text-[13px] font-medium text-ink">{row.title}</p>
                    <p className="truncate text-xs text-ink-muted">{row.desc}</p>
                  </div>
                </div>
                <ChevronRight className="size-4 shrink-0 text-ink-muted transition-transform group-hover:translate-x-0.5 group-hover:text-ink" />
              </button>
            );
          })}
        </section>

        <section className="flex flex-col justify-between md:rounded-2xl md:border md:border-line md:bg-elevated md:p-5 md:shadow-[var(--shadow-sm)]">
          <div>
            <div className="flex items-center justify-between gap-2">
              <h2 className="text-sm font-semibold text-ink">服务与账户状态</h2>
              <span className="inline-flex items-center gap-1.5 rounded-full bg-success-soft px-2 py-0.5 text-[11px] font-medium text-success">
                <span className="size-1.5 rounded-full bg-success" />
                在线运行
              </span>
            </div>
            <dl className="mt-4 divide-y divide-line/60 text-xs">
              <div className="flex items-center justify-between py-2.5 first:pt-0">
                <dt className="text-ink-muted">当前身份</dt>
                <dd className="font-medium text-ink">
                  {system?.secure && user ? user.username : "单用户本地模式"}
                </dd>
              </div>
              <div className="flex items-center justify-between py-2.5">
                <dt className="text-ink-muted">服务端版本</dt>
                <dd className="font-mono text-ink-secondary">v{system?.version || "6.0"}</dd>
              </div>
              <div className="flex items-center justify-between py-2.5 last:pb-0">
                <dt className="text-ink-muted">运行时长</dt>
                <dd className="font-mono text-ink-secondary tabular-nums">
                  {formatUptime(system?.uptimeSeconds)}
                </dd>
              </div>
            </dl>
          </div>

          <div className="mt-4 border-t border-line pt-3">
            <Button
              variant="ghost"
              size="xs"
              onClick={() => navigate("/settings")}
              className="w-full justify-between px-2 text-ink-secondary"
            >
              <span>管理外观、字体与同步选项</span>
              <ChevronRight className="size-3.5" />
            </Button>
          </div>
        </section>
      </div>

      {/* Inline Book Inspector for Quick Details / TOC from MePage */}
      <BookInspectorSheet
        open={Boolean(inspected)}
        onClose={() => setInspected(null)}
        book={inspected?.book ?? null}
        initialTab={inspected?.tab ?? "overview"}
      />
    </PageEnter>
  );
}
