import { CircleAlert, LibraryBig, Link2Off } from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";

import { AnnotationsDrawer } from "@/components/reader/AnnotationsDrawer";
import {
  AddBookmarkDialog,
  BookmarksDrawer,
  locateBookmark,
  useBookmarkActions,
  type BookmarkDraft,
} from "@/components/reader/BookmarksDrawer";
import type { ChapterEndBlockProps } from "@/components/reader/ChapterEndBlock";
import type { Book } from "@/types/api";
import { ContentView, ReaderLoading, type FocusRequest } from "@/components/reader/ContentView";
import { ReaderChapterBar, ReaderTopBar } from "@/components/reader/ReaderToolbar";
import { SelectionToolbar } from "@/components/reader/SelectionToolbar";
import { SettingsPanel } from "@/components/reader/SettingsPanel";
import { TocDrawer } from "@/components/reader/TocDrawer";
import { TtsBar } from "@/components/reader/TtsBar";
import { useShelfToggle } from "@/components/reader/useShelfToggle";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  EmptyState,
  cn,
  toast,
} from "@/components/ui";
import { useBookData } from "@/hooks/useBookData";
import { useChapterContent } from "@/hooks/useChapterContent";
import { useReaderProgress } from "@/hooks/useReaderProgress";
import { useReaderTheme } from "@/hooks/useReaderTheme";
import { useToc } from "@/hooks/useToc";
import { useTts } from "@/hooks/useTts";
import { humanizeError } from "@/lib/errors";
import { BOOKS_QUERY_KEY, errorMessage } from "@/hooks/useBookshelf";
import { saveBook } from "@/services/bookshelf";
import { getAvailableBookSource, setBookSource, type AvailableBookSource } from "@/services/explore";
import { createBookmark, type Bookmark } from "@/services/bookmarks";
import {
  NO_ANNOTATIONS,
  useAnnotationsStore,
  type Annotation,
} from "@/stores/annotations-store";
import { useReaderUIStore } from "@/stores/reader-ui-store";
import { useSettingsStore } from "@/stores/settings-store";

/**
 * 全屏阅读容器: 覆盖 AppShell, 主题色 token 由 <html> 上的 .dark/data-theme 提供.
 * immersive 是 Fullscreen API 被拒时的回落态, 抬到 z-50 把剩下的壳也盖住.
 */
function ReaderFrame({
  centered = false,
  immersive = false,
  children,
}: {
  centered?: boolean;
  immersive?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "fixed inset-0 z-30 bg-background text-foreground",
        immersive && "z-50",
        centered && "flex items-center justify-center",
      )}
    >
      {children}
    </div>
  );
}

/** 后端「未配置书源」= 本书没有任何可用书源, 重试无用, 只能去书源页导入/启用 */
function isMissingBookSource(raw: string): boolean {
  return /未配置书源|没有找到书源|无可用书源/.test(raw);
}

/**
 * 阅读器页面 (沉浸式, 不进 AppShell):
 * 常显顶栏 (返回/书签/书架/批注/朗读/自动滚动/沉浸/目录/设置 + 本章进度条) + 正文 + 常显底栏 (切章/书签/日夜)
 * + 目录/书签/批注抽屉 + 设置浮动面板 + 划选批注工具条 + 朗读浮动条.
 * 章末块 (纯标记/上下章导航) 由正文自己渲染, 见 ContentView.
 * 路由: /reader?url={bookUrl}&index={chapterIndex?}
 */
