import { CoverImage, SkeletonCard, cn } from "@/components/ui";
import { getCoverUrl } from "@/services/book";
import type { SearchBook } from "@/types/api";

export interface ExploreBookGridProps {
  books: SearchBook[];
  /** 加载中: 渲染与书架同款的封面骨架 */
  loading?: boolean;
  onSelect: (book: SearchBook) => void;
  /** 网格容器 id: 分类标签的 aria-controls 指向它 */
  id?: string;
  className?: string;
}

/** 与书架页同款的自适应封面网格(3:4 封面 + 书名 + 作者) */
const GRID_CLASS =
  "grid grid-cols-[repeat(auto-fill,minmax(96px,1fr))] gap-x-3 gap-y-4 md:grid-cols-[repeat(auto-fill,minmax(160px,1fr))] md:gap-x-4 md:gap-y-6";
const SKELETON_COUNT = 12;

/** 发现结果网格: 点击卡片打开书籍详情弹窗 */
export function ExploreBookGrid({
  books,
  loading = false,
  onSelect,
  id,
  className,
}: ExploreBookGridProps) {
  if (loading) {
    return (
      <div aria-hidden className={cn(GRID_CLASS, className)}>
        {Array.from({ length: SKELETON_COUNT }, (_, index) => (
          <SkeletonCard key={index} />
        ))}
      </div>
    );
  }

  return (
    <div id={id} className={cn(GRID_CLASS, className)}>
      {books.map((book) => (
        <button
          key={`${book.bookUrl}|${book.origin}`}
          type="button"
          onClick={() => onSelect(book)}
          className="group flex cursor-pointer flex-col gap-1.5 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <CoverImage
            src={getCoverUrl(book.coverUrl)}
            alt={book.name}
            className="w-full shadow-sm ring-1 ring-border/60 transition-transform duration-300 ease-out group-hover:-translate-y-1 group-hover:ring-accent/50"
          />
          <p className="font-display line-clamp-2 text-xs leading-4 font-medium tracking-[-0.01em] text-foreground transition-colors duration-150 ease-out group-hover:text-accent">
            {book.name}
          </p>
          <p className="truncate text-xs text-muted-foreground">
            {book.author.length > 0 ? book.author : book.originName}
          </p>
        </button>
      ))}
    </div>
  );
}
