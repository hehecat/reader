import { BookMarked, CheckSquare, Download, Ellipsis, Info, Square, Trash2 } from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import {
  CoverImage,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  IconButton,
  cn,
} from "@/components/ui";
import { bookCover, bookGroupNames, readerPath } from "@/hooks/useBookshelf";
import type { Book, BookGroup } from "@/types/api";

/** grid=封面网格(封面 + 居中标题/作者两行); list=行卡(封面 + 书名 + 作者·分组 + 引言) */
export type BookCardLayout = "grid" | "list";

export interface BookCardProps {
  book: Book;
  /** 展示形态, 默认网格 */
  layout?: BookCardLayout;
  /** 自定义分组(getBookGroups): 列表模式把 book.group 位掩码翻译成组名 */
  groups?: BookGroup[];
  /** 编辑模式: 出现选择框, 点击卡片切换选中而不是进入阅读器 */
  editMode?: boolean;
  selected?: boolean;
  onToggleSelect?: (book: Book) => void;
  /** 打开书籍详情弹窗 */
  onShowInfo?: (book: Book) => void;
  /** 请求删除(由页面统一确认后执行) */
  /** 后台缓存整书 (warp cacheBookSSE); 本地书不显示 */
  onCacheBook?: (book: Book) => void;
  onDelete?: (book: Book) => void;
  /** 设为读完: 进度推进到最后一章 */
  onMarkRead?: (book: Book) => void;
  className?: string;
}

/**
 * 书架卡片(移植自砚台原型 MyShelf / LibraryGrid):
 * - 网格: 3:4 封面(无封面时由 CoverImage 渲染书脊渐变) + 居中标题与作者两行, 悬停封面轻抬;
 * - 列表: 行卡(封面 + 衬线书名 + 作者·分组 + 两行引言).
 *
 * 整张卡片是一个覆盖式按钮(点击进入阅读器); 省略号菜单悬停/聚焦/触摸时显现,
 * 桌面右键为加速器. 选择框与菜单浮在覆盖层之上, 互不嵌套.
 */
