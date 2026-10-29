import { ArrowLeft, CircleAlert, LibraryBig, Link2Off } from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";

import { GraphGenerateDialog } from "@/components/graph-demo/GraphGenerateDialog";
import { AiChatPanel } from "@/components/reader/AiChatPanel";
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
import { ContentView, type FocusRequest } from "@/components/reader/ContentView";
import { ReaderLoadingStatus, ReaderStateCard } from "@/components/reader/ReaderStatus";
import { ReaderChapterBar, ReaderTopBar } from "@/components/reader/ReaderToolbar";
import { SelectionToolbar } from "@/components/reader/SelectionToolbar";
import { SettingsPanel } from "@/components/reader/SettingsPanel";
import { TocDrawer } from "@/components/reader/TocDrawer";
import { TtsBar } from "@/components/reader/TtsBar";
import { useShelfToggle } from "@/components/reader/useShelfToggle";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, EmptyState, cn, toast } from "@/components/ui";
import { useBookData } from "@/hooks/useBookData";
import { useChapterContent } from "@/hooks/useChapterContent";
import { useReaderProgress } from "@/hooks/useReaderProgress";
import { useReaderTheme } from "@/hooks/useReaderTheme";
import { useToc } from "@/hooks/useToc";
import { useTts } from "@/hooks/useTts";
import { humanizeError } from "@/lib/errors";
import { BOOKS_QUERY_KEY, errorMessage } from "@/hooks/useBookshelf";
import { getAiStatus } from "@/services/ai";
import { saveBook } from "@/services/bookshelf";
import { SwitchSourceDialog } from "@/components/book/SwitchSourceDialog";
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
 * 后端原始错误文案: 目录/正文失败的**具体原因**(「目录抓取超时(预算 30s, 已抓 2 页)…」
 * 这类)必须原样交给用户, 不能被人话映射抹平成「网络或服务器异常」—— 那样用户既不知道
 * 等多久、也不知道该重试还是该换源。人话版只在新增一条 hint 时并存, 不替换原文。
 */
function rawErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error) {
    const text = error.message.trim();
    if (text !== "") {
      return text;
    }
  }
  return fallback;
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
    shelfBook,
    shelfQuery,
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
  /** 手动刷新目录的失败原文: 抽屉/错误卡都要能看到「为什么还是拿不到目录」 */
  const [tocError, setTocError] = React.useState("");
  const chapterTitles = React.useMemo(() => chapters.map((chapter) => chapter.title), [chapters]);

  /** 换源面板直接打开: 未入架书在面板里给显式「加入书架」入口, 不再静默入架 */
  const openSwitchSource = (): void => {
    setSwitchOpen(true);
  };

  const progress = useReaderProgress({
    bookUrl,
    // 进度只认书架记录: getBookInfo 不含 durChapter*, 沿用 book 会永远从第 1 章开读,
    // 随后又把服务端进度覆盖成 0 —— 多端"进度不跟随"的根因
    durChapterIndex: shelfBook?.durChapterIndex ?? book?.durChapterIndex,
    durChapterPos: shelfBook?.durChapterPos ?? book?.durChapterPos,
    chapterCount: chapters.length,
    chapterTitles,
    searchParams,
    setSearchParams,
  });
  const { stepChapter, goToChapter } = progress;

  // 进阅读器补一次最新书架数据: 权威进度在书架记录里, 它有 30s staleTime ——
  // 但不能在进书那一刻抢带宽 (它会和书籍详情/目录/正文同时排队, 实测接口并发把首屏推后):
  // 改为「首屏正文已出 + 书架数据确实过期」才补, 且每本书只补一次 (实现见下方 useChapterContent 之后)。
  // 挂载本身就会 fetch (无缓存时 isLoading), 那种情况直接跳过, 与改动前等价。


  /**
   * 多端进度提示: 回到前台/窗口获得焦点时重取服务端进度, 若明显更靠后则提示跳转
   * (PC 读到 220 而手机停在第 100 章时, 手机上给一条「其他设备读到第 220 章」)。
   * 只提示"服务端更靠后"的情况; 已忽略过的位置不再重复打扰。
   */
  const [remoteHint, setRemoteHint] = React.useState<{ index: number; title: string } | null>(null);
  const dismissedRemote = React.useRef<number | null>(null);
  const progressIndexRef = React.useRef(progress.index);
  React.useEffect(() => {
    progressIndexRef.current = progress.index;
  }, [progress.index]);
  React.useEffect(() => {
    const check = async (): Promise<void> => {
      if (document.visibilityState !== "visible" || bookUrl === "") {
        return;
      }
      // 进度只存在于书架记录(getBookInfo 不返回 durChapter*) → 重取书架再取该书
      const result = await shelfQuery.refetch();
      const remote = result.data?.find((item) => item.bookUrl === bookUrl);
      const remoteIndex = remote?.durChapterIndex;
      if (remoteIndex === undefined) {
        return;
      }
      if (remoteIndex > progressIndexRef.current + 1 && dismissedRemote.current !== remoteIndex) {
        setRemoteHint({
          index: remoteIndex,
          title: remote?.durChapterTitle ?? "",
        });
      }
    };
    const onVisible = (): void => {
      if (document.visibilityState === "visible") {
        void check();
      }
    };
    window.addEventListener("focus", onVisible);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", onVisible);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [shelfQuery, bookUrl]);

  // 换源: 面板见 SwitchSourceDialog(与书架详情共用); 未入架时需先入架才能查候选
  const queryClient = useQueryClient();
  const [switchOpen, setSwitchOpen] = React.useState(false);
  /** 刚点过「加入书架」: 后端读缓存数秒, 书架列表不会立刻反映, 面板分支用它兜底 */
  const [justAdded, setJustAdded] = React.useState(false);
  /** 未入架书换源前置: 用户显式点击才入架 (候选接口只查书架书) */
  const addForSwitch = useMutation({
    mutationFn: (target: Book) => saveBook(target),
    onSuccess: () => {
      setJustAdded(true);
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
    },
    onError: (error) => {
      toast.error(humanizeError(errorMessage(error, "加入书架失败")));
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

  // 书架数据补取 (取代原来进书立刻 refetch): 只在「首屏正文已出」且「书架数据过期」时补一次。
  // 断网/后端挂住时这里不会连环重试 —— 首屏可读优先, 多端进度提示另有 focus 检查兜底。
  const shelfIsStale = shelfQuery.isStale;
  const shelfSettled = shelfQuery.isSuccess;
  // 「正文已出」= 有可渲染段落 (缓存命中也算) 或已明确失败
  const bodyReady = content.items.length > 0 || content.isError;
  const shelfSyncedBookRef = React.useRef("");
  const refetchShelf = shelfQuery.refetch;
  React.useEffect(() => {
    if (bookUrl === "" || !bodyReady || !shelfSettled || !shelfIsStale) {
      return;
    }
    if (shelfSyncedBookRef.current === bookUrl) {
      return;
    }
    shelfSyncedBookRef.current = bookUrl;
    void refetchShelf();
  }, [bookUrl, bodyReady, shelfSettled, shelfIsStale, refetchShelf]);

  const toc = useToc({ book, onNavigate: goToChapter });

  // 重新拉取本章: 绕过服务端永久正文缓存 (源站修正过 / 缓存了防盗页); 失败保留原正文只提示
  const { reload: reloadChapter } = content;
  const handleReloadChapter = React.useCallback(() => {
    reloadChapter().then(
      () => {
        toast.success("已重新拉取本章");
      },
      (error: unknown) => {
        toast.error(humanizeError(errorMessage(error, "重新拉取本章失败")));
      },
    );
  }, [reloadChapter]);

  const readMode = useSettingsStore((state) => state.readMode);

  // AI 阅读助手: 设置开关决定入口是否出现; 后端是否配置模型决定面板可用态
  const aiAssistant = useSettingsStore((state) => state.aiAssistant);
  const [aiOpen, setAiOpen] = React.useState(false);
  const aiStatusQuery = useQuery({
    queryKey: ["aiStatus"],
    queryFn: getAiStatus,
    enabled: aiAssistant,
    staleTime: 5 * 60 * 1000,
  });
  /** 点 AI 回答里的章节引用: 按标题反查章节并跳转 */
  const jumpToTitle = React.useCallback(
    (title: string) => {
      // 模型有时把引用写成【第N章 …】/「第N章 …」: 去掉包裹符号再匹配
      const needle = title
        .trim()
        .replace(/^[【\[「『]+/, "")
        .replace(/[】\]」』]+$/, "")
        .trim();
      if (needle === "") {
        return;
      }
      // 引用可能来自模型(标题可能被简写): 精确 → 章号前缀 → 包含匹配
      let idx = chapters.findIndex((c) => c.title === needle);
      if (idx < 0) {
        const head = /^第[0-9零一二三四五六七八九十百千两]+章/.exec(needle);
        if (head !== null) {
          idx = chapters.findIndex((c) => c.title.startsWith(head[0]));
        }
      }
      if (idx < 0) {
        idx = chapters.findIndex(
          (c) => c.title.includes(needle) || needle.includes(c.title),
        );
      }
      if (idx >= 0) {
        goToChapter(idx);
        setAiOpen(false);
      }
    },
    [chapters, goToChapter],
  );
  const setTocOpen = useReaderUIStore((state) => state.setTocOpen);
  const setSettingsOpen = useReaderUIStore((state) => state.setSettingsOpen);

  // 书签: 抽屉开关 + 待存草稿 (点「加入书签」时从正文定位当前段落)
  const bookmarkActions = useBookmarkActions();
  const [bookmarksOpen, setBookmarksOpen] = React.useState(false);
  const [draft, setDraft] = React.useState<BookmarkDraft | null>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  // 批注抽屉开关 (与书签抽屉同款页面级 state; 顶栏 Highlighter 打开)
  const [annotationsOpen, setAnnotationsOpen] = React.useState(false);
  // 关系图谱: 底栏入口打开「预估 → 生成 → 查看」弹窗 (数据与进度都在弹窗内管理)
  const [graphOpen, setGraphOpen] = React.useState(false);

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

  // 点击正文中央/空白: 收起上下工具条让正文占满全屏 (不持久化——进章保持, 刷新恢复常显)
  const [chromeHidden, setChromeHidden] = React.useState(false);
  const handleToggleChrome = React.useCallback(() => {
    setChromeHidden((prev) => !prev);
  }, []);
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
  /** 回书架 (根路由就是书架): 错误态里的「返回书架」用它, 语义比「后退」明确 */
  const handleBackToShelf = React.useCallback(() => navigate("/"), [navigate]);

  /**
   * 目录重取 (抽屉「刷新目录」+ 错误卡「重试」共用): 走 refreshChapters 绕开服务端目录缓存,
   * 成功直接写回查询缓存, 失败把后端原文留在 tocError —— 否则 .catch 缺失会变成
   * 「点了没反应 + console 一个 unhandled rejection」。
   */
  const handleRefreshToc = React.useCallback(() => {
    setTocError("");
    setTocRefreshing(true);
    refreshChapters()
      .catch((error: unknown) => {
        setTocError(rawErrorMessage(error, "刷新目录失败"));
      })
      .finally(() => setTocRefreshing(false));
  }, [refreshChapters]);

  const handleOpenToc = React.useCallback(() => setTocOpen(true), [setTocOpen]);
  const handleOpenSettings = React.useCallback(() => setSettingsOpen(true), [setSettingsOpen]);
  const handleOpenBookmarks = React.useCallback(() => setBookmarksOpen(true), []);
  const handleOpenAnnotations = React.useCallback(() => setAnnotationsOpen(true), []);
  const handleOpenGraph = React.useCallback(() => setGraphOpen(true), []);

  // 章末块的数据与回调: memo 好整体交给 ContentView (Virtuoso 直接拿它当 context)
  const endBlock = React.useMemo<ChapterEndBlockProps>(
    () => ({
      index: progress.index,
      total: chapters.length,
      nextTitle: chapters[progress.index + 1]?.title ?? null,
      onPrevChapter: handlePrevChapter,
      onNextChapter: handleNextChapter,
    }),
    [
      progress.index,
      chapters,
      handlePrevChapter,
      handleNextChapter,
    ],
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

  // 后端原始错误: 「目录抓取超时(预算 30s, 已抓 2 页)」这类具体原因原样展示,
  // 人话版只作为补充 hint 并存, 不替换原文 (否则用户不知道该等/该重试/该换源)
  const bookError = bookQuery.isError ? rawErrorMessage(bookQuery.error, "书籍信息缺失") : "";
  const chaptersError = chaptersQuery.isError
    ? rawErrorMessage(chaptersQuery.error, "目录加载失败")
    : "";
  /** 目录失败的人话版: 只在和原文不同时作为补充 hint 出现 */
  const chaptersHint = humanizeError(chaptersError, "");

  if (book === undefined) {
    // 书籍信息未就绪 (含书架列表未就绪 → 查询还没发起): 全屏加载/错误态,
    // 左上角恒有返回, 失败给重试 + 返回书架 —— 原来这里是一个没有任何出口的死转圈
    const bootError = bookError !== "" ? bookError : chaptersError;
    const bootHint = humanizeError(bootError, "");
    const missingSource = isMissingBookSource(bootError);
    return (
      <ReaderFrame centered>
        <div className="absolute left-3 top-3">
          <Button variant="ghost" onClick={handleBack}>
            <ArrowLeft aria-hidden />
            返回
          </Button>
        </div>
        {bootError === "" ? (
          <ReaderLoadingStatus label="正在加载书籍信息…" />
        ) : (
          <ReaderStateCard
            icon={<CircleAlert />}
            title="书籍加载失败"
            message={bootError}
            hint={bootHint === bootError ? undefined : bootHint}
            actions={
              <>
                {missingSource ? (
                  <Button onClick={handleGoToSources}>
                    <LibraryBig aria-hidden />
                    去书源页
                  </Button>
                ) : null}
                <Button
                  variant={missingSource ? "secondary" : "primary"}
                  loading={bookQuery.isFetching || chaptersQuery.isFetching}
                  onClick={() => {
                    void bookQuery.refetch();
                    void chaptersQuery.refetch();
                  }}
                >
                  重试
                </Button>
                <Button variant="secondary" onClick={handleBackToShelf}>
                  返回书架
                </Button>
              </>
            }
          />
        )}
      </ReaderFrame>
    );
  }

  return (
    <ReaderFrame immersive={immersive}>
      {remoteHint !== null ? (
        <div className="fixed left-1/2 top-3 z-50 -translate-x-1/2">
          <div className="flex items-center gap-3 rounded-full border border-border bg-surface px-3 py-1.5 text-xs shadow-lg">
            <span className="text-muted-foreground">其他设备读到</span>
            <span className="max-w-40 truncate">
              {remoteHint.title || `第 ${remoteHint.index + 1} 章`}
            </span>
            <button
              type="button"
              className="cursor-pointer font-medium text-accent"
              onClick={() => {
                goToChapter(remoteHint.index);
                setRemoteHint(null);
              }}
            >
              跳转
            </button>
            <button
              type="button"
              className="cursor-pointer text-muted-foreground"
              onClick={() => {
                dismissedRemote.current = remoteHint.index;
                setRemoteHint(null);
              }}
            >
              忽略
            </button>
          </div>
        </div>
      ) : null}
      <div className="flex h-full">
        <div className="relative flex min-w-0 flex-1 flex-col">
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
          onOpenAi={aiAssistant ? () => setAiOpen(true) : undefined}
          aiOpen={aiAssistant && aiOpen}
          chromeHidden={chromeHidden}
        />

        {/* 目录来自本地缓存且这次取新失败: 只给一条「可能不是最新」的轻提示, 正文照常可读,
            绝不用整块错误卡把已缓存的目录/正文盖掉 (断网二次进书的核心场景) */}
        {chaptersError !== "" && chapters.length > 0 ? (
          <div className="flex items-center justify-center gap-2 border-b border-border bg-surface px-3 py-1 text-xs text-muted-foreground">
            <span className="truncate">目录可能不是最新(用的是本地缓存), 可刷新目录重试</span>
            <button
              type="button"
              className="shrink-0 cursor-pointer font-medium text-accent disabled:opacity-50"
              disabled={tocRefreshing}
              onClick={handleRefreshToc}
            >
              {tocRefreshing ? "刷新中…" : "刷新"}
            </button>
          </div>
        ) : null}

        {/* 正文容器: 加入书签时扫描其中带 data-pos 的段落取可视区首段作摘要; 划选工具条也只认这个容器内的选区 */}
        <div ref={contentRef} className="min-h-0 flex-1">
          {/* 目录未就绪时不再整页返回一个死转圈: 留在阅读框架内(顶栏「返回」始终可用),
              加载超过 8s 解释原因, 失败/空目录给原文 + 重试/换源/返回书架。
              注意判据都是「没有目录」时才算 loading/失败: 本地缓存已有目录就直接渲染正文 */}
          {chaptersQuery.isLoading && chapters.length === 0 ? (
            <div className="flex h-full items-center justify-center">
              <ReaderLoadingStatus />
            </div>
          ) : chaptersError !== "" && chapters.length === 0 ? (
            <div className="flex h-full items-center justify-center px-6">
              <ReaderStateCard
                icon={<CircleAlert />}
                title="目录加载失败"
                message={tocError !== "" ? tocError : chaptersError}
                hint={chaptersHint === chaptersError ? undefined : chaptersHint}
                actions={
                  <>
                    <Button loading={tocRefreshing} onClick={handleRefreshToc}>
                      重试
                    </Button>
                    <Button variant="secondary" onClick={openSwitchSource}>
                      换源
                    </Button>
                    <Button variant="secondary" onClick={handleBackToShelf}>
                      返回书架
                    </Button>
                  </>
                }
              />
            </div>
          ) : chapters.length === 0 ? (
            <div className="flex h-full items-center justify-center px-6">
              <ReaderStateCard
                icon={<CircleAlert />}
                title="章节列表为空"
                message={
                  tocError !== "" ? tocError : "本书暂时没有可阅读的章节, 可以刷新目录重试."
                }
                actions={
                  <>
                    <Button loading={tocRefreshing} onClick={handleRefreshToc}>
                      刷新目录
                    </Button>
                    <Button variant="secondary" onClick={openSwitchSource}>
                      换源
                    </Button>
                    <Button variant="secondary" onClick={handleBackToShelf}>
                      返回书架
                    </Button>
                  </>
                }
              />
            </div>
          ) : (
          <ContentView
            key={progress.index}
            items={content.items}
            isPending={content.isPending}
            isError={content.isError}
            errorMessage={content.errorMessage}
            isEmpty={content.isEmpty}
            onRetry={content.refetch}
            onReload={handleReloadChapter}
            isReloading={content.isReloading}
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
            onToggleChrome={handleToggleChrome}
          />
          )}
        </div>

        <ReaderChapterBar
          chapters={chapters}
          index={progress.index}
          onGoToChapter={goToChapter}
          onOpenBookmarks={handleOpenBookmarks}
          onOpenGraph={handleOpenGraph}
          chromeHidden={chromeHidden}
        />
        </div>

        <AiChatPanel
          open={aiAssistant && aiOpen}
          onClose={() => setAiOpen(false)}
          bookUrl={bookUrl}
          bookName={book.name}
          titles={chapters.map((c) => c.title)}
          currentTitle={chapters[progress.index]?.title ?? ""}
          configured={aiStatusQuery.data?.enabled === true}
          model={aiStatusQuery.data?.model ?? null}
          onJump={jumpToTitle}
        />
      </div>

      {/* 朗读浮动条: 盖在底栏上方, idle 时组件自己不渲染 */}
      <TtsBar tts={tts} />

      {/* 目录来自缓存时抽屉不该再唱空城计: 有章节就不算 loading, 也不该把已缓存的目录盖成错误页 */}
      <TocDrawer
        open={toc.tocOpen}
        onOpenChange={toc.setTocOpen}
        bookName={book.name}
        bookAuthor={book.author}
        chapters={chapters}
        currentIndex={progress.index}
        totalChapterNum={shelfBook?.totalChapterNum ?? book?.totalChapterNum}
        onReloadChapter={handleReloadChapter}
        reloadingChapter={content.isReloading}
        refreshingToc={tocRefreshing}
        tocError={chapters.length === 0 ? (chaptersError !== "" ? chaptersError : tocError) : tocError}
        tocLoading={chaptersQuery.isLoading && chapters.length === 0}
        onRefreshToc={handleRefreshToc}
        onSwitchSource={() => {
          toc.setTocOpen(false);
          setSwitchOpen(true);
        }}
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
      <GraphGenerateDialog
        open={graphOpen}
        onOpenChange={setGraphOpen}
        bookUrl={bookUrl}
        bookName={book.name}
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

      <SwitchSourceDialog
        book={book ?? null}
        open={switchOpen}
        onOpenChange={setSwitchOpen}
        chapterIndex={progress.index}
        chapterTitle={chapters[progress.index]?.title}
        inShelf={inShelf || justAdded}
        addingToShelf={addForSwitch.isPending}
        onAddToShelf={
          book === undefined
            ? undefined
            : () => {
                addForSwitch.mutate(book);
              }
        }
        onSwitched={() => toast.success("已换源, 正在重新加载")}
      />
    </ReaderFrame>
  );
}
