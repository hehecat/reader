import { BookPlus, Check } from "lucide-react";
import type { ReactNode } from "react";

import { Badge, CoverImage, IconButton, cn } from "@/components/ui";
import { chapterTitleText } from "@/lib/text";
import { useAddToShelf } from "@/hooks/useAddToShelf";
import { getCoverUrl } from "@/services/book";
import type { Book, SearchBook } from "@/types/api";

export interface SearchResultCardProps {
  book: SearchBook;
  /** 已在书架(页面按 ["books"] 缓存 + 本次会话新增判定): 快速加架转为「在架」chip */
  inShelf: boolean;
  /** 点击卡片: 打开砚台式书籍详情(开始阅读/换源都在详情里) */
  onSelect: (book: SearchBook) => void;
  onAdded: (saved: Book) => void;
  /** 当前搜索词: 书名/作者/简介命中处高亮, 让「为什么出现这条」可见 */
  keyword?: string;
  /** grid=桌面网格卡 / list=行卡 (桌面列表布局); 移动端恒为行卡 */
  layout?: "grid" | "list";
}

/** 关键词命中高亮: indexOf 切分 (无正则, 特殊字符安全), 命中段着 accent 色 */
function Highlight({ text, keyword }: { text: string; keyword: string }) {
  const needle = keyword.trim().toLowerCase();
  if (needle.length === 0) {
    return <>{text}</>;
  }
  const lower = text.toLowerCase();
  const parts: ReactNode[] = [];
  let from = 0;
  for (;;) {
    const at = lower.indexOf(needle, from);
    if (at === -1) {
      break;
    }
    if (at > from) {
      parts.push(text.slice(from, at));
    }
    parts.push(
      <span key={at} className="font-semibold text-accent">
        {text.slice(at, at + needle.length)}
      </span>,
    );
    from = at + needle.length;
  }
  if (from < text.length) {
    parts.push(text.slice(from));
  }
  return <>{parts}</>;
}

/**
 * 搜索结果卡片(移植自砚台原型 LibraryGrid):
 * - sm 起为网格卡: 3:4 封面(悬停轻抬) + 居中衬线书名 + 作者·来源 + 两行引言;
 * - 移动端为紧凑行卡: w-20 封面 + 信息列 + 行尾的「在架」chip / 快速加架.
 *
 * 整张卡片是一个覆盖式按钮(点击打开书籍详情); chip 与加架按钮浮在覆盖层之上.
 */
export function SearchResultCard({
  book,
  inShelf,
  onSelect,
  onAdded,
  keyword = "",
  layout = "grid",
}: SearchResultCardProps) {
  const grid = layout === "grid";
  const addToShelf = useAddToShelf({ onSaved: onAdded });

  const originCount = book.origins?.length ?? 1;
  const intro = book.intro?.trim() ?? "";
  const meta = [book.author.trim(), book.originName].filter(Boolean).join(" · ");
  /** 源搜索规则常不给简介: 退而展示分类 (最新章节名易被误读为简介, 不放进兜底) */
  const fallback = book.kind?.trim() ?? "";

  return (
    <div
      className={cn(
        "group relative flex touch-manipulation select-none items-center gap-3 rounded-xl border border-border bg-surface p-3",
        "transition-colors duration-150 ease-out hover:border-accent/50",
        grid &&
          "sm:flex-col sm:items-stretch sm:gap-0 sm:rounded-none sm:border-0 sm:bg-transparent sm:p-0 sm:hover:border-0",
      )}
    >
      <div className={cn("relative w-20 shrink-0", grid && "sm:w-full")}>
        <CoverImage
          src={getCoverUrl(book.coverUrl)}
          alt={book.name}
          author={book.author}
          intro={intro}
          className={cn(
            "p-2.5 transition duration-300 ease-out group-hover:-translate-y-1 group-hover:shadow-xl",
            grid && "sm:p-4",
          )}
        />
        {originCount > 1 ? (
          <Badge
            size="sm"
            className="pointer-events-none absolute top-1.5 left-1.5 z-20 bg-background/85 shadow-sm backdrop-blur-sm"
          >
            {originCount} 个源
          </Badge>
        ) : null}
      </div>

      <div className={cn("min-w-0 flex-1", grid && "sm:mt-3 sm:flex-none")}>
        <p
          title={book.name}
          className={cn(
            "truncate font-display text-sm font-medium tracking-[-0.01em] transition-colors duration-150 ease-out group-hover:text-accent",
            grid && "sm:text-center",
          )}
        >
          <Highlight text={book.name} keyword={keyword} />
        </p>
        <p className={cn("mt-0.5 truncate text-xs text-muted-foreground", grid && "sm:text-center")}>
          {meta.length > 0 ? (
            <>
              {book.author.trim() !== "" ? (
                <Highlight text={book.author.trim()} keyword={keyword} />
              ) : null}
              {book.author.trim() !== "" && book.originName ? " · " : ""}
              {book.originName ?? ""}
            </>
          ) : (
            " "
          )}
        </p>
        {chapterTitleText(book.latestChapterTitle) ? (
          <p className="mt-1 truncate text-xs text-muted-foreground/80">
            最新 {chapterTitleText(book.latestChapterTitle)}
          </p>
        ) : null}
        {intro.length > 0 ? (
          <p className={cn("mt-1.5 line-clamp-2 text-xs leading-relaxed text-muted-foreground", grid && "sm:mt-2")}>
            <Highlight text={intro} keyword={keyword} />
          </p>
        ) : fallback.length > 0 ? (
          <p className={cn("mt-1.5 line-clamp-2 text-xs leading-relaxed text-muted-foreground/80", grid && "sm:mt-2")}>
            {fallback}
          </p>
        ) : null}
      </div>
      {/* 在架态 / 快速加架: 移动端在行尾, sm 起浮在封面右上角 */}
      <div className={cn("relative z-20 shrink-0", grid && "sm:absolute sm:top-1.5 sm:right-1.5")}>
        {inShelf ? (
          <Badge
            size="sm"
            variant="accent"
            className={cn("pointer-events-none", grid && "sm:bg-background/85 sm:shadow-sm sm:backdrop-blur-sm")}
          >
            <Check aria-hidden />
            在架
          </Badge>
        ) : (
          <IconButton
            size="sm"
            variant="secondary"
            tooltip="加入书架"
            aria-label={`把《${book.name}》加入书架`}
            loading={addToShelf.isPending}
            disabled={addToShelf.isPending}
            onClick={() => addToShelf.mutate(book)}
            className="bg-background/85 opacity-80 shadow-sm backdrop-blur-sm transition-opacity duration-150 ease-out group-hover:opacity-100"
          >
            <BookPlus aria-hidden />
          </IconButton>
        )}
      </div>

      <button
        type="button"
        onClick={() => onSelect(book)}
        aria-label={`查看《${book.name}》的详情`}
        className={cn(
          "absolute inset-0 z-10 cursor-pointer rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-accent/60 active:scale-98",
          grid && "sm:rounded-none",
        )}
      />
    </div>
  );
}
