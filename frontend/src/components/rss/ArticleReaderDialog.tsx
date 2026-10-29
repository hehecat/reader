import { CircleAlert, ExternalLink, FileText } from "lucide-react";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  Spinner,
} from "@/components/ui";
import { sanitizeChapterHtml } from "@/hooks/useChapterContent";
import { decorateReaderImages } from "@/lib/readerImages";
import type { RssArticle, RssSource } from "@/services/rss";

import { rssErrorMessage, useRssArticleContent } from "./useRss";

export interface ArticleReaderDialogProps {
  /** 文章所属订阅; null 表示未打开 */
  source: RssSource | null;
  /** 阅读的文章; null 表示未打开 */
  article: RssArticle | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** 发布时间 → "YYYY-MM-DD HH:mm"; 解析不了时原样展示 */
export function formatPubDate(value: string): string {
  if (value.length === 0) return "";
  const time = Date.parse(value);
  if (Number.isNaN(time)) return value;
  const date = new Date(time);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/**
 * 文章阅读对话框: 正文来自 /getRssContent(或文章自带 content), 经 DOMPurify 清洗后
 * 复用阅读器排版变量 (--reader-*) 渲染; md 以下全屏, md 以上居中卡片.
 */
export function ArticleReaderDialog({
  source,
  article,
  open,
  onOpenChange,
}: ArticleReaderDialogProps) {
  const content = useRssArticleContent(source?.sourceUrl ?? "", open ? article : null);

  // 相对图片以文章原文地址为基准解析(经封面代理), 与阅读器同一套清洗管线
  const baseUrl = React.useMemo(() => {
    if (article === null) return undefined;
    try {
      return new URL(article.link, article.origin).href;
    } catch {
      return article.origin.length > 0 ? article.origin : undefined;
    }
  }, [article]);

  const sanitized = React.useMemo(
    () => (content.content.length > 0 ? sanitizeChapterHtml(content.content, baseUrl) : ""),
    [content.content, baseUrl],
  );

  // 正文里的远程图片包一层带 aspect-ratio 的容器, 占位高度先撑好, 加载完不再把下文顶走 (CLS)
  const articleRef = React.useRef<HTMLElement | null>(null);
  React.useEffect(() => {
    const host = articleRef.current;
    if (host === null) {
      return;
    }
    return decorateReaderImages(host);
  }, [sanitized]);

  const title = article !== null && article.title.length > 0 ? article.title : "文章";
  const pubDate = article !== null ? formatPubDate(article.pubDate) : "";
  const link = article?.link ?? "";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="lg">
        <DialogHeader>
          <DialogTitle className="line-clamp-3">{title}</DialogTitle>
          <DialogDescription>
            {pubDate.length > 0 ? `${pubDate} · ` : ""}
            {source?.sourceName ?? ""}
          </DialogDescription>
        </DialogHeader>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 md:px-5">
          {content.isLoading ? (
            <div className="flex flex-col items-center justify-center gap-2 py-16 text-sm text-muted-foreground">
              <Spinner />
              正在加载正文…
            </div>
          ) : content.error !== null ? (
            <EmptyState
              compact
              icon={<CircleAlert aria-hidden />}
              title="正文加载失败"
              description={rssErrorMessage(content.error, "网络异常或订阅正文规则无效")}
              action={
                <Button size="sm" variant="secondary" onClick={content.refetch}>
                  重试
                </Button>
              }
            />
          ) : sanitized.length === 0 ? (
            <EmptyState
              compact
              icon={<FileText aria-hidden />}
              title="没有解析到正文"
              description={
                article !== null && article.description.length > 0
                  ? article.description
                  : "该订阅未配置正文规则 (ruleContent), 可通过下方按钮打开原文页面"
              }
            />
          ) : (
            <article
              ref={articleRef}
              className="rss-article-content mx-auto w-full max-w-(--reader-content-width) py-2 text-foreground [&_h1]:mt-5 [&_h1]:mb-2 [&_h1]:text-xl [&_h1]:font-semibold [&_h2]:mt-5 [&_h2]:mb-2 [&_h2]:text-lg [&_h2]:font-semibold [&_h3]:mt-4 [&_h3]:mb-2 [&_h3]:font-semibold [&_img]:my-3 [&_img]:rounded-lg [&_p]:mb-(--reader-paragraph-gap) [&_p:last-child]:mb-0"
              style={{
                fontSize: "var(--reader-font-size)",
                lineHeight: "var(--reader-line-height)",
              }}
              dangerouslySetInnerHTML={{ __html: sanitized }}
            />
          )}
        </div>

        {/* DialogFooter 默认移动端 col-reverse(主操作在上), 会让 [查看原文][关闭] 在断点两侧换序; 这里锁定为 DOM 同序堆叠 */}
        <DialogFooter className="flex-col">
          {link.length > 0 ? (
            <Button variant="secondary" asChild>
              <a href={link} target="_blank" rel="noreferrer noopener">
                <ExternalLink aria-hidden />
                查看原文
              </a>
            </Button>
          ) : null}
          <DialogClose asChild>
            <Button variant="ghost">关闭</Button>
          </DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
