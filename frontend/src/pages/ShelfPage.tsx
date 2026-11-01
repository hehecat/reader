import {
  Check,
  FolderInput,
  Grid2X2,
  Library,
  List,
  ListChecks,
  RotateCw,
  Search,
  Trash2,
  Upload,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { BookInfoDialog } from "@/components/shelf/BookInfoDialog";
import { BookCard, type BookCardLayout } from "@/components/shelf/BookCard";
import { ImportBookDialog } from "@/components/shelf/ImportBookDialog";
import { GroupTabs } from "@/components/shelf/GroupTabs";
import { PullToRefresh } from "@/components/shelf/PullToRefresh";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  IconButton,
  Input,
  PageIntro,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  SkeletonCard,
  SkeletonList,
  cn,
  toast,
} from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import { startCacheBookStream } from "@/services/cache";
import {
  GROUP_ALL,
  buildShelfGroups,
  errorMessage,
  filterBooksByGroup,
  readerPath,
  useBookGroups,
  useBookshelf,
  useDeleteBook,
  useMarkBookRead,
  useSaveBookGroupId,
} from "@/hooks/useBookshelf";
import { getJSON, setJSON } from "@/lib/storage";
/** 后台缓存任务去重 (bookUrl 维度, 模块级跨挂载持久) */
const cachingBooks = new Set<string>();

import type { Book } from "@/types/api";

/** 卡片容器 id: 分组 chips 通过 aria-controls 指向它 */
const GRID_ID = "shelf-book-grid";
const SKELETON_COUNT = 12;
/** 网格布局(照抄原型 MyShelf, 按第三批反馈放大): 手机 3 列 → sm 4 列 → lg 5 列, 间距同步加大 */
const GRID_CLASS = "grid grid-cols-3 gap-x-6 gap-y-8 sm:grid-cols-4 lg:grid-cols-5";
/** 列表布局(移植自原型 LibraryGrid): 行卡纵向堆叠 */
const LIST_CLASS = "flex flex-col gap-3";
/** 空态/错误态: 虚线卡片(原型 MyShelf / LibraryGrid 空态样式) */
const EMPTY_CLASS = "rounded-xl border border-dashed border-border bg-surface/60 py-12";
const EMPTY_SELECTION: ReadonlySet<string> = new Set();
/** 布局偏好持久化 key(lib/storage 统一 reader. 前缀) */
const LAYOUT_KEY = "reader.shelfLayout";

interface LayoutToggleProps {
  active: boolean;
  /** 无障碍名称, 如「网格布局」 */
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}