export function BookCard({
  book,
  layout = "grid",
  groups,
  editMode = false,
  selected = false,
  onToggleSelect,
  onShowInfo,
  onDelete,
  onMarkRead,
  onCacheBook,
  className,
}: BookCardProps) {
  const navigate = useNavigate();
  const [menuOpen, setMenuOpen] = React.useState(false);

  const unread = book.totalChapterNum - 1 - book.durChapterIndex;
  const progressPct =
    book.totalChapterNum > 0
      ? Math.min(100, Math.round(((book.durChapterIndex + 1) / book.totalChapterNum) * 100))
      : 0;
  const progressBar =
    book.totalChapterNum > 0 ? (
      <div aria-hidden className="h-0.5 overflow-hidden rounded-full bg-border/60">
        <div className="h-full bg-accent/70" style={{ width: `${progressPct}%` }} />
      </div>
    ) : null;
  const canMarkRead = book.totalChapterNum > 0 && unread > 0;
  const chapter = book.durChapterTitle?.trim();
  /** 阅读进度文案: 读至章节名/序号 + 总章数; 未读开时退化为最新章节 */
  // 章节名常超长被截断: 卡片只用序号(短且对齐), 章节名在详情弹窗看
  // 章号以进度下标为准(index+1): 标题可能滞后于进度(换源/源站缺号), 不作为章号来源
  const progressText =
    book.totalChapterNum > 0
      ? `读至第 ${book.durChapterIndex + 1} 章 · 共 ${book.totalChapterNum} 章`
      : (book.latestChapterTitle?.trim() ? `最新 ${book.latestChapterTitle.trim()}` : "");
  const intro = (book.customIntro ?? book.intro ?? "").trim();
  const groupNames = groups === undefined ? [] : bookGroupNames(book, groups);

  const activate = () => {
    if (editMode) {
      onToggleSelect?.(book);
      return;
    }
    navigate(readerPath(book.bookUrl, book.durChapterIndex));
  };

  const handleContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    if (editMode) return;
    event.preventDefault();
    setMenuOpen(true);
  };

  const selectedRing = selected && "ring-2 ring-accent ring-offset-2 ring-offset-background";

  const checkbox = editMode ? (
    <button
      type="button"
      role="checkbox"
      aria-checked={selected}
      aria-label={`选择《${book.name}》`}
      onClick={() => onToggleSelect?.(book)}
      className={cn(
        "z-20 grid cursor-pointer place-items-center rounded-md bg-background/85 shadow ring-1 ring-border backdrop-blur-sm after:absolute after:-inset-2.5 after:content-['']",
        layout === "grid" ? "absolute left-1.5 top-1.5 size-6" : "relative size-9 shrink-0",
      )}
    >
      {selected ? (
        <CheckSquare aria-hidden className="size-4 text-accent" />
      ) : (
        <Square aria-hidden className="size-4 text-muted-foreground" />
      )}
    </button>
  ) : null;

  const menu = editMode ? null : (
    <DropdownMenu open={menuOpen} onOpenChange={setMenuOpen}>
      <DropdownMenuTrigger asChild>
        <IconButton
          size="sm"
          variant={layout === "grid" ? "secondary" : "ghost"}
          aria-label={`《${book.name}》的操作`}
          className={cn(
            "z-20 opacity-0 transition-opacity duration-150 ease-out group-hover:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100 pointer-coarse:opacity-100",
            layout === "grid"
              ? "absolute bottom-1.5 right-1.5 bg-background/85 shadow backdrop-blur-sm"
              : "relative shrink-0",
          )}
        >
          <Ellipsis aria-hidden />
        </IconButton>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" side="top" className="min-w-36">
        <DropdownMenuItem onSelect={() => onShowInfo?.(book)}>
          <Info aria-hidden />
          书籍详情
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!canMarkRead} onSelect={() => onMarkRead?.(book)}>
          <BookMarked aria-hidden />
          设为读完
        </DropdownMenuItem>
              {book.bookUrl.startsWith("local://") ? null : (
                <DropdownMenuItem onSelect={() => onCacheBook?.(book)}>
                  <Download aria-hidden />
                  缓存本书
                </DropdownMenuItem>
              )}
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="danger" onSelect={() => onDelete?.(book)}>
          <Trash2 aria-hidden />
          删除
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );

  const overlay = (
    <button
      type="button"
      onClick={activate}
      tabIndex={editMode ? -1 : 0}
      aria-label={editMode ? undefined : `阅读《${book.name}》`}
      aria-hidden={editMode || undefined}
      className="absolute inset-0 z-10 cursor-pointer rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-accent/60 active:scale-98"
    />
  );

  if (layout === "list") {
    const meta = [book.author.trim(), groupNames.join("/")].filter(Boolean).join(" · ");
    return (
      <div
        className={cn(
          "group relative flex touch-manipulation select-none items-center gap-3 rounded-xl border border-border bg-surface p-3 transition-colors duration-150 ease-out hover:border-accent/50 sm:gap-4",
          selectedRing,
          className,
        )}
        onContextMenu={handleContextMenu}
      >
        {checkbox}

        <div className="relative w-14 shrink-0 sm:w-16">
          <CoverImage
            src={bookCover(book)}
            alt={book.name}
            author={book.author}
            intro={intro}
            className="p-2 shadow-sm ring-1 ring-border/70"
          />
        </div>

        <div className="min-w-0 flex-1">
          <p className="font-display truncate text-base font-semibold leading-tight tracking-[-0.01em]">
            {book.name}
          </p>
          <p className="mt-1 truncate text-xs leading-4 text-muted-foreground">
            {meta.length > 0 ? meta : chapter || " "}
          </p>
          {progressText.length > 0 ? (
            <p className="mt-1 truncate text-xs leading-4 text-muted-foreground/80">{progressText}</p>
          ) : null}
          {intro.length > 0 ? (
            <p className="mt-1.5 line-clamp-2 text-xs leading-relaxed text-muted-foreground">
              {intro}
            </p>
          ) : null}
        </div>

        {menu}
        {overlay}
        {progressBar !== null ? (
          <div className="pointer-events-none absolute inset-x-3 bottom-0">{progressBar}</div>
        ) : null}
      </div>
    );
  }

  return (
    <div
      className={cn(
        "group relative flex touch-manipulation select-none flex-col rounded-xl",
        selectedRing,
        className,
      )}
      onContextMenu={handleContextMenu}
    >
      <div className="relative">
        <CoverImage
          src={bookCover(book)}
          alt={book.name}
          author={book.author}
          intro={intro}
          className="transition duration-150 ease-out group-hover:-translate-y-0.5 group-hover:shadow-xl"
        />

        {checkbox}
        {menu}
      </div>

      <p
        title={book.name}
        className="mt-2 min-w-0 truncate px-0.5 text-center text-sm font-medium"
      >
        {book.name}
      </p>

      {book.author.trim().length > 0 ? (
        <p
          title={book.author}
          className="mt-0.5 min-w-0 truncate px-0.5 text-center text-xs text-muted-foreground"
        >
          {book.author.trim()}
        </p>
      ) : null}

      {progressText.length > 0 ? (
        <p
          title={progressText}
          className="mt-0.5 min-w-0 truncate px-0.5 text-center text-xs text-muted-foreground/80"
        >
          {progressText}
        </p>
      ) : null}
      {progressBar !== null ? <div className="mt-1.5 px-0.5">{progressBar}</div> : null}

      {overlay}
    </div>
  );
}
