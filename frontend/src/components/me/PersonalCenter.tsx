import { BookMarked, BookOpen, ChevronRight, Clock3, Layers, Library } from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import {
  Button,
  CoverImage,
  EmptyState,
  PageIntro,
  SettingCard,
  SettingRow,
  Skeleton,
} from "@/components/ui";
import { bookCover, errorMessage, readerPath, useBookshelf } from "@/hooks/useBookshelf";
import { effectiveTotalChapterNum } from "@/lib/chapterCount";
import type { Book } from "@/types/api";

/** 最近阅读展示条数 */
const RECENT_COUNT = 3;

/** 读完: 目录已拉取且进度推到最后一章(与书架详情弹窗的「已读完」判定一致) */
function isFinished(book: Book): boolean {
  const total = effectiveTotalChapterNum(book);
  return total > 0 && book.durChapterIndex >= total - 1;
}

/** 在读: 有阅读记录(时长或章节进度)且尚未读到最后一章 */
function isReading(book: Book): boolean {
  return !isFinished(book) && (book.durChapterTime > 0 || book.durChapterIndex > 0);
}

/** 待读章节: 与 BookCard 的未读角标一致, 总章节数 - 1 - 当前章节 */
function unreadChapters(book: Book): number {
  return Math.max(0, effectiveTotalChapterNum(book) - 1 - book.durChapterIndex);
}

/** 阅读进度百分比(0-100, 向下取整): 总章节数未知时按 0 计 */
function progressPercent(book: Book): number {
  const total = effectiveTotalChapterNum(book);
  if (total <= 0) {
    return 0;
  }
  const percent = ((book.durChapterIndex + 1) / total) * 100;
  return Math.min(100, Math.max(0, Math.floor(percent)));
}

/** 是否已经开读: 用来把「读了一点但不足 1%」和「还没开始」区分开 */
function hasStarted(book: Book): boolean {
  return book.durChapterIndex > 0 || (book.durChapterTitle?.trim() ?? "") !== "";
}

/** 最近阅读条目: 封面 + 衬线书名 + 章节 + 进度条, 点击进入阅读器续读 */
function RecentBook({ book }: { book: Book }) {
  const navigate = useNavigate();
  const percent = progressPercent(book);
  const total = effectiveTotalChapterNum(book);
  const started = hasStarted(book);

  return (
    <button
      type="button"
      onClick={() => navigate(readerPath(book.bookUrl, book.durChapterIndex))}
      className="flex w-full cursor-pointer items-center gap-4 rounded-lg px-3 py-3 text-left outline-none transition-colors hover:bg-surface-muted focus-visible:ring-2 focus-visible:ring-ring"
    >
      <CoverImage
        src={bookCover(book)}
        alt={book.name}
        className="w-12 shrink-0 ring-1 ring-border/60"
      />
      <div className="min-w-0 flex-1">
        <p className="font-display truncate text-sm font-semibold">{book.name}</p>
        <p className="mt-0.5 truncate text-xs text-muted-foreground">
          {book.durChapterTitle?.trim() ||
            (book.durChapterIndex > 0 ? `第 ${book.durChapterIndex + 1} 章` : "尚未开始阅读")}
          {total > 0
            ? book.durChapterTitle?.trim() || book.durChapterIndex > 0
              ? ` · ${book.durChapterIndex + 1} / ${total} 章`
              : ` · 共 ${total} 章`
            : ""}
        </p>
        <div
          role="progressbar"
          aria-valuenow={percent}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label={`《${book.name}》阅读进度`}
          className="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-muted"
        >
          {/* 不足 1% 也要留一截, 否则开读过和没开读看起来一样 */}
          <div
            className="h-full rounded-full bg-accent"
            style={{ width: `${percent > 0 ? Math.max(percent, 3) : started ? 3 : 0}%` }}
          />
        </div>
      </div>
      <span className="w-10 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
        {percent === 0 && started ? "<1%" : `${percent}%`}
      </span>
    </button>
  );
}