export default function ReaderPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const bookUrl = searchParams.get("url") ?? "";
  // 搜索预览直达携带的书源提示: 未入架书用它解析目录/正文
  const bookSourceHint = searchParams.get("bookSource") ?? undefined;

  useReaderTheme();

  const {
    book,
    chapters,
    bookmarks,
    bookSourceUrl,
    inShelf,
    bookQuery,
    chaptersQuery,
    bookmarksQuery,
    refreshChapters,
  } = useBookData(bookUrl, bookSourceHint);
  const [tocRefreshing, setTocRefreshing] = React.useState(false);
  const chapterTitles = React.useMemo(() => chapters.map((chapter) => chapter.title), [chapters]);

  /** 换源面板直接打开: 未入架书在面板里给显式「加入书架」入口, 不再静默入架 */
  const openSwitchSource = (): void => {
    setSwitchOpen(true);
  };

  const progress = useReaderProgress({
    bookUrl,
    durChapterIndex: book?.durChapterIndex,
    durChapterPos: book?.durChapterPos,
    chapterCount: chapters.length,
    chapterTitles,
    searchParams,
    setSearchParams,
  });
  const { stepChapter, goToChapter } = progress;

  // 空正文救出口: 候选源列表 → setBookSource → 失效重载
  const queryClient = useQueryClient();
  const [switchOpen, setSwitchOpen] = React.useState(false);
  const switchCandidates = useQuery({
    queryKey: ["switchCandidates", bookUrl],
    queryFn: () => getAvailableBookSource(bookUrl),
    enabled: switchOpen && bookUrl !== "",
  });
  /** 刚点过「加入书架」: 后端读缓存数秒, 书架列表不会立刻反映, 面板分支用它兜底 */
  const [justAdded, setJustAdded] = React.useState(false);
  /** 未入架书换源前置: 用户显式点击才入架 (候选接口只查书架书) */
  const addForSwitch = useMutation({
    mutationFn: (target: Book) => saveBook(target),
    onSuccess: () => {
      setJustAdded(true);
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      void queryClient.invalidateQueries({ queryKey: ["switchCandidates", bookUrl] });
    },
    onError: (error) => {
      toast.error(humanizeError(errorMessage(error, "加入书架失败")));
    },
  });

  const applySwitch = useMutation({
    mutationFn: (candidate: AvailableBookSource) =>
      setBookSource({ bookUrl, newBookUrl: candidate.bookUrl, bookSourceUrl: candidate.origin }),
    onSuccess: () => {
      setSwitchOpen(false);
      toast.success("已换源, 正在重新加载");
      void queryClient.invalidateQueries({ queryKey: ["bookInfo", bookUrl] });
      void queryClient.invalidateQueries({ queryKey: ["chapters", bookUrl] });
      void queryClient.invalidateQueries({ queryKey: ["content", bookUrl] });
    },
    onError: (error) => {
      toast.error(humanizeError(errorMessage(error, "换源失败")));
    },
  });

  const chapter = chapters[progress.index];
  const content = useChapterContent(
    bookUrl,
    progress.index,
    chapter,
    bookSourceUrl ?? book?.origin,
    chapters.length,
    chapters[progress.index + 1]?.url,
    chapters[progress.index + 2]?.url,
    chapters[progress.index + 1]?.title,
    chapters[progress.index + 2]?.title,
  );

  const toc = useToc({ book, onNavigate: goToChapter });

  const readMode = useSettingsStore((state) => state.readMode);
  const setTocOpen = useReaderUIStore((state) => state.setTocOpen);
  const setSettingsOpen = useReaderUIStore((state) => state.setSettingsOpen);

  // 书签: 抽屉开关 + 待存草稿 (点「加入书签」时从正文定位当前段落)
  const bookmarkActions = useBookmarkActions();
  const [bookmarksOpen, setBookmarksOpen] = React.useState(false);
  const [draft, setDraft] = React.useState<BookmarkDraft | null>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  // 批注抽屉开关 (与书签抽屉同款页面级 state; 顶栏 Highlighter 打开)
  const [annotationsOpen, setAnnotationsOpen] = React.useState(false);

  // 划选批注: 只存本机 (localStorage), 按章节过滤出本章批注交给正文渲染 <mark>
  const bookAnnotations = useAnnotationsStore((state) =>
    bookUrl === "" ? NO_ANNOTATIONS : (state.byBook[bookUrl] ?? NO_ANNOTATIONS),
  );
  const removeAnnotation = useAnnotationsStore((state) => state.remove);
  const chapterAnnotations = React.useMemo(
    () => bookAnnotations.filter((annotation) => annotation.chapterIndex === progress.index),
    [bookAnnotations, progress.index],
  );

  // 批注抽屉跳转: 先记下目标 (章 + 段), 目标章就位后交给 ContentView 滚动并闪烁
  const [focus, setFocus] = React.useState<(FocusRequest & { chapterIndex: number }) | null>(
    null,
  );
  const activeFocus = React.useMemo(
    () =>
      focus !== null && focus.chapterIndex === progress.index
        ? { paraIndex: focus.paraIndex, nonce: focus.nonce }
        : null,
    [focus, progress.index],
  );
  const handleFocusHandled = React.useCallback(() => setFocus(null), []);

  // 顶栏进度条: 本章阅读比例 (滚动模式 = 滚动进度, 翻页模式 = 页码占比)
  const [readPercent, setReadPercent] = React.useState(0);
  const handleReadRatio = React.useCallback((ratio: number) => {
    const next = Math.round(Math.min(1, Math.max(0, ratio)) * 100);
    // 百分比没变就返回原值, React 直接跳过这次重渲染
    setReadPercent((previous) => (previous === next ? previous : next));
  }, []);

  // 自动滚动 / 沉浸阅读: 状态留在页面层, 换章 (ContentView 重挂) 也不丢
  const [autoScroll, setAutoScroll] = React.useState(false);
  const handleAutoScrollStop = React.useCallback(() => {
    setAutoScroll(false);
  }, []);

  const [immersive, setImmersive] = React.useState(false);
  React.useEffect(() => {
    const onFullscreenChange = () => {
      setImmersive(document.fullscreenElement !== null);
    };
    document.addEventListener("fullscreenchange", onFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", onFullscreenChange);
  }, []);
  const handleToggleImmersive = React.useCallback(() => {
    if (document.fullscreenElement !== null) {
      document.exitFullscreen().catch(() => setImmersive(false));
      return;
    }
    // Fullscreen API 被拒 (非手势触发 / iframe 无权限) 时静默回落成覆盖式全屏
    if (immersive) {
      setImmersive(false);
      return;
    }
    document.documentElement.requestFullscreen().catch(() => setImmersive(true));
  }, [immersive]);

  const shelf = useShelfToggle(book);

  const handleAddBookmark = React.useCallback(() => {
    const target = chapters[progress.index];
    if (book === undefined || target === undefined) {
      toast.info("正文加载完成后才能加入书签");
      return;
    }
    const excerpt = locateBookmark(contentRef.current, content.items);
    setDraft({
      chapterIndex: target.index,
      chapterName: target.title,
      chapterPos: excerpt.pos,
      bookText: excerpt.text,
    });
  }, [book, chapters, content.items, progress.index]);

  // 本章书签: warp 允许一书多条(主键 bookUrl+位置), 落在已有书签的章节时顶栏显示「已加」
  const currentBookmark = React.useMemo(
    () => bookmarks.find((bookmark) => bookmark.chapterIndex === progress.index),
    [bookmarks, progress.index],
  );

  // 顶栏的书签开关: 已加则删掉, 没加则走既有弹窗 (摘当前段落 + 覆盖确认)
  const handleToggleBookmark = React.useCallback(() => {
    if (currentBookmark !== undefined) {
      bookmarkActions.remove(currentBookmark);
      return;
    }
    handleAddBookmark();
  }, [bookmarkActions, currentBookmark, handleAddBookmark]);

  const handleSaveBookmark = React.useCallback(
    (note: string) => {
      if (draft === null || book === undefined) {
        return;
      }
      bookmarkActions.add(
        createBookmark({
          bookUrl: book.bookUrl,
          bookName: book.name,
          bookAuthor: book.author,
          chapterIndex: draft.chapterIndex,
          chapterPos: draft.chapterPos,
          chapterName: draft.chapterName,
          bookText: draft.bookText,
          content: note,
        }),
        () => setDraft(null),
      );
    },
    [book, bookmarkActions, draft],
  );

  // 点批注抽屉条目: 关抽屉, 必要时跳章, 然后交给 ContentView 滚到段落并闪烁 2s
  const handleSelectAnnotation = React.useCallback(
    (annotation: Annotation) => {
      setAnnotationsOpen(false);
      if (annotation.chapterIndex !== progress.index) {
        goToChapter(annotation.chapterIndex);
      }
      setFocus({
        chapterIndex: annotation.chapterIndex,
        paraIndex: annotation.paraIndex,
        nonce: Date.now(),
      });
    },
    [goToChapter, progress.index],
  );

  const handleDeleteAnnotation = React.useCallback(
    (annotation: Annotation) => {
      removeAnnotation(bookUrl, annotation.id);
    },
    [removeAnnotation, bookUrl],
  );

  // 点书签跳章 (章内位置由 useReaderProgress 的恢复逻辑决定, 后端书签的 chapterPos 仅存档)
  const handleSelectBookmark = React.useCallback(
    (bookmark: Bookmark) => {
      setBookmarksOpen(false);
      goToChapter(bookmark.chapterIndex);
    },
    [goToChapter],
  );

  const handleClearBookmarks = React.useCallback(() => {
    bookmarkActions.clear(bookmarks);
  }, [bookmarkActions, bookmarks]);

  // 书签拉取失败不阻塞阅读: 原始串留在 react-query 的 error 里, 抽屉只展示映射后的人话
  const bookmarkError = bookmarksQuery.isError
    ? humanizeError(bookmarksQuery.error?.message ?? "", "书签加载失败")
    : "";

  const handlePrevChapter = React.useCallback(() => stepChapter(-1), [stepChapter]);
  const handleNextChapter = React.useCallback(() => stepChapter(1), [stepChapter]);

  // 语音朗读 (Web Speech API): 段落与正文同源, 起读位置取可视区首段, 章末按连读开关决定续读
  const tts = useTts({
    items: content.items,
    containerRef: contentRef,
    chapterIndex: progress.index,
    chapterCount: chapters.length,
    resetKey: bookUrl,
    onNextChapter: handleNextChapter,
    onStopAutoScroll: handleAutoScrollStop,
  });

  // 自动滚动与朗读互斥: 打开自动滚动先停朗读 (反向由 useTts 起播时关掉自动滚动)
  const handleToggleAutoScroll = React.useCallback(() => {
    const next = !autoScroll;
    if (next) {
      tts.stop();
    }
    setAutoScroll(next);
  }, [autoScroll, tts]);

  const handleBack = React.useCallback(() => {
    if (window.history.length > 1) {
      navigate(-1);
    } else {
      navigate("/");
    }
  }, [navigate]);

  const handleGoToSources = React.useCallback(() => navigate("/sources"), [navigate]);

  const handleOpenToc = React.useCallback(() => setTocOpen(true), [setTocOpen]);
  const handleOpenSettings = React.useCallback(() => setSettingsOpen(true), [setSettingsOpen]);
  const handleOpenBookmarks = React.useCallback(() => setBookmarksOpen(true), []);
  const handleOpenAnnotations = React.useCallback(() => setAnnotationsOpen(true), []);

  // 章末块的数据与回调: memo 好整体交给 ContentView (Virtuoso 直接拿它当 context)
  const endBlock = React.useMemo<ChapterEndBlockProps>(
    () => ({
      index: progress.index,
      total: chapters.length,
      nextTitle: chapters[progress.index + 1]?.title ?? null,
      onPrevChapter: handlePrevChapter,
      onNextChapter: handleNextChapter,
    }),
    [progress.index, chapters, handlePrevChapter, handleNextChapter],
  );

  // Esc 关闭全部浮层 (←/→ 在 ContentView 内处理; 顶栏底栏常显, 没有需要点出来的工具栏)
  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) {
        return;
      }
      if (event.key !== "Escape") {
        return;
      }
      const target = event.target;
      if (target instanceof HTMLElement) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable) {
          return;
        }
      }
      useReaderUIStore.getState().closeAll();
      setAnnotationsOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  if (bookUrl === "") {
    return (
      <ReaderFrame centered>
        <EmptyState
          icon={<Link2Off />}
          title="缺少书籍链接"
          description="请从书架或搜索结果进入阅读器."
          action={<Button onClick={() => navigate("/")}>返回书架</Button>}
        />
      </ReaderFrame>
    );
  }

  if (bookQuery.isLoading || chaptersQuery.isLoading) {
    return (
      <ReaderFrame centered>
        <ReaderLoading label="正在加载章节…" />
      </ReaderFrame>
    );
  }

  if (book === undefined || bookQuery.isError || chaptersQuery.isError) {
    const failed = bookQuery.isError ? bookQuery.error : chaptersQuery.error;
    const raw = failed?.message ?? "";
    const missingSource = isMissingBookSource(raw);
    const message = failed === null ? "书籍信息缺失" : humanizeError(raw, "网络或服务器异常");
    return (
      <ReaderFrame centered>
        <EmptyState
          icon={<CircleAlert />}
          title="书籍加载失败"
          description={message}
          action={
            <>
              {missingSource ? (
                <Button onClick={handleGoToSources}>
                  <LibraryBig aria-hidden />
                  去书源页
                </Button>
              ) : null}
              <Button
                variant="secondary"
                onClick={() => {
                  void bookQuery.refetch();
                  void chaptersQuery.refetch();
                }}
              >
                重试
              </Button>
              <Button variant={missingSource ? "ghost" : "primary"} onClick={handleBack}>
                返回
              </Button>
            </>
          }
        />
      </ReaderFrame>
    );
  }

  if (chapters.length === 0) {
    return (
      <ReaderFrame centered>
        <EmptyState
          icon={<CircleAlert />}
          title="章节列表为空"
          description="本书暂时没有可阅读的章节, 可以刷新目录重试."
          action={
            <>
              <Button
                loading={tocRefreshing}
                onClick={() => {
                  setTocRefreshing(true);
                  void refreshChapters().finally(() => setTocRefreshing(false));
                }}
              >
                刷新目录
              </Button>
              <Button variant="secondary" onClick={openSwitchSource}>
                换源
              </Button>
              <Button variant="secondary" onClick={handleBack}>
                返回
              </Button>
            </>
          }
        />
      </ReaderFrame>
    );
  }

  return (
    <ReaderFrame immersive={immersive}>
      <div className="flex h-full flex-col">
        <ReaderTopBar
          bookName={book.name}
          chapterTitle={chapter?.title ?? ""}
          percent={readPercent}
          onBack={handleBack}
          bookmarked={currentBookmark !== undefined}
          onToggleBookmark={handleToggleBookmark}
          inShelf={shelf.inShelf}
          shelfDisabled={shelf.isLoading || shelf.busy}
          onToggleShelf={shelf.toggle}
          onOpenAnnotations={handleOpenAnnotations}
          ttsSupported={tts.supported}
          ttsActive={tts.status !== "idle"}
          onToggleTts={tts.toggle}
          showAutoScroll={readMode === "scroll"}
          autoScroll={autoScroll}
          onToggleAutoScroll={handleToggleAutoScroll}
          immersive={immersive}
          onToggleImmersive={handleToggleImmersive}
          onOpenToc={handleOpenToc}
          onOpenSettings={handleOpenSettings}
        />

        {/* 正文容器: 加入书签时扫描其中带 data-pos 的段落取可视区首段作摘要; 划选工具条也只认这个容器内的选区 */}
        <div ref={contentRef} className="min-h-0 flex-1">
          <ContentView
            key={progress.index}
            items={content.items}
            isPending={content.isPending}
            isError={content.isError}
            errorMessage={content.errorMessage}
            isEmpty={content.isEmpty}
            onRetry={content.refetch}
            onSwitchSource={openSwitchSource}
            readMode={readMode}
            bookName={book.name}
            bookAuthor={book.author}
            restorePos={progress.restorePos}
            onPosChange={progress.reportPos}
            onReadRatio={handleReadRatio}
            onPrevChapter={handlePrevChapter}
            onNextChapter={handleNextChapter}
            annotations={chapterAnnotations}
            focusRequest={activeFocus}
            onFocusHandled={handleFocusHandled}
            endBlock={endBlock}
            autoScroll={autoScroll}
            onAutoScrollStop={handleAutoScrollStop}
            onGoToSources={
              content.isError && isMissingBookSource(content.errorMessage)
                ? handleGoToSources
                : undefined
            }
          />
        </div>

        <ReaderChapterBar
          chapters={chapters}
          index={progress.index}
          onGoToChapter={goToChapter}
          onOpenBookmarks={handleOpenBookmarks}
        />
      </div>

      {/* 朗读浮动条: 盖在底栏上方, idle 时组件自己不渲染 */}
      <TtsBar tts={tts} />

      <TocDrawer
        open={toc.tocOpen}
        onOpenChange={toc.setTocOpen}
        bookName={book.name}
        bookAuthor={book.author}
        chapters={chapters}
        currentIndex={progress.index}
        reversed={toc.reversed}
        onToggleReversed={toc.toggleReversed}
        onSelect={toc.selectChapter}
      />
      <BookmarksDrawer
        open={bookmarksOpen}
        onOpenChange={setBookmarksOpen}
        bookmarks={bookmarks}
        currentIndex={progress.index}
        isLoading={bookmarksQuery.isLoading}
        errorMessage={bookmarkError}
        onRetry={() => void bookmarksQuery.refetch()}
        onSelect={handleSelectBookmark}
        onDelete={bookmarkActions.remove}
        onClear={handleClearBookmarks}
        busy={bookmarkActions.busy}
      />
      <AnnotationsDrawer
        open={annotationsOpen}
        onOpenChange={setAnnotationsOpen}
        annotations={bookAnnotations}
        chapters={chapters}
        currentIndex={progress.index}
        onSelect={handleSelectAnnotation}
        onDelete={handleDeleteAnnotation}
      />
      <AddBookmarkDialog
        draft={draft}
        onOpenChange={(open) => {
          if (!open) {
            setDraft(null);
          }
        }}
        existing={
          draft === null
            ? undefined
            : bookmarks.find(
                (bookmark) =>
                  bookmark.chapterIndex === draft.chapterIndex &&
                  bookmark.chapterPos === draft.chapterPos,
              )
        }
        pending={bookmarkActions.adding}
        onSave={handleSaveBookmark}
      />
      <SelectionToolbar
        containerRef={contentRef}
        bookUrl={bookUrl}
        chapterIndex={progress.index}
      />
      <SettingsPanel />

      <Dialog open={switchOpen} onOpenChange={setSwitchOpen}>
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>切换书源</DialogTitle>
          </DialogHeader>
          <div className="max-h-80 overflow-y-auto px-4 md:px-5">
            {!inShelf && !justAdded ? (
              <div className="flex flex-col items-center gap-3 py-6 text-center text-sm text-muted-foreground">
                <p>未入架的书换源需要先加入书架 (用于保存进度与同步)</p>
                <Button
                  size="sm"
                  loading={addForSwitch.isPending}
                  disabled={book === undefined}
                  onClick={() => {
                    if (book !== undefined) {
                      addForSwitch.mutate(book);
                    }
                  }}
                >
                  加入书架并继续换源
                </Button>
              </div>
            ) : switchCandidates.isLoading ? (
              <p className="py-6 text-center text-sm text-muted-foreground">正在获取候选书源…</p>
            ) : (switchCandidates.data ?? []).length === 0 ? (
              <p className="py-6 text-center text-sm text-muted-foreground">暂无其他可用书源</p>
            ) : (
              <div className="divide-y divide-border/70">
                {(switchCandidates.data ?? []).map((candidate) => (
                  <button
                    key={candidate.bookUrl}
                    type="button"
                    disabled={applySwitch.isPending}
                    onClick={() => applySwitch.mutate(candidate)}
                    className="flex w-full cursor-pointer items-center justify-between gap-3 py-3 text-left text-sm hover:text-accent disabled:opacity-50"
                  >
                    <span className="min-w-0 truncate">{candidate.originName || candidate.origin}</span>
                    <span className="shrink-0 text-xs text-muted-foreground">
                      {candidate.latestChapterTitle ?? ""}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </ReaderFrame>
  );
}
