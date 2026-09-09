import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeftRight,
  BookOpen,
  BookPlus,
  Check,
  CircleAlert,
  RefreshCw,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import {
  Badge,
  Button,
  CoverImage,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  Skeleton,
  Spinner,
  cn,
  toast,
} from "@/components/ui";
import { useAddToShelf } from "@/hooks/useAddToShelf";
import {
  BOOKS_QUERY_KEY,
  LOCAL_ORIGIN,
  bookCover,
  errorMessage,
  readerPath,
  useBookshelf,
} from "@/hooks/useBookshelf";
import { humanizeError } from "@/lib/errors";
import { getBookInfo, getChapterList, getCoverUrl } from "@/services/book";
import {
  getAvailableBookSource,
  setBookSource,
  toAvailableBookSource,
  type AvailableBookSource,
} from "@/services/explore";
import { getBookSourcesLite } from "@/services/sources";
import type { Book, SearchBook } from "@/types/api";

export interface BookDetailDialogProps {
  /** 为 null 时弹窗保持关闭 */
  book: SearchBook | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 加入书架成功回报(可选): 调用方用它兜住后端读缓存约 5 秒的成员态窗口 */
  onAdded?: (saved: Book) => void;
}

/** 目录超过这个章节数就限高滚动, 免得弹窗被一本书的目录撑到几千行 */
const CHAPTER_SCROLL_LIMIT = 300;

/**
 * 书籍详情(书海/搜索等非书架来源共用), 结构移植自砚台原型 book-detail:
 * 上半部 hero(左封面 + 右书名/作者·来源/引言/动作行), 下半部章节目录(点击直达该章).
 * 弹窗关闭即卸载, 内部状态(换源身份、面板开关)与查询自然复位.
 */
export function BookDetailDialog({ book, open, onOpenChange, onAdded }: BookDetailDialogProps) {
  if (book === null) {
    return <Dialog open={false} onOpenChange={onOpenChange} />;
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="xl">
        <BookDetail book={book} onAdded={onAdded} />
      </DialogContent>
    </Dialog>
  );
}