/**
 * 个人中心(移植自砚台原型 PersonalCenter):
 * 统计卡(藏书/读完/待读章节/在读书籍) + 最近阅读 top3 + 数据与备份(链设置页) + 关于.
 */
export function PersonalCenter() {
  const navigate = useNavigate();
  const { books, isLoading, error, refetch } = useBookshelf();

  const stats = React.useMemo(
    () => [
      { key: "total", label: "藏书", value: books.length, icon: Library },
      { key: "finished", label: "读完", value: books.filter(isFinished).length, icon: BookMarked },
      {
        key: "unread",
        label: "待读章节",
        value: books.reduce((sum, book) => sum + unreadChapters(book), 0),
        icon: Layers,
      },
      // 「在读」排除已读完的: 否则同一本书既算读完又算在读
      {
        key: "reading",
        label: "在读",
        value: books.filter((book) => isReading(book) && !isFinished(book)).length,
        icon: BookOpen,
      },
    ],
    [books],
  );

  // useBookshelf 返回的 books 已按 durChapterTime 降序, 取有阅读记录的前几本即「最近阅读」
  const recent = React.useMemo(
    () => books.filter((book) => book.durChapterTime > 0).slice(0, RECENT_COUNT),
    [books],
  );

  return (
    <div className="mx-auto w-full max-w-5xl px-4 pb-10 pt-6 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="MY DATA"
        title="我的"
        desc="收纳你的阅读足迹: 藏书统计、最近阅读进度与数据备份。"
      />
      {isLoading ? (
        <div aria-hidden className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          {Array.from({ length: 4 }, (_, index) => (
            <Skeleton key={index} shape="rect" className="h-20" />
          ))}
        </div>
      ) : error !== null ? (
        <EmptyState
          icon={<Library aria-hidden />}
          title="书架数据加载失败"
          description={errorMessage(error, "网络异常或登录态已失效")}
          action={
            <Button size="sm" variant="secondary" onClick={() => refetch()}>
              重试
            </Button>
          }
        />
      ) : (
        <div className="space-y-5">
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            {stats.map(({ key, label, value, icon: Icon }) => (
              <div
                key={key}
                className="flex items-center gap-3 rounded-lg border border-border bg-surface p-3 sm:p-4"
              >
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent/10 text-accent">
                  <Icon className="size-4" aria-hidden />
                </span>
                <div className="min-w-0">
                  <p className="text-xs text-muted-foreground">{label}</p>
                  <p className="mt-1 font-display text-xl font-semibold tabular-nums">{value}</p>
                </div>
              </div>
            ))}
          </div>
          {/* grid-cols-1: 显式声明列宽, 否则隐式 auto 轨道会被子项 nowrap 内容撑到超出内容区 */}
          <div className="grid grid-cols-1 items-start gap-5 lg:grid-cols-2">
            <section className="rounded-xl border border-border bg-surface p-5">
              <h3 className="flex items-center gap-2 font-display text-lg font-semibold">
                <Clock3 className="size-4 text-accent" aria-hidden />
                最近阅读
              </h3>
              {recent.length === 0 ? (
                <p className="py-6 text-center text-sm text-muted-foreground">
                  还没有阅读记录，去书架挑一本开始吧
                </p>
              ) : (
                <div className="mt-2 divide-y divide-border/70">
                  {recent.map((book) => (
                    <RecentBook key={book.bookUrl} book={book} />
                  ))}
                </div>
              )}
            </section>
            <div className="space-y-5">
              <SettingCard title="数据与备份" desc="WebDAV 同步、缓存与本地数据统一在设置页管理">
                <SettingRow label="备份与恢复" value="WebDAV 同步 · 缓存清理 · 数据管理">
                  <Button size="sm" variant="secondary" onClick={() => navigate("/settings")}>
                    前往设置
                    <ChevronRight aria-hidden />
                  </Button>
                </SettingRow>
              </SettingCard>
              <SettingCard title="关于" desc="砚台 · 在线小说阅读">
                <SettingRow label="书源" value="兼容 legado 规则, 可导入 .json 书源" />
                <SettingRow label="数据" value="书架与进度保存在你自己的服务器上" />
              </SettingCard>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
