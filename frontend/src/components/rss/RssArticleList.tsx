import { ChevronLeft, CircleAlert, Inbox, RotateCw } from "lucide-react";
import * as React from "react";

import { formatPubDate } from "./ArticleReaderDialog";
import { rssErrorMessage, useRssArticles } from "./useRss";
import {
  Button,
  EmptyState,
  IconButton,
  SkeletonList,
  Tabs,
  TabsList,
  TabsTrigger,
  cn,
} from "@/components/ui";
import { parseSortUrls, type RssArticle, type RssSource } from "@/services/rss";

export interface RssArticleListProps {
  /** 选中的订阅 */
  source: RssSource;
  /** 本次会话已打开过的文章链接(标题降饱和展示) */
  readLinks: ReadonlySet<string>;
  onOpenArticle: (article: RssArticle) => void;
  /** 移动端返回订阅列表(桌面隐藏) */
  onBack: () => void;
}

/** 文章缩略图: 直接热链原站 (warp 没有 /reader3/cover 代理); 加载失败时整体隐藏 */
function ArticleThumb({ src, alt }: { src: string; alt: string }) {
  const [failed, setFailed] = React.useState(false);
  if (failed) return null;
  return (
    <img
      src={src}
      alt={alt}
      loading="lazy"
      decoding="async"
      onError={() => setFailed(true)}
      className="h-16 w-24 shrink-0 rounded-lg bg-surface-muted object-cover"
    />
  );
}

/** 状态行的更新时间: 当天只给 HH:mm, 更早带上 MM-DD(免得把几天前的更新读成刚刚) */
function formatUpdated(time: number): string {
  const date = new Date(time);
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  const clock = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  return sameDay ? clock : `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${clock}`;
}

/**
 * 选中订阅的文章面板: 移动端自带返回条, 分类(sortUrl)多于一个时出 Tabs;
 * 列表项 = 标题/摘要/发布时间 + 右侧缩略图; 分页由 ruleNextPage 驱动(「加载更多」按钮).
 */
export function RssArticleList({ source, readLinks, onOpenArticle, onBack }: RssArticleListProps) {
  const sorts = React.useMemo(() => parseSortUrls(source), [source]);
  const [activeSort, setActiveSort] = React.useState(0);

  // 切换订阅时回到第一个分类
  React.useEffect(() => {
    setActiveSort(0);
  }, [source.sourceUrl]);

  const sort = sorts.length > 0 ? (sorts[Math.min(activeSort, sorts.length - 1)] ?? null) : null;
  const {
    articles,
    isLoading,
    isFetchingNextPage,
    fetchedAt,
    error,
    refetch,
    hasMore,
    fetchNextPage,
  } = useRssArticles(source, sort);

  const name = source.sourceName.length > 0 ? source.sourceName : source.sourceUrl;

  // 轻量状态: 篇数 + 最近更新时间(首篇 pubDate 优先, 解析不了退到抓取完成时间)
  const status = React.useMemo(() => {
    const newest = articles[0];
    if (newest === undefined || fetchedAt === 0) return null;
    const published = Date.parse(newest.pubDate);
    const time = Number.isNaN(published) ? fetchedAt : published;
    return `${articles.length} 篇 · 更新于 ${formatUpdated(time)}`;
  }, [articles, fetchedAt]);

  return (
    <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-border bg-surface">
      <header className="flex shrink-0 items-center gap-1.5 border-b border-border/70 px-3 py-2.5">
        <IconButton variant="ghost" aria-label="返回订阅列表" tooltip="返回" className="md:hidden" onClick={onBack}>
          <ChevronLeft aria-hidden />
        </IconButton>
        <div className="min-w-0 flex-1">
          <h2 className="truncate font-display text-base font-semibold" title={name}>
            {name}
          </h2>
          {status !== null ? (
            <p className="mt-0.5 truncate text-xs text-muted-foreground tabular-nums">{status}</p>
          ) : null}
        </div>
        <IconButton
          variant="ghost"
          aria-label="刷新文章列表"
          tooltip="刷新"
          disabled={isLoading}
          onClick={refetch}
        >
          <RotateCw aria-hidden />
        </IconButton>
      </header>

      {sorts.length > 1 ? (
        <Tabs
          value={String(activeSort)}
          onValueChange={(value) => setActiveSort(Number(value))}
          className="shrink-0 border-b border-border/70 px-3 py-2.5"
        >
          <TabsList>
            {sorts.map((item, index) => (
              <TabsTrigger key={`${item.name}-${item.url}`} value={String(index)}>
                {item.name}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      ) : null}

      {isLoading ? (
        <SkeletonList count={5} className="p-4" />
      ) : error !== null ? (
        <EmptyState
          compact
          icon={<CircleAlert aria-hidden />}
          title="文章列表加载失败"
          description={rssErrorMessage(error, "订阅地址不可达或解析规则无效")}
          action={
            <Button size="sm" variant="secondary" onClick={refetch}>
              <RotateCw aria-hidden />
              重试
            </Button>
          }
          className="m-3 rounded-xl border border-dashed border-border py-10"
        />
      ) : articles.length === 0 ? (
        <EmptyState
          compact
          icon={<Inbox aria-hidden />}
          title="没有解析到文章"
          description="检查订阅地址是否可访问, 或在编辑里调整列表规则 (ruleArticles)"
          className="m-3 rounded-xl border border-dashed border-border py-10"
        />
      ) : (
        <>
          <ul className="divide-y divide-border/70">
            {articles.map((article, index) => {
              const read = article.link.length > 0 && readLinks.has(article.link);
              // 后端图片规则为空时会把 image 回落成 Feed 地址, 与源同址的图不展示
              const image =
                article.image.length > 0 && article.image !== source.sourceUrl ? article.image : null;
              return (
                <li key={article.link.length > 0 ? article.link : `${article.title}-${index}`}>
                  <button
                    type="button"
                    className="flex w-full cursor-pointer items-center gap-3 px-4 py-3 text-left outline-none transition-colors duration-150 ease-out hover:bg-surface-muted/60 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
                    onClick={() => onOpenArticle(article)}
                  >
                    {/* 未读圆点: 打开过的文章降为边框色, 标题同步降饱和 */}
                    <span
                      aria-hidden
                      className={cn("size-1.5 shrink-0 rounded-full", read ? "bg-border" : "bg-accent")}
                    />
                    <span className="min-w-0 flex-1">
                      <span
                        className={cn(
                          "line-clamp-2 block text-sm",
                          read ? "text-muted-foreground" : "font-medium",
                        )}
                      >
                        <span className="sr-only">{read ? "已读: " : "未读: "}</span>
                        {article.title.length > 0 ? article.title : "(无标题)"}
                      </span>
                      {article.description.length > 0 ? (
                        <span className="mt-1 line-clamp-2 block text-xs leading-5 text-muted-foreground">
                          {article.description}
                        </span>
                      ) : null}
                      {article.pubDate.length > 0 ? (
                        <span className="mt-1.5 block text-xs text-muted-foreground tabular-nums">
                          {formatPubDate(article.pubDate)}
                        </span>
                      ) : null}
                    </span>
                    {image !== null ? <ArticleThumb src={image} alt={article.title} /> : null}
                  </button>
                </li>
              );
            })}
          </ul>
          {hasMore ? (
            <div className="shrink-0 border-t border-border/70 p-3">
              <Button
                variant="secondary"
                className="w-full"
                loading={isFetchingNextPage}
                onClick={fetchNextPage}
              >
                加载更多
              </Button>
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