function BookDetail({ book, onAdded }: { book: SearchBook; onAdded?: (saved: Book) => void }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { books } = useBookshelf();

  /** 换源成功后书籍身份(bookUrl/origin)改变, 详情与目录查询随之切到新身份 */
  const [identity, setIdentity] = React.useState<{ bookUrl: string; origin: string } | null>(null);
  const [switchOpen, setSwitchOpen] = React.useState(false);
  /** true = 让后端用全部书源重新搜索候选(慢), 作为独立 query key 与缓存结果区分 */
  const [deepSearch, setDeepSearch] = React.useState(false);
  /** 本次会话里刚加入书架: 后端用户存储读取有数秒缓存, 书架列表不会立刻反映 */
  const [added, setAdded] = React.useState(false);

  const detailKey = React.useMemo(
    () => ["bookInfo", identity?.bookUrl ?? book.bookUrl, identity?.origin ?? book.origin],
    [identity, book.bookUrl, book.origin],
  );
  const detail = useQuery({
    queryKey: detailKey,
    // 未换源: 带上搜索结果里的书源提示(书不在书架时必须如此);
    // 已换源: 带新身份的书源提示(未入架书的换源是纯前端切换, 后端同样需要显式源)
    queryFn: () =>
      identity === null
        ? getBookInfo(book)
        : getBookInfo(
            identity.origin === LOCAL_ORIGIN
              ? identity.bookUrl
              : { url: identity.bookUrl, bookSourceUrl: identity.origin },
          ),
  });
  const info = detail.data;
  const currentBookUrl = identity?.bookUrl ?? info?.bookUrl ?? book.bookUrl;
  const currentOrigin = identity?.origin ?? info?.origin ?? book.origin;

  // 书不在书架时后端需要显式书源才能取目录(getChapterList 的 bookSourceUrl 入参)
  const chapters = useQuery({
    queryKey: ["chapters", currentBookUrl, currentOrigin],
    queryFn: () => getChapterList(currentBookUrl, false, { bookSourceUrl: currentOrigin }),
  });
  const chapterList = chapters.data ?? [];

  // 后端 saveBook 以「书名 + 作者」判重, 这里用同一口径判断是否已在书架
  const shelfBook = books.find(
    (item) => item.bookUrl === currentBookUrl || (item.name === book.name && item.author === book.author),
  );
  const inShelf = added || shelfBook !== undefined;
  // 换源候选接口按 bookUrl 精确查书架行: 同名他源的书(标题口径在架)不算, 需先以本身份入架
  const exactInShelf = added || books.some((item) => item.bookUrl === currentBookUrl);

  const available = useQuery({
    queryKey: ["availableBookSources", currentBookUrl, deepSearch ? 1 : 0],
    queryFn: () => getAvailableBookSource(currentBookUrl, deepSearch),
    enabled: switchOpen && exactInShelf,
  });

  const sourcesQuery = useQuery({
    queryKey: ["sourcesLite"],
    queryFn: () => getBookSourcesLite(),
    enabled: switchOpen,
    staleTime: 5 * 60_000,
  });
  /** 搜索聚合已保留各源 bookUrl(originUrls): 开面板直接构造候选, 不再按源重搜;
   * 未保留 url 的源进 deadOrigins, 由界面按需重试 */
  const originCandidatesData = React.useMemo<
    Array<{ origin: string; candidate: AvailableBookSource | null }>
  >(() => {
    const lite = sourcesQuery.data ?? [];
    return (book.origins ?? [])
      .filter((origin) => origin !== currentOrigin)
      .map((origin) => {
        const url = book.originUrls?.[origin];
        if (!url) {
          return { origin, candidate: null };
        }
        const srcName = lite.find((item) => item.bookSourceUrl === origin)?.bookSourceName;
        return {
          origin,
          candidate: {
            bookUrl: url,
            origin,
            originName: srcName ?? origin,
            name: book.name,
            author: book.author,
            type: book.type,
            kind: book.kind,
            coverUrl: book.coverUrl,
            intro: book.intro,
            latestChapterTitle: book.latestChapterTitle,
          },
        };
      });
  }, [book, currentOrigin, sourcesQuery.data]);

  /** 入架成功的公共副作用: 本地成员态 + 换源候选缓存 + 回报调用方 */
  const handleSaved = React.useCallback(
    (saved: Book) => {
      setAdded(true);
      onAdded?.(saved);
      queryClient.setQueryData(["availableBookSources", saved.bookUrl, 0], [
        toAvailableBookSource(saved),
      ]);
    },
    [onAdded, queryClient],
  );

  const addToShelf = useAddToShelf({ onSaved: handleSaved });
  /** 已入架书换源: 后端 setBookSource 改书架行链接与源 */
  const changeSource = useMutation({
    mutationFn: (target: AvailableBookSource) =>
      setBookSource({
        bookUrl: currentBookUrl,
        newBookUrl: target.bookUrl,
        bookSourceUrl: target.origin,
      }),
    onSuccess: (saved) => {
      // 先用后端返回的新书籍信息填充详情(免一次往返), 新 query key 挂载后再由 getBookInfo 后台复核
      queryClient.setQueryData(["bookInfo", saved.bookUrl, saved.origin], saved);
      setIdentity({ bookUrl: saved.bookUrl, origin: saved.origin });
      setDeepSearch(false);
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      toast.success(`已换源: ${saved.originName}`);
    },
    onError: (error) => {
      toast.error(humanizeError(errorMessage(error, "换源失败")));
    },
  });

  // 大目录首次抓取可达数十秒 (warp 载入后缓存): 显示已等待秒数安抚
  const [tocWait, setTocWait] = React.useState(0);
  React.useEffect(() => {
    const started = Date.now();
    const timer = window.setInterval(() => {
      setTocWait(Math.round((Date.now() - started) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [chapters.isLoading]);

  // warp getBookInfo 对抓取失败/规则缺失的字段回空串而非 null: 空串一律回退搜索结果值
  const name = (info?.name ?? "").trim() || book.name;
  const author = (info?.author ?? "").trim() || book.author;
  const cover =
    info !== undefined && (info.coverUrl || info.customCoverUrl)
      ? bookCover(info)
      : getCoverUrl(book.coverUrl);
  const intro =
    ((info === undefined ? book.intro : (info.customIntro ?? info.intro)) ?? "").trim() ||
    (book.intro ?? "").trim();
  const kind = (info?.kind ?? "").trim() || book.kind || "";
  const originCount = book.origins?.length ?? 1;
  const byline = [
    author.trim().length > 0 ? author.trim() : "佚名",
    (info?.originName ?? book.originName) || currentOrigin,
    kind.trim(),
  ]
    .filter((part) => part.length > 0)
    .join(" · ");

  const candidates = React.useMemo(() => {
    const list = [...(available.data ?? [])];
    const known = new Set(list.map((item) => `${item.bookUrl}|${item.origin}`));
    for (const entry of originCandidatesData ?? []) {
      const item = entry.candidate;
      if (item === null) {
        continue;
      }
      const key = `${item.bookUrl}|${item.origin}`;
      if (!known.has(key)) {
        list.push(item);
        known.add(key);
      }
    }
    return list;
  }, [available.data, originCandidatesData]);
  /** 搜索命中过但现在取不到书的源: 灰行列出并说明, 不静默消失 */
  const deadOrigins = React.useMemo(
    () => (originCandidatesData ?? []).filter((entry) => entry.candidate === null).map((entry) => entry.origin),
    [originCandidatesData],
  );
  const busy = addToShelf.isPending || changeSource.isPending;

  const handleStartReading = () => {
    const target = shelfBook ?? info;
    // 已入架带进度直读; 未入架也直接进阅读器(URL 携书源), 不再自动入架
    navigate(
      readerPath(
        target?.bookUrl ?? book.bookUrl,
        target?.durChapterIndex ?? 0,
        inShelf ? undefined : target?.origin ?? currentOrigin,
      ),
    );
  };

  const handleOpenChapter = (index: number) => {
    const target = shelfBook ?? info;
    // 未入架点章节同样直进阅读器(携书源), 不再自动入架
    navigate(
      readerPath(target?.bookUrl ?? currentBookUrl, index, inShelf ? undefined : currentOrigin),
    );
  };

  return (
    <div className="flex flex-col gap-8 p-4 md:p-6 md:pb-8">
      {/* ---------- hero: 左封面 + 右信息 ---------- */}
      <section className="grid gap-6 sm:gap-8 md:grid-cols-[220px_minmax(0,1fr)]">
        <div className="mx-auto w-40 sm:w-48 md:mx-0 md:w-full">
          <CoverImage
            src={cover}
            alt={name}
            author={author}
            intro={intro}
            className="shadow-xl"
          />
        </div>

        <div className="flex min-w-0 flex-col">
          {/* 顶部一行给右上角关闭按钮让位 */}
          <div className="flex flex-wrap items-center gap-2 pr-8 md:pr-10">
            <p className="text-xs font-medium tracking-[0.2em] text-accent uppercase">
              Book Detail
            </p>
            {info?.type === 1 ? (
              <Badge variant="accent" size="sm">
                音频
              </Badge>
            ) : null}
            {currentOrigin === LOCAL_ORIGIN ? (
              <Badge variant="muted" size="sm">
                本地
              </Badge>
            ) : null}
            {originCount > 1 ? (
              <Badge variant="outline" size="sm">
                {originCount} 个源
              </Badge>
            ) : null}
            {detail.isFetching ? (
              <Spinner size="sm" label="读取书籍详情" className="ml-auto" />
            ) : null}
          </div>

          <DialogTitle className="mt-3 font-display text-3xl leading-tight font-semibold break-all text-balance md:text-4xl">
            {name}
          </DialogTitle>
          <DialogDescription className="mt-3 truncate text-sm text-muted-foreground">
            {byline}
          </DialogDescription>

          <p className="mt-6 line-clamp-6 text-base leading-8 whitespace-pre-line text-foreground/80">
            {intro.length > 0 ? intro : "暂无简介"}
          </p>

          <div className="mt-7 flex flex-wrap gap-3">
            <Button className="px-5" disabled={busy} onClick={handleStartReading}>
              <BookOpen aria-hidden />
              开始阅读
            </Button>
            {inShelf ? (
              <Button variant="secondary" className="px-5" disabled>
                <Check aria-hidden className="text-accent" />
                已在书架
              </Button>
            ) : (
              <Button
                variant="secondary"
                className="px-5"
                loading={addToShelf.isPending}
                disabled={busy}
                onClick={() => addToShelf.mutate(book)}
              >
                {addToShelf.isPending ? null : <BookPlus aria-hidden />}
                加入书架
              </Button>
            )}
            <Button
              variant="ghost"
              className="px-5"
              aria-expanded={switchOpen}
              disabled={busy}
              onClick={() => setSwitchOpen((open) => !open)}
            >
              <ArrowLeftRight aria-hidden />
              换源
            </Button>
          </div>

          {detail.isError ? (
            <div
              role="alert"
              className="mt-4 flex items-center gap-2 rounded-xl border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger"
            >
              <CircleAlert aria-hidden className="size-4 shrink-0" />
              <span className="min-w-0 flex-1">
                {humanizeError(errorMessage(detail.error, "读取书籍详情失败"))}
              </span>
              <Button size="sm" variant="ghost" onClick={() => void detail.refetch()}>
                重试
              </Button>
            </div>
          ) : null}

          {switchOpen ? (
            <section
              aria-label="切换书源"
              className="mt-4 rounded-xl border border-border bg-background p-4"
            >
              <div className="flex items-center gap-2">
                <h3 className="font-display text-base font-semibold">可用书源</h3>
                {changeSource.isPending ? <Spinner size="sm" label="换源中" /> : null}
                <Button
                  size="sm"
                  variant="ghost"
                  className="ml-auto"
                  loading={available.isFetching}
                  disabled={!inShelf || changeSource.isPending}
                  onClick={() => setDeepSearch(true)}
                >
                  {available.isFetching ? null : <RefreshCw aria-hidden />}
                  重新搜索
                </Button>
              </div>

              {!exactInShelf ? (
                <p className="mt-2 text-xs text-muted-foreground">
                  未入架: 换源只作用于本次预览, 不会加入书架
                </p>
              ) : null}
              {(exactInShelf ? available.isLoading : sourcesQuery.isLoading) ? (
                <p className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                  <Spinner size="sm" label="读取可用书源" />
                  正在读取可用书源
                </p>
              ) : exactInShelf && available.isError ? (
                <p className="mt-2 text-xs text-danger">
                  {humanizeError(errorMessage(available.error, "读取可用书源失败"))}
                </p>
              ) : candidates.length === 0 && deadOrigins.length === 0 ? (
                <p className="mt-2 text-xs text-muted-foreground">
                  {exactInShelf
                    ? "暂无可用书源, 点「重新搜索」用全部书源找一遍"
                    : "搜索结果只命中当前源, 暂无其他书源"}
                </p>
              ) : (
                <ul className="mt-2 flex flex-col gap-1">
                  {candidates.map((item) => {
                    // 当前源同样可点: 后端会按该源重取书籍信息与目录, 相当于「刷新这本书」
                    const current = item.origin === currentOrigin && item.bookUrl === currentBookUrl;
                    return (
                      <li key={`${item.bookUrl}|${item.origin}`}>
                        <button
                          type="button"
                          disabled={busy}
                          onClick={() => {
                            if (exactInShelf) {
                              changeSource.mutate(item);
                              return;
                            }
                            // 未入架: 纯前端切身份, 详情/目录/后续阅读全部跟随新源, 不动书架
                            setIdentity({ bookUrl: item.bookUrl, origin: item.origin });
                            setSwitchOpen(false);
                            toast.success(`已换源: ${item.originName}`);
                          }}
                          className="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition duration-150 ease-out hover:bg-surface-muted focus-visible:ring-2 focus-visible:ring-accent/60 disabled:pointer-events-none disabled:opacity-60"
                        >
                          <span className="min-w-0 flex-1 truncate">{item.originName}</span>
                          {item.origin === LOCAL_ORIGIN ? (
                            <Badge variant="muted" size="sm">
                              本地
                            </Badge>
                          ) : null}
                          {item.latestChapterTitle !== undefined &&
                          item.latestChapterTitle.length > 0 ? (
                            <span className="hidden max-w-40 truncate text-xs text-muted-foreground sm:block">
                              {item.latestChapterTitle}
                            </span>
                          ) : null}
                          {current ? (
                            <Badge variant="accent" size="sm">
                              当前
                            </Badge>
                          ) : null}
                        </button>
                      </li>
                    );
                  })}
                  {deadOrigins.map((origin) => (
                    <li key={origin}>
                      <div className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-muted-foreground/70">
                        <span className="min-w-0 flex-1 truncate">
                          {sourcesQuery.data?.find((source) => source.bookSourceUrl === origin)
                            ?.bookSourceName ?? origin}
                        </span>
                        <Badge variant="muted" size="sm">
                          不可达
                        </Badge>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          ) : null}
        </div>
      </section>

      {/* ---------- 章节目录 ---------- */}
      <section aria-label="章节目录">
        <div className="flex items-center justify-between gap-3 border-b border-border pb-3">
          <h2 className="font-display text-xl font-semibold">章节目录</h2>
          <span className="shrink-0 text-xs text-muted-foreground">
            {chapters.isLoading
              ? tocWait > 3
                ? `正在读取目录 · 已等 ${tocWait}s (大目录首次较慢, 载入后缓存)`
                : "正在读取目录"
              : chapters.isError
                ? "目录读取失败"
                : `共 ${chapterList.length} 章`}
          </span>
        </div>

        {chapters.isLoading ? (
          <div aria-hidden className="flex flex-col divide-y divide-border/60">
            {Array.from({ length: 6 }, (_, index) => (
              <div key={index} className="flex items-center py-4">
                <Skeleton className={index % 3 === 2 ? "w-2/5" : "w-3/5"} />
              </div>
            ))}
          </div>
        ) : chapters.isError ? (
          <div
            role="alert"
            className="flex items-center gap-2 py-4 text-xs text-danger"
          >
            <CircleAlert aria-hidden className="size-4 shrink-0" />
            <span className="min-w-0 flex-1">
              {humanizeError(errorMessage(chapters.error, "读取章节目录失败"))}
            </span>
            <Button size="sm" variant="ghost" onClick={() => void chapters.refetch()}>
              重试
            </Button>
          </div>
        ) : chapterList.length === 0 ? (
          <p className="py-4 text-sm text-muted-foreground">这本书没有目录</p>
        ) : (
          <div
            className={cn(
              "divide-y divide-border/60",
              chapterList.length > CHAPTER_SCROLL_LIMIT && "max-h-80 overflow-y-auto",
            )}
          >
            {chapterList.map((chapter, position) =>
              chapter.isVolume ? (
                // 分卷标题: 没有正文可跳, 只作视觉分隔
                <p
                  key={`${chapter.url}|${position}`}
                  className="py-3 text-xs font-medium tracking-wide text-muted-foreground"
                >
                  {chapter.title}
                </p>
              ) : (
                <button
                  key={`${chapter.url}|${position}`}
                  type="button"
                  disabled={busy}
                  onClick={() => handleOpenChapter(chapter.index)}
                  className="flex w-full cursor-pointer items-center gap-3 py-3.5 text-left text-sm transition-colors duration-150 ease-out hover:text-accent focus-visible:text-accent focus-visible:outline-none disabled:pointer-events-none disabled:opacity-60"
                >
                  <span className="min-w-0 flex-1 truncate">{chapter.title}</span>
                </button>
              ),
            )}
          </div>
        )}
      </section>
    </div>
  );
}