/** hero 右侧的布局切换钮: 活跃态 accent 描边 + 浅底 */
function LayoutToggle({ active, label, onClick, children }: LayoutToggleProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      className={cn(
        "flex size-9 cursor-pointer items-center justify-center rounded-md border outline-none transition-colors duration-150 ease-out focus-visible:ring-2 focus-visible:ring-ring",
        active
          ? "border-accent bg-accent/10 text-accent"
          : "border-border text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

export default function ShelfPage() {
  const navigate = useNavigate();
  const { books, isLoading, isRefreshing, error, refetch, refreshShelf } = useBookshelf();
  const deleteBooks = useDeleteBook();
  const moveToGroup = useSaveBookGroupId();
  const markRead = useMarkBookRead();
  const [keyword, setKeyword] = React.useState("");
  const [layout, setLayout] = React.useState<BookCardLayout>(() =>
    getJSON<string>(LAYOUT_KEY) === "list" ? "list" : "grid",
  );
  const [activeGroup, setActiveGroup] = React.useState(GROUP_ALL);
  const [editMode, setEditMode] = React.useState(false);
  const [selected, setSelected] = React.useState<ReadonlySet<string>>(EMPTY_SELECTION);
  const [infoBook, setInfoBook] = React.useState<Book | null>(null);
  const [infoOpen, setInfoOpen] = React.useState(false);
  const [pendingDelete, setPendingDelete] = React.useState<Book[] | null>(null);
  const [moveOpen, setMoveOpen] = React.useState(false);
  const [moveTarget, setMoveTarget] = React.useState("");
  const [importOpen, setImportOpen] = React.useState(false);
  const { groups } = useBookGroups();
  const shelfGroups = React.useMemo(() => buildShelfGroups(books, groups), [books, groups]);

  // 当前分组可能因为书被删空而消失, 回落到「全部」
  const currentGroup = React.useMemo(
    () =>
      shelfGroups.some((item) => item.group.groupId === activeGroup) ? activeGroup : GROUP_ALL,
    [shelfGroups, activeGroup],
  );

  // 分组被删空后回落「全部」时给出说明, 避免选中态凭空消失
  const groupFellBack = activeGroup !== GROUP_ALL && !shelfGroups.some((item) => item.group.groupId === activeGroup);
  React.useEffect(() => {
    if (groupFellBack) {
      toast.info("该分组已空, 已回到全部");
      setActiveGroup(GROUP_ALL);
    }
  }, [groupFellBack]);

  const visibleBooks = React.useMemo(() => {
    const inGroup = filterBooksByGroup(books, currentGroup);
    const query = keyword.trim().toLowerCase();
    if (query.length === 0) return inGroup;
    return inGroup.filter(
      (book) =>
        book.name.toLowerCase().includes(query) || book.author.toLowerCase().includes(query),
    );
  }, [books, currentGroup, keyword]);

  /** saveBookGroupId 只接受 groupId > 0, 未分组(0)与内置负数分组不能作为目标 */
  const customGroups = React.useMemo(() => groups.filter((group) => group.groupId > 0), [groups]);

  const selectedBooks = React.useMemo(
    () => books.filter((book) => selected.has(book.bookUrl)),
    [books, selected],
  );

  const toggleSelect = (book: Book) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(book.bookUrl)) next.delete(book.bookUrl);
      else next.add(book.bookUrl);
      return next;
    });
  };

  const exitEdit = () => {
    setEditMode(false);
    setSelected(EMPTY_SELECTION);
  };

  const toggleEdit = () => {
    if (editMode) exitEdit();
    else setEditMode(true);
  };

  const startReading = (book: Book) => {
    setInfoOpen(false);
    navigate(readerPath(book.bookUrl, book.durChapterIndex));
  };

  const confirmDelete = () => {
    const targets = pendingDelete ?? [];
    setPendingDelete(null);
    deleteBooks.mutate(
      targets.map((book) => book.bookUrl),
      {
        onSuccess: () => {
          setInfoOpen(false);
          setSelected((previous) => {
            const next = new Set(previous);
            for (const book of targets) next.delete(book.bookUrl);
            return next;
          });
        },
      },
    );
  };

  const handleMove = async () => {
    const groupId = Number(moveTarget);
    const targets = selectedBooks;
    setMoveOpen(false);
    setMoveTarget("");
    if (!Number.isInteger(groupId) || groupId <= 0 || targets.length === 0) return;

    const groupName = customGroups.find((group) => group.groupId === groupId)?.groupName ?? "分组";
    const results = await Promise.allSettled(
      targets.map((book) => moveToGroup.mutateAsync({ url: book.bookUrl, groupId })),
    );
    const moved = results.filter((result) => result.status === "fulfilled").length;
    if (moved === 0) return;
    toast.success(`已移动 ${moved} 本书到「${groupName}」`);
    exitEdit();
  };

  const clearFilters = () => {
    setKeyword("");
    setActiveGroup(GROUP_ALL);
  };

  /** 切换网格/列表并记住偏好 */
  const changeLayout = (next: BookCardLayout) => {
    setLayout(next);
    setJSON(LAYOUT_KEY, next);
  };

  // 后台整书缓存: 模块级去重 (同书并发只起一个任务), 进度在阅读器目录抽屉可见
  const handleCacheBook = (book: Book) => {
    if (cachingBooks.has(book.bookUrl)) {
      toast.info("该书已在后台缓存");
      return;
    }
    cachingBooks.add(book.bookUrl);
    toast.info("后台缓存已启动, 阅读器目录可见进度");
    startCacheBookStream(book.bookUrl, {
      onProgress: () => {},
      onDone: (progress) => {
        cachingBooks.delete(book.bookUrl);
        if (progress.cancelled) {
          toast.info("缓存已取消");
        } else {
          toast.success(`已缓存 ${progress.cached} 章`);
        }
      },
      onError: (error) => {
        cachingBooks.delete(book.bookUrl);
        toast.error(humanizeError(error.message, "缓存失败"));
      },
    });
  };

  const renderBook = (book: Book) => (
    <BookCard
      key={book.bookUrl}
      book={book}
      layout={layout}
      groups={groups}
      editMode={editMode}
      selected={selected.has(book.bookUrl)}
      onToggleSelect={toggleSelect}
      onShowInfo={(target) => {
        setInfoBook(target);
        setInfoOpen(true);
      }}
      onDelete={(target) => setPendingDelete([target])}
      onMarkRead={(target) => markRead.mutate(target)}
      onCacheBook={handleCacheBook}
    />
  );

  const deleteTarget = pendingDelete?.[0];
  const shelfReady = !isLoading && error === null;

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col px-4 pb-8 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="GOOD STORIES, QUIET MOMENTS"
        title="书架"
        desc="收纳你的故事, 也收纳此刻的心情 —— 每一次打开, 都从上次停下的地方开始."
        action={
          <>
            <Input
              className="w-56 sm:w-64"
              aria-label="在书架中筛选"
              placeholder="搜索书名或作者"
              value={keyword}
              onChange={(event) => setKeyword(event.target.value)}
              clearable
              onClear={() => setKeyword("")}
              prefixIcon={<Search aria-hidden className="size-4 text-muted-foreground" />}
            />
            <div className="flex items-center gap-2">
              <LayoutToggle
                active={layout === "grid"}
                label="网格布局"
                onClick={() => changeLayout("grid")}
              >
                <Grid2X2 aria-hidden className="size-4" />
              </LayoutToggle>
              <LayoutToggle
                active={layout === "list"}
                label="列表布局"
                onClick={() => changeLayout("list")}
              >
                <List aria-hidden className="size-4" />
              </LayoutToggle>
            </div>
          </>
        }
      />

      <PullToRefresh
        onRefresh={refreshShelf}
        disabled={editMode || isLoading}
        className="flex min-h-0 flex-1 flex-col"
      >
        {/* 分组 chips 只在书架有数据时出现, 加载/错误态直接进「我的书架」段 */}
        {shelfReady ? (
          <GroupTabs
            books={books}
            groups={groups}
            value={currentGroup}
            onChange={setActiveGroup}
            controlsId={GRID_ID}
            className="mb-8 min-w-0"
          />
        ) : null}

        <section className="flex min-h-0 flex-1 flex-col">
          <div className="mb-5 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <h2 className="font-display flex items-center gap-2 text-lg font-semibold">
              <Library aria-hidden className="size-4 shrink-0 text-accent" />
              我的书架
              {shelfReady && books.length > 0 ? (
                <span className="text-sm font-normal text-muted-foreground">
                  {books.length} 本作品
                </span>
              ) : null}
            </h2>
            <div className="flex shrink-0 items-center gap-1.5">
              <IconButton
                variant="ghost"
                aria-label="刷新书架"
                tooltip="刷新书架"
                disabled={isLoading || isRefreshing}
                onClick={() => void refreshShelf()}
              >
                <RotateCw aria-hidden className={cn(isRefreshing && "ui-spin")} />
              </IconButton>
              <IconButton
                variant="ghost"
                aria-label="导入本地书籍"
                tooltip="导入书籍"
                onClick={() => setImportOpen(true)}
              >
                <Upload aria-hidden />
              </IconButton>
              <Button
                size="sm"
                variant={editMode ? "primary" : "ghost"}
                aria-pressed={editMode}
                onClick={toggleEdit}
              >
                {editMode ? <Check aria-hidden /> : <ListChecks aria-hidden />}
                {editMode ? "完成" : "管理"}
              </Button>
            </div>
          </div>

          {isLoading ? (
            layout === "list" ? (
              <SkeletonList count={6} />
            ) : (
              <div aria-hidden className={GRID_CLASS}>
                {Array.from({ length: SKELETON_COUNT }, (_, index) => (
                  <SkeletonCard key={index} />
                ))}
              </div>
            )
          ) : error !== null ? (
            <EmptyState
              className={EMPTY_CLASS}
              icon={<Library aria-hidden />}
              title="书架加载失败"
              description={errorMessage(error, "网络异常或登录态已失效")}
              action={
                <Button size="sm" variant="secondary" onClick={() => refetch()}>
                  <RotateCw aria-hidden />
                  重试
                </Button>
              }
            />
          ) : books.length === 0 ? (
            <EmptyState
              className={EMPTY_CLASS}
              icon={<Library aria-hidden />}
              title="书架还是空的"
              description="搜索书名或作者, 把第一本书加入书架开始阅读"
              action={
                <Button size="sm" onClick={() => navigate("/search")}>
                  <Search aria-hidden />
                  去搜索
                </Button>
              }
            />
          ) : visibleBooks.length === 0 ? (
            <EmptyState
              compact
              className={EMPTY_CLASS}
              icon={<Search aria-hidden />}
              title="没有符合条件的书"
              description={keyword.trim() ? "换个关键词看看" : undefined}
              action={
                <Button size="sm" variant="secondary" onClick={clearFilters}>
                  清除筛选
                </Button>
              }
            />
          ) : (
            <div id={GRID_ID} className={layout === "list" ? LIST_CLASS : GRID_CLASS}>
              {visibleBooks.map(renderBook)}
            </div>
          )}
        </section>
      </PullToRefresh>

      {editMode ? (
        <div className="sticky bottom-0 z-30 flex flex-wrap items-center gap-2 rounded-xl border border-border bg-background/95 px-3 pt-2.5 pb-[max(0.625rem,env(safe-area-inset-bottom,0px))] shadow-lg backdrop-blur-sm">
          <span className="whitespace-nowrap text-sm leading-8 text-muted-foreground">
            已选
            <span className="mx-1 font-medium text-foreground tabular-nums">{selected.size}</span>
            本
          </span>
          <div className="ml-auto flex items-center gap-2">
            <Button
              size="sm"
              variant="secondary"
              disabled={selected.size === 0 || customGroups.length === 0}
              title={customGroups.length === 0 ? "先创建分组才能移动" : undefined}
              onClick={() => setMoveOpen(true)}
            >
              <FolderInput aria-hidden />
              移动分组
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={selected.size === 0}
              loading={deleteBooks.isPending}
              onClick={() => setPendingDelete(selectedBooks)}
            >
              <Trash2 aria-hidden />
              删除
            </Button>
            <Button size="sm" variant="ghost" onClick={exitEdit}>
              取消
            </Button>
          </div>
        </div>
      ) : null}

      <BookInfoDialog
        book={infoBook}
        groups={groups}
        open={infoOpen}
        onOpenChange={setInfoOpen}
        onStartReading={startReading}
        onMarkRead={(target) => markRead.mutate(target)}
        onDelete={(book) => {
          setPendingDelete([book]);
        }}
      />

      <ImportBookDialog open={importOpen} onOpenChange={setImportOpen} />

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>删除书籍</DialogTitle>
            <DialogDescription>
              {pendingDelete !== null && pendingDelete.length === 1
                ? `确定把《${deleteTarget?.name ?? ""}》从书架删除吗? 阅读进度会一并清除.`
                : `确定删除选中的 ${pendingDelete?.length ?? 0} 本书吗? 阅读进度会一并清除.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button variant="danger" loading={deleteBooks.isPending} onClick={confirmDelete}>
              <Trash2 aria-hidden />
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={moveOpen} onOpenChange={setMoveOpen}>
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>移动到分组</DialogTitle>
            <DialogDescription>
              把选中的 {selectedBooks.length} 本书移动到指定分组(会覆盖原有分组归属)
            </DialogDescription>
          </DialogHeader>

          <div className="px-4 md:px-5">
            {customGroups.length === 0 ? (
              <EmptyState
                compact
                icon={<FolderInput aria-hidden />}
                title="还没有自定义分组"
                description="分组管理上线后可以先创建分组, 再回来移动书籍"
              />
            ) : (
              <Select value={moveTarget} onValueChange={setMoveTarget}>
                <SelectTrigger aria-label="目标分组" className="w-full">
                  <SelectValue placeholder="选择分组" />
                </SelectTrigger>
                <SelectContent>
                  {customGroups.map((group) => (
                    <SelectItem key={group.groupId} value={String(group.groupId)}>
                      {group.groupName}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
          </div>

          <DialogFooter>
            <Button variant="ghost" onClick={() => setMoveOpen(false)}>
              取消
            </Button>
            <Button
              disabled={moveTarget.length === 0}
              loading={moveToGroup.isPending}
              onClick={() => void handleMove()}
            >
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
