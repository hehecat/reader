import { useQuery, useQueryClient } from "@tanstack/react-query";
import DOMPurify from "dompurify";
import {
  ArrowLeft,
  ArrowLeftRight,
  BookOpen,
  CloudDownload,
  Download,
  Headphones,
  RefreshCw,
} from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { fontFileUrl, resolveImageUrl } from "@/api/client";
import { booksApi, featuresApi, type RemoteBookProgress } from "@/api/endpoints";
import type {
  Book,
  BookChapter,
  BookContentPayload,
  BookInfo,
  SearchBook,
} from "@/api/types";
import { BookInspectorSheet } from "@/features/book/BookInspectorSheet";
import {
  getCachedChapter,
  getCachedToc,
  setCachedChapter,
  setCachedToc,
} from "@/lib/cache";
import { buildReaderUrl, cleanChapterTitle, stripDuplicateHeading } from "@/lib/format";
import { getReaderOrigin } from "@/lib/readerOrigin";
import { useNotesStore, type ReadingNote } from "@/stores/notes";
import { usePrefsStore } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button } from "@/ui/Button";
import { EmptyView, Spinner } from "@/ui/Controls";
import { AnimatePresence, motion, SPRING_POP, useReducedMotion } from "@/ui/motion";
import {
  ChapterEndColophon,
  PagedCanvas,
  type JumpSignal,
  type PageProgressInfo,
  type ParsedChapter,
} from "./PagedCanvas";
import { NotesSheet, TocSheet } from "./ReaderDrawers";
import { ReaderHud } from "./ReaderHud";
import { ScrollCanvas } from "./ScrollCanvas";
import { SelectionInkBar } from "./SelectionInkBar";
import { TtsPlayerBar } from "./TtsPlayerBar";

/* ============================================================================
   Content Parsing & Sanitization Helpers
   ============================================================================ */

function chapterCacheKey(bookUrl: string, chapterUrl: string): string {
  return `${bookUrl}::${chapterUrl}`;
}

function decodeBasicEntities(s: string): string {
  return s
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'");
}

function splitPlainParagraphs(raw: string, chapterTitle?: string | null): string[] {
  const normalized = decodeBasicEntities(
    raw
      .replace(/\r\n/g, "\n")
      .replace(/<br\s*\/?>/gi, "\n")
      .replace(/<\/(p|div|li|h[1-6])>/gi, "\n")
      .replace(/<[^>]*>/g, ""),
  );
  const rawLines = normalized
    .split("\n")
    .map((line) => line.replace(/^[\s\u3000]+|[\s\u3000]+$/g, ""))
    .filter(Boolean);
  return stripDuplicateHeading(rawLines, chapterTitle);
}

function parseHtmlBlocks(
  rawHtml: string,
  chapterUrl: string,
  chapterTitle?: string | null,
): { paragraphs: string[]; htmlBlocks: string[] } {
  const clean = DOMPurify.sanitize(rawHtml, {
    ADD_ATTR: ["data-src", "data-original"],
  });
  const doc = new DOMParser().parseFromString(clean, "text/html");

  // Proxy external images cleanly
  for (const img of Array.from(doc.body.querySelectorAll("img"))) {
    const rawSrc =
      img.getAttribute("src") ||
      img.getAttribute("data-src") ||
      img.getAttribute("data-original") ||
      "";
    const proxied = resolveImageUrl(rawSrc, chapterUrl);
    if (proxied) {
      img.setAttribute("src", proxied);
      img.setAttribute("loading", "lazy");
      img.setAttribute("decoding", "async");
    } else {
      img.remove();
    }
  }

  const blockPairs: Array<{ text: string; html: string }> = [];
  const children = Array.from(doc.body.childNodes);

  for (const node of children) {
    if (node.nodeType === Node.TEXT_NODE) {
      const lines = (node.textContent ?? "")
        .split(/\r?\n/)
        .map((l) => l.replace(/^[\s\u3000]+|[\s\u3000]+$/g, ""))
        .filter(Boolean);
      for (const line of lines) {
        blockPairs.push({ text: line, html: line });
      }
      continue;
    }

    if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      const hasImg = el.tagName === "IMG" || Boolean(el.querySelector("img"));
      const text = (el.textContent ?? "").replace(/^[\s\u3000]+|[\s\u3000]+$/g, "");
      if (!text && !hasImg) continue;

      if (el.tagName === "IMG") {
        blockPairs.push({
          text: "",
          html: `<span class="reader-img-box">${el.outerHTML}</span>`,
        });
      } else {
        // Wrap bare img children inside reader-img-box
        if (hasImg && !el.classList.contains("reader-img-box")) {
          el.classList.add("reader-img-box");
        }
        blockPairs.push({
          text,
          html: el.innerHTML || el.outerHTML,
        });
      }
    }
  }

  if (blockPairs.length === 0) {
    const fallback = splitPlainParagraphs(rawHtml, chapterTitle);
    return { paragraphs: fallback, htmlBlocks: fallback };
  }

  // Strip duplicate heading using the exact same helper on plain text
  const rawTexts = blockPairs.map((b) => b.text);
  const strippedTexts = stripDuplicateHeading(rawTexts, chapterTitle);
  const dropCount = rawTexts.length - strippedTexts.length;
  const finalPairs = dropCount > 0 ? blockPairs.slice(dropCount) : blockPairs;

  return {
    paragraphs: finalPairs.map((b) => b.text),
    htmlBlocks: finalPairs.map((b) => b.html),
  };
}

function parseBookContent(
  payload: BookContentPayload | string | null | undefined,
  chapterUrl: string,
  chapterTitle?: string | null,
): ParsedChapter {
  if (!payload) return { kind: "empty", paragraphs: [] };

  if (typeof payload === "string") {
    const paragraphs = splitPlainParagraphs(payload, chapterTitle);
    return paragraphs.length > 0
      ? { kind: "text", paragraphs }
      : { kind: "empty", paragraphs: [] };
  }

  if ("images" in payload && Array.isArray(payload.images)) {
    const images = payload.images
      .map((u) => resolveImageUrl(u, chapterUrl))
      .filter((u): u is string => Boolean(u));
    return images.length > 0
      ? { kind: "comic", paragraphs: [], images }
      : { kind: "empty", paragraphs: [] };
  }

  if ("audioUrl" in payload && payload.audioUrl) {
    return {
      kind: "audio",
      paragraphs: [],
      audioUrl: payload.audioUrl,
      audioType: payload.contentType,
    };
  }

  if ("videoUrl" in payload && payload.videoUrl) {
    return {
      kind: "video",
      paragraphs: [],
      videoUrl: payload.videoUrl,
    };
  }

  if ("downloadUrl" in payload && payload.downloadUrl) {
    return {
      kind: "file",
      paragraphs: [],
      downloadUrl: payload.downloadUrl,
    };
  }

  if ("content" in payload && typeof payload.content === "string") {
    const raw = payload.content.trim();
    if (!raw) return { kind: "empty", paragraphs: [] };

    const isRichHtml = /<(img|figure|table|blockquote|h[1-6]|ul|ol|pre)\b/i.test(raw);
    if (isRichHtml) {
      const { paragraphs, htmlBlocks } = parseHtmlBlocks(raw, chapterUrl, chapterTitle);
      return htmlBlocks.length > 0
        ? { kind: "html", paragraphs, htmlBlocks }
        : { kind: "empty", paragraphs: [] };
    }

    const paragraphs = splitPlainParagraphs(raw, chapterTitle);
    return paragraphs.length > 0
      ? { kind: "text", paragraphs }
      : { kind: "empty", paragraphs: [] };
  }

  return { kind: "empty", paragraphs: [] };
}

function deserializeCachedPayload(raw: string): BookContentPayload | string {
  const trimmed = raw.trim();
  if (trimmed.startsWith("{")) {
    try {
      const parsed = JSON.parse(trimmed) as BookContentPayload;
      if (parsed && typeof parsed === "object") return parsed;
    } catch {
      // fall through as plain string
    }
  }
  return raw;
}

const EMPTY_PARSED_CHAPTER: ParsedChapter = { kind: "empty", paragraphs: [] };
const MAX_MEM_CHAPTERS = 16;
/** Synchronous in-memory parsed chapter LRU cache so cross-chapter page flips render with 0ms delay and zero stale-text flash */
const parsedChapterMemCache = new Map<string, ParsedChapter>();

function putParsedChapterMemCache(key: string, data: ParsedChapter) {
  if (data.kind === "empty") return;
  if (parsedChapterMemCache.has(key)) {
    parsedChapterMemCache.delete(key);
  }
  parsedChapterMemCache.set(key, data);
  while (parsedChapterMemCache.size > MAX_MEM_CHAPTERS) {
    const oldest = parsedChapterMemCache.keys().next().value;
    if (oldest === undefined) break;
    parsedChapterMemCache.delete(oldest);
  }
}

/* ============================================================================
   ReaderPage — Central Immersive Reading Hub
   ============================================================================ */

export default function ReaderPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [searchParams, setSearchParams] = useSearchParams();
  const entryIdxRef = React.useRef<number>(
    (window.history.state as { idx?: number } | null)?.idx ?? 0,
  );
  const origin = React.useMemo(() => getReaderOrigin(), []);
  const backLabel = origin?.label || "书架";

  const bookUrl = searchParams.get("url") ?? "";
  const urlBookSource = searchParams.get("bookSource") ?? "";
  const urlIndexParam = searchParams.get("index");
  const parsedUrlIndex =
    urlIndexParam !== null && urlIndexParam !== "" && Number.isFinite(Number(urlIndexParam))
      ? Math.max(0, Math.floor(Number(urlIndexParam)))
      : null;

  const readLayout = usePrefsStore((s) => s.readLayout);
  const setReadLayout = usePrefsStore((s) => s.setReadLayout);

  // Shelf & Book metadata (always refetch on mount so Device B sees Device A's latest progress)
  const { data: shelfBooks = [], isFetched: shelfFetched } = useQuery({
    queryKey: ["shelf"],
    queryFn: () => booksApi.getShelf(),
    staleTime: 30_000,
    refetchOnMount: "always",
  });

  const shelfBook = React.useMemo(
    () => shelfBooks.find((b) => b.bookUrl === bookUrl) ?? null,
    [shelfBooks, bookUrl],
  );
  const isInShelf = Boolean(shelfBook);

  const { data: remoteBookInfo } = useQuery({
    queryKey: ["book-info", bookUrl, urlBookSource],
    queryFn: ({ signal }) => booksApi.getBookInfo(bookUrl, urlBookSource || undefined, signal),
    enabled: Boolean(bookUrl) && !shelfBook,
    staleTime: 120_000,
  });

  const activeBook: Book | BookInfo | null = shelfBook ?? remoteBookInfo ?? null;
  const effectiveSource = urlBookSource || activeBook?.origin || "";
  const bookName = activeBook?.name || "";
  const bookAuthor = activeBook?.author || "";
  const tocUrl = activeBook?.tocUrl || undefined;
  const bookType = activeBook?.type;

  // Custom fonts (also ensures loaded in document.fonts)
  const { data: customFonts = [] } = useQuery({
    queryKey: ["custom-fonts"],
    queryFn: () => featuresApi.listFonts(),
    staleTime: 120_000,
  });

  React.useEffect(() => {
    if (!customFonts.length || typeof FontFace === "undefined") return;
    for (const font of customFonts) {
      const alreadyLoaded = Array.from(document.fonts).some(
        (f) => f.family === font.family || f.family === `"${font.family}"`,
      );
      if (alreadyLoaded) continue;
      const face = new FontFace(font.family, `url(${fontFileUrl(font.id)})`);
      void face
        .load()
        .then((loaded) => document.fonts.add(loaded))
        .catch(() => undefined);
    }
  }, [customFonts]);

  // Bookmarks & Notes
  const { data: bookmarks = [] } = useQuery({
    queryKey: ["bookmarks", bookUrl],
    queryFn: () => booksApi.getBookmarks(bookUrl),
    enabled: Boolean(bookUrl),
    staleTime: 30_000,
  });

  const bookNotesRaw = useNotesStore((s) => s.byBook[bookUrl]);
  const bookNotes = React.useMemo(() => bookNotesRaw ?? [], [bookNotesRaw]);

  // Core reading state
  const [chapters, setChapters] = React.useState<BookChapter[]>([]);
  const [loadingChapters, setLoadingChapters] = React.useState(false);
  const [chapterIndex, setChapterIndex] = React.useState<number>(
    () => parsedUrlIndex ?? shelfBook?.durChapterIndex ?? 0,
  );
  const [parsedState, setParsedState] = React.useState<{ key: string; data: ParsedChapter }>({
    key: "",
    data: EMPTY_PARSED_CHAPTER,
  });
  const [loadingContent, setLoadingContent] = React.useState(false);
  const [contentError, setContentError] = React.useState<string | null>(null);
  const [contentRefreshNonce, setContentRefreshNonce] = React.useState(0);
  const [jumpSignal, setJumpSignal] = React.useState<JumpSignal>(() => ({
    paraIndex: parsedUrlIndex === null ? (shelfBook?.durChapterPos ?? 0) : 0,
    align: "start",
    nonce: 0,
  }));

  // Progress & UI overlay states
  const [progressInfo, setProgressInfo] = React.useState<PageProgressInfo>({
    page: 0,
    totalPages: 1,
    chapterRatio: 0,
    paraIndex: 0,
  });
  const [hudOpen, setHudOpen] = React.useState(false);
  const [workbenchOpen, setWorkbenchOpen] = React.useState(false);
  const [shortcutsOpen, setShortcutsOpen] = React.useState(false);
  const [tocOpen, setTocOpen] = React.useState(false);
  const [notesOpen, setNotesOpen] = React.useState(false);
  const [inspectorOpen, setInspectorOpen] = React.useState(false);
  const [savingShelf, setSavingShelf] = React.useState(false);
  const [ttsActive, setTtsActive] = React.useState(false);
  const [ttsParaIndex, setTtsParaIndex] = React.useState(0);
  const [autoScrollActive, setAutoScrollActive] = React.useState(false);
  const [isFullscreen, setIsFullscreen] = React.useState(false);

  const reduced = useReducedMotion();
  const [remoteSyncBanner, setRemoteSyncBanner] = React.useState<RemoteBookProgress | null>(null);

  const stageContainerRef = React.useRef<HTMLDivElement>(null);
  const resolvedBookRef = React.useRef<string>("");
  const baseChapterTimeRef = React.useRef<number>(0);
  const syncedPosRef = React.useRef<{ chapterIndex: number; paraIndex: number } | null>(null);
  const forceNextSaveRef = React.useRef<boolean>(false);
  const lastSyncCheckAtRef = React.useRef<number>(0);
  const saveTimerRef = React.useRef<number | null>(null);
  const latestProgressRef = React.useRef({
    bookUrl,
    isInShelf,
    chapterIndex,
    paraIndex: 0,
    chapterTitle: "",
  });

  // Resolve starting chapter when bookUrl changes (honoring explicit ?index or shelf progress)
  React.useEffect(() => {
    if (!bookUrl) return;
    if (resolvedBookRef.current === bookUrl) return;
    if (parsedUrlIndex === null && !shelfFetched) return;

    resolvedBookRef.current = bookUrl;
    const initIdx = parsedUrlIndex ?? shelfBook?.durChapterIndex ?? 0;
    const initPos = parsedUrlIndex === null ? (shelfBook?.durChapterPos ?? 0) : 0;
    const remoteTime = shelfBook?.durChapterTime ?? 0;
    baseChapterTimeRef.current = remoteTime;
    syncedPosRef.current = { chapterIndex: initIdx, paraIndex: initPos };
    forceNextSaveRef.current = false;
    setRemoteSyncBanner(null);

    // If opened with an explicit stale ?index= while server already has a different newer position, prompt to sync
    if (
      parsedUrlIndex !== null &&
      shelfBook &&
      remoteTime > 0 &&
      shelfBook.durChapterIndex !== parsedUrlIndex
    ) {
      setRemoteSyncBanner({
        durChapterIndex: shelfBook.durChapterIndex,
        durChapterPos: shelfBook.durChapterPos ?? 0,
        durChapterTitle: shelfBook.durChapterTitle || "",
        durChapterTime: remoteTime,
      });
    }

    setChapterIndex(initIdx);
    setJumpSignal((prev) => ({ paraIndex: initPos, align: "start", nonce: prev.nonce + 1 }));
    setTtsActive(false);
    setAutoScrollActive(false);
  }, [bookUrl, parsedUrlIndex, shelfFetched, shelfBook]);

  // If fresh mount refetch of ["shelf"] brings newer remote progress than cached initial state, surface banner
  React.useEffect(() => {
    if (!bookUrl || resolvedBookRef.current !== bookUrl || !shelfBook) return;
    const remoteTime = shelfBook.durChapterTime ?? 0;
    if (remoteTime <= 0 || remoteTime <= baseChapterTimeRef.current) return;
    const snap = latestProgressRef.current;
    const differs =
      shelfBook.durChapterIndex !== snap.chapterIndex ||
      Math.abs((shelfBook.durChapterPos ?? 0) - snap.paraIndex) >= 2;
    if (differs) {
      setRemoteSyncBanner({
        durChapterIndex: shelfBook.durChapterIndex,
        durChapterPos: shelfBook.durChapterPos ?? 0,
        durChapterTitle: shelfBook.durChapterTitle || "",
        durChapterTime: remoteTime,
      });
    } else {
      baseChapterTimeRef.current = remoteTime;
    }
  }, [bookUrl, shelfBook]);

  // Sync ?index= query parameter cleanly on chapter switch (after initial resolution)
  React.useEffect(() => {
    if (!bookUrl || resolvedBookRef.current !== bookUrl) return;
    const currentParam = searchParams.get("index");
    if (currentParam === String(chapterIndex)) return;
    const next = new URLSearchParams(searchParams);
    next.set("index", String(chapterIndex));
    setSearchParams(next, { replace: true });
  }, [bookUrl, chapterIndex, searchParams, setSearchParams]);

  // Instant TOC loader: IndexedDB first, background network refresh + write-back
  const fetchToc = React.useCallback(
    async (forceRefresh = false, signal?: AbortSignal) => {
      if (!bookUrl) return;
      if (!forceRefresh) {
        const cached = await getCachedToc(bookUrl);
        if (cached && cached.length > 0 && !signal?.aborted) {
          setChapters(cached);
        }
      }
      setLoadingChapters(true);
      try {
        const remote = await booksApi.getChapters(
          {
            url: bookUrl,
            tocUrl,
            bookSource: effectiveSource || undefined,
            refresh: forceRefresh ? 1 : 0,
          },
          signal,
        );
        if (!signal?.aborted && Array.isArray(remote)) {
          setChapters(remote);
          void setCachedToc(bookUrl, remote);
        }
      } catch (err) {
        if (!signal?.aborted && forceRefresh) {
          notify.error(err instanceof Error ? err.message : "刷新目录失败");
        }
      } finally {
        if (!signal?.aborted) {
          setLoadingChapters(false);
        }
      }
    },
    [bookUrl, tocUrl, effectiveSource],
  );

  React.useEffect(() => {
    if (!bookUrl) return;
    const controller = new AbortController();
    void fetchToc(false, controller.signal);
    return () => controller.abort();
  }, [bookUrl, fetchToc]);

  React.useEffect(() => {
    if (chapters.length > 0 && chapterIndex >= chapters.length) {
      setChapterIndex(chapters.length - 1);
    }
  }, [chapters.length, chapterIndex]);

  const currentChapter = chapters[chapterIndex] ?? null;
  const prevChapter = chapterIndex > 0 ? (chapters[chapterIndex - 1] ?? null) : null;
  const nextChapter = chapters[chapterIndex + 1] ?? null;
  const currentChapterUrl = currentChapter?.url ?? "";
  const currentChapterTitle = currentChapter?.title ?? "";
  const currentChapterIdx = currentChapter?.index ?? chapterIndex;
  const currentChapterKey =
    bookUrl && currentChapterUrl ? chapterCacheKey(bookUrl, currentChapterUrl) : "";
  const prevChapterRef = React.useRef<BookChapter | null>(prevChapter);
  prevChapterRef.current = prevChapter;
  const nextChapterRef = React.useRef<BookChapter | null>(nextChapter);
  nextChapterRef.current = nextChapter;

  // Bind parsed content strictly to currentChapterKey so old chapter text NEVER flashes under a new chapter
  const parsed: ParsedChapter = React.useMemo(() => {
    if (!currentChapterKey) return EMPTY_PARSED_CHAPTER;
    if (parsedState.key === currentChapterKey) return parsedState.data;
    if (contentRefreshNonce === 0) {
      const mem = parsedChapterMemCache.get(currentChapterKey);
      if (mem) return mem;
    }
    return EMPTY_PARSED_CHAPTER;
  }, [currentChapterKey, parsedState, contentRefreshNonce]);

  const effectiveLoadingContent =
    loadingContent ||
    (Boolean(currentChapterKey) && parsed.kind === "empty" && !contentError);

  const displayChapterTitle =
    cleanChapterTitle(currentChapter?.title) ||
    currentChapter?.title ||
    (chapters.length > 0 ? `第 ${chapterIndex + 1} 章` : "");

  // Update browser document.title
  React.useEffect(() => {
    const prev = document.title;
    if (bookName || displayChapterTitle) {
      document.title = [displayChapterTitle, bookName, "砚台"].filter(Boolean).join(" · ");
    }
    return () => {
      document.title = prev;
    };
  }, [bookName, displayChapterTitle]);

  // Instant Chapter Content Loader + Background Silent Next-Chapter Prefetch (cache: 1)
  React.useEffect(() => {
    if (!bookUrl || !currentChapterUrl) return;
    const controller = new AbortController();
    const forceRefresh = contentRefreshNonce > 0;
    const key = chapterCacheKey(bookUrl, currentChapterUrl);

    let cancelled = false;

    const warmAdjacentFromIdb = async (ch: BookChapter | null) => {
      if (!ch || cancelled) return;
      const adjKey = chapterCacheKey(bookUrl, ch.url);
      if (parsedChapterMemCache.has(adjKey)) return;
      const raw = await getCachedChapter(adjKey);
      if (raw && !cancelled) {
        const parsedAdj = parseBookContent(deserializeCachedPayload(raw), ch.url, ch.title);
        if (parsedAdj.kind !== "empty") {
          putParsedChapterMemCache(adjKey, parsedAdj);
        }
      }
    };

    const prefetchNext = async () => {
      // Warm previous chapter from IndexedDB into sync memory cache for instant back-turn
      void warmAdjacentFromIdb(prevChapterRef.current);

      const nextCh = nextChapterRef.current;
      if (!nextCh || cancelled) return;
      const nextKey = chapterCacheKey(bookUrl, nextCh.url);
      if (parsedChapterMemCache.has(nextKey)) return;
      const alreadyCached = await getCachedChapter(nextKey);
      if (alreadyCached && !cancelled) {
        const parsedNext = parseBookContent(
          deserializeCachedPayload(alreadyCached),
          nextCh.url,
          nextCh.title,
        );
        if (parsedNext.kind !== "empty") {
          putParsedChapterMemCache(nextKey, parsedNext);
          return;
        }
      }
      if (cancelled) return;
      try {
        const nextPayload = await booksApi.getContent({
          chapterUrl: nextCh.url,
          bookUrl,
          bookSource: effectiveSource || undefined,
          index: nextCh.index,
          title: nextCh.title,
          cache: 1,
          type: bookType,
        });
        if (!cancelled && nextPayload) {
          void setCachedChapter(nextKey, JSON.stringify(nextPayload));
          const parsedNext = parseBookContent(nextPayload, nextCh.url, nextCh.title);
          if (parsedNext.kind !== "empty") {
            putParsedChapterMemCache(nextKey, parsedNext);
          }
        }
      } catch {
        // silent background prefetch
      }
    };

    void (async () => {
      setContentError(null);

      if (forceRefresh) {
        parsedChapterMemCache.delete(key);
      } else {
        const memHit = parsedChapterMemCache.get(key);
        if (memHit && !cancelled) {
          setParsedState({ key, data: memHit });
          setLoadingContent(false);
          void prefetchNext();
          return;
        }
        const cachedRaw = await getCachedChapter(key);
        if (cachedRaw && !cancelled) {
          const payload = deserializeCachedPayload(cachedRaw);
          const result = parseBookContent(payload, currentChapterUrl, currentChapterTitle);
          if (result.kind !== "empty") {
            putParsedChapterMemCache(key, result);
            setParsedState({ key, data: result });
            setLoadingContent(false);
            void prefetchNext();
            return;
          }
        }
      }

      setLoadingContent(true);
      try {
        const payload = await booksApi.getContent(
          {
            chapterUrl: currentChapterUrl,
            bookUrl,
            bookSource: effectiveSource || undefined,
            index: currentChapterIdx,
            title: currentChapterTitle,
            refresh: forceRefresh ? 1 : undefined,
            type: bookType,
          },
          controller.signal,
        );
        if (cancelled) return;
        const result = parseBookContent(payload, currentChapterUrl, currentChapterTitle);
        if (result.kind !== "empty") {
          putParsedChapterMemCache(key, result);
          void setCachedChapter(key, JSON.stringify(payload));
        }
        setParsedState({ key, data: result });
        if (forceRefresh) {
          setContentRefreshNonce(0);
          notify.success("已重新拉取本章正文");
        }
        void prefetchNext();
      } catch (err) {
        if (cancelled) return;
        setContentError(err instanceof Error ? err.message : "章节正文加载失败");
        if (forceRefresh) setContentRefreshNonce(0);
      } finally {
        if (!cancelled) setLoadingContent(false);
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [
    bookUrl,
    currentChapterUrl,
    currentChapterTitle,
    currentChapterIdx,
    effectiveSource,
    bookType,
    contentRefreshNonce,
  ]);

  // Keep latest progress ref updated for throttled / flush saves
  latestProgressRef.current = {
    bookUrl,
    isInShelf,
    chapterIndex,
    paraIndex: progressInfo.paraIndex,
    chapterTitle: displayChapterTitle,
  };

  const flushProgressSave = React.useCallback(
    (opts?: { keepalive?: boolean; force?: boolean }) => {
      const snap = latestProgressRef.current;
      if (!snap.bookUrl || !snap.isInShelf) return;
      const force = Boolean(opts?.force || forceNextSaveRef.current);
      const synced = syncedPosRef.current;
      const isDirty =
        !synced ||
        synced.chapterIndex !== snap.chapterIndex ||
        synced.paraIndex !== snap.paraIndex;
      // Never write back if the user has not actually moved on this device (prevents background Device A clobbering Device B)
      if (!isDirty && !force) return;

      const sentChapter = snap.chapterIndex;
      const sentPara = snap.paraIndex;
      const sentTitle = snap.chapterTitle;

      void booksApi
        .saveProgress(
          {
            bookUrl: snap.bookUrl,
            durChapterIndex: sentChapter,
            durChapterPos: sentPara,
            durChapterTitle: sentTitle || undefined,
            baseChapterTime: baseChapterTimeRef.current,
            force: force || undefined,
          },
          { keepalive: opts?.keepalive },
        )
        .then((res) => {
          if (res?.conflict && res.remote) {
            setRemoteSyncBanner(res.remote);
            return;
          }
          const serverTime = res?.durChapterTime ?? Date.now();
          baseChapterTimeRef.current = serverTime;
          syncedPosRef.current = { chapterIndex: sentChapter, paraIndex: sentPara };
          forceNextSaveRef.current = false;
          queryClient.setQueryData<Book[]>(["shelf"], (old) =>
            old?.map((b) =>
              b.bookUrl === snap.bookUrl
                ? {
                    ...b,
                    durChapterIndex: sentChapter,
                    durChapterPos: sentPara,
                    durChapterTime: serverTime,
                    durChapterTitle: sentTitle || b.durChapterTitle,
                  }
                : b,
            ),
          );
        })
        .catch(() => undefined);
    },
    [queryClient],
  );

  // Throttled progress save on genuine chapter / paragraph movement
  React.useEffect(() => {
    if (!isInShelf || !bookUrl || resolvedBookRef.current !== bookUrl) return;
    const synced = syncedPosRef.current;
    const isDirty =
      !synced ||
      synced.chapterIndex !== chapterIndex ||
      synced.paraIndex !== progressInfo.paraIndex;
    if (!isDirty) return;

    if (saveTimerRef.current !== null) window.clearTimeout(saveTimerRef.current);
    saveTimerRef.current = window.setTimeout(() => {
      saveTimerRef.current = null;
      flushProgressSave();
    }, 1600);
    return () => {
      if (saveTimerRef.current !== null) {
        window.clearTimeout(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
  }, [isInShelf, bookUrl, chapterIndex, progressInfo.paraIndex, flushProgressSave]);

  // Flush on unmount & detect cross-device progress when returning to foreground
  const jumpToPosition = React.useCallback(
    (targetChapter: number, targetPara = 0, align: "start" | "end" = "start") => {
      const clamped = Math.min(Math.max(0, chapters.length - 1), Math.max(0, targetChapter));
      setChapterIndex(clamped);
      setJumpSignal((prev) => ({
        paraIndex: Math.max(0, targetPara),
        align,
        nonce: prev.nonce + 1,
      }));
      setTtsParaIndex(Math.max(0, targetPara));
    },
    [chapters.length],
  );

  const handleAcceptRemoteSync = React.useCallback(() => {
    if (!remoteSyncBanner) return;
    const target = remoteSyncBanner;
    baseChapterTimeRef.current = target.durChapterTime;
    syncedPosRef.current = {
      chapterIndex: target.durChapterIndex,
      paraIndex: target.durChapterPos,
    };
    forceNextSaveRef.current = false;
    setRemoteSyncBanner(null);
    jumpToPosition(target.durChapterIndex, target.durChapterPos, "start");
    notify.success(`已同步至第 ${target.durChapterIndex + 1} 章`);
  }, [remoteSyncBanner, jumpToPosition]);

  const handleKeepLocalPosition = React.useCallback(() => {
    forceNextSaveRef.current = true;
    setRemoteSyncBanner(null);
    flushProgressSave({ force: true });
  }, [flushProgressSave]);

  React.useEffect(() => {
    const checkRemoteSync = async () => {
      const snap = latestProgressRef.current;
      if (!snap.bookUrl || !snap.isInShelf) return;
      const now = Date.now();
      if (now - lastSyncCheckAtRef.current < 4_000) return;
      lastSyncCheckAtRef.current = now;
      try {
        const list = await booksApi.getShelf();
        queryClient.setQueryData<Book[]>(["shelf"], list);
        const remote = list.find((b) => b.bookUrl === snap.bookUrl);
        if (!remote) return;
        const remoteTime = remote.durChapterTime ?? 0;
        const isNewerThanBase = remoteTime > baseChapterTimeRef.current;
        const differsInPosition =
          remote.durChapterIndex !== snap.chapterIndex ||
          Math.abs((remote.durChapterPos ?? 0) - snap.paraIndex) >= 2;
        if (isNewerThanBase && differsInPosition) {
          setRemoteSyncBanner({
            durChapterIndex: remote.durChapterIndex,
            durChapterPos: remote.durChapterPos ?? 0,
            durChapterTitle: remote.durChapterTitle || "",
            durChapterTime: remoteTime,
          });
        } else if (isNewerThanBase) {
          baseChapterTimeRef.current = remoteTime;
        }
      } catch {
        // ignore network hiccup
      }
    };

    const onVisibility = () => {
      if (document.visibilityState === "hidden") {
        flushProgressSave({ keepalive: true });
      } else if (document.visibilityState === "visible") {
        void checkRemoteSync();
      }
    };

    const onWindowFocus = () => {
      void checkRemoteSync();
    };

    const onPageHide = () => {
      flushProgressSave({ keepalive: true });
    };

    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("focus", onWindowFocus);
    window.addEventListener("pagehide", onPageHide);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", onWindowFocus);
      window.removeEventListener("pagehide", onPageHide);
      flushProgressSave({ keepalive: true });
    };
  }, [flushProgressSave, queryClient]);

  // Fullscreen state listener
  React.useEffect(() => {
    const onFsChange = () => setIsFullscreen(Boolean(document.fullscreenElement));
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, []);

  // Notes grouped by paragraph for current chapter
  const notesByPara = React.useMemo(() => {
    const map = new Map<number, ReadingNote[]>();
    for (const n of bookNotes) {
      if (n.chapterIndex !== chapterIndex) continue;
      const arr = map.get(n.paragraphIndex);
      if (arr) arr.push(n);
      else map.set(n.paragraphIndex, [n]);
    }
    return map;
  }, [bookNotes, chapterIndex]);

  // Handlers
  const handleToggleHud = React.useCallback(() => {
    if (workbenchOpen) {
      setWorkbenchOpen(false);
      return;
    }
    setHudOpen((o) => !o);
  }, [workbenchOpen]);

  const handlePrevChapter = React.useCallback(
    (align: "start" | "end" = "start") => {
      if (chapterIndex <= 0) return;
      flushProgressSave();
      jumpToPosition(chapterIndex - 1, 0, align);
    },
    [chapterIndex, flushProgressSave, jumpToPosition],
  );

  const handleNextChapter = React.useCallback(() => {
    if (chapterIndex >= chapters.length - 1) return;
    flushProgressSave();
    jumpToPosition(chapterIndex + 1, 0, "start");
  }, [chapterIndex, chapters.length, flushProgressSave, jumpToPosition]);

  const handleAddToShelf = async () => {
    if (!bookUrl || savingShelf) return;
    setSavingShelf(true);
    try {
      await booksApi.saveBook({
        bookUrl,
        tocUrl: tocUrl || bookUrl,
        origin: effectiveSource,
        originName: activeBook?.originName || "",
        name: bookName || "未命名书籍",
        author: bookAuthor || "",
        coverUrl: activeBook?.coverUrl ?? undefined,
        intro: activeBook?.intro ?? undefined,
        kind: activeBook?.kind ?? undefined,
        type: bookType ?? 0,
        isInShelf: true,
        durChapterIndex: chapterIndex,
        durChapterPos: progressInfo.paraIndex,
        durChapterTitle: displayChapterTitle || undefined,
        durChapterTime: Date.now(),
      });
      await queryClient.invalidateQueries({ queryKey: ["shelf"] });
      notify.success("已收入书房");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "收入书房失败");
    } finally {
      setSavingShelf(false);
    }
  };

  const handleQuickBookmark = async () => {
    if (!bookUrl) return;
    try {
      const now = new Date();
      const timeStr = `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
      const excerpt = parsed.paragraphs[progressInfo.paraIndex]?.slice(0, 120) ?? "";
      await booksApi.saveBookmark({
        bookUrl,
        title: `${displayChapterTitle || `第 ${chapterIndex + 1} 章`} · ${timeStr}`,
        bookName: bookName || "",
        bookAuthor: bookAuthor || "",
        paragraphIndex: progressInfo.paraIndex,
        chapterIndex,
        chapterName: displayChapterTitle || `第 ${chapterIndex + 1} 章`,
        bookText: excerpt,
        content: "",
        createdAt: Date.now(),
      });
      await queryClient.invalidateQueries({ queryKey: ["bookmarks", bookUrl] });
      notify.success("已添加书签");
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "添加书签失败");
    }
  };

  const handleToggleTts = () => {
    if (ttsActive) {
      setTtsActive(false);
      return;
    }
    if (parsed.paragraphs.length === 0) {
      notify.info("当前章节无可朗读正文");
      return;
    }
    setTtsParaIndex(Math.min(progressInfo.paraIndex, Math.max(0, parsed.paragraphs.length - 1)));
    setTtsActive(true);
    setWorkbenchOpen(false);
  };

  const handleToggleAutoScroll = () => {
    if (autoScrollActive) {
      setAutoScrollActive(false);
      return;
    }
    if (readLayout !== "scroll") {
      setReadLayout("scroll");
      notify.info("已切换至长卷模式并开启自动滚屏");
    }
    setHudOpen(false);
    setWorkbenchOpen(false);
    setAutoScrollActive(true);
  };

  const handleToggleFullscreen = async () => {
    try {
      if (!document.fullscreenElement) {
        await document.documentElement.requestFullscreen();
      } else {
        await document.exitFullscreen();
      }
    } catch {
      notify.info("当前浏览器不支持全屏切换");
    }
  };

  const handleBack = React.useCallback(() => {
    flushProgressSave();
    const currentIdx = (window.history.state as { idx?: number } | null)?.idx ?? 0;
    const entryIdx = entryIdxRef.current;
    if (entryIdx > 0 && origin) {
      const delta = Math.max(1, currentIdx - entryIdx + 1);
      navigate(-delta);
    } else {
      navigate(origin?.path || "/", { replace: true });
    }
  }, [flushProgressSave, navigate, origin]);

  const handleSourceSwitched = (updated: Book | SearchBook) => {
    setInspectorOpen(false);
    void queryClient.invalidateQueries({ queryKey: ["shelf"] });
    const nextUrl = buildReaderUrl(updated.bookUrl, chapterIndex, updated.origin);
    navigate(nextUrl, { replace: true });
    setContentRefreshNonce((n) => n + 1);
    notify.success(`已切换书源：${updated.originName || updated.origin}`);
  };

  const inspectorBook: (Partial<Book> & { bookUrl: string; name?: string }) | null =
    React.useMemo(() => {
      if (!bookUrl) return null;
      if (shelfBook) return shelfBook;
      if (remoteBookInfo) {
        return {
          ...remoteBookInfo,
          tocUrl: remoteBookInfo.tocUrl ?? undefined,
        };
      }
      return {
        bookUrl,
        name: bookName || "未命名书籍",
        author: bookAuthor || "",
        origin: effectiveSource,
        originName: "",
        type: bookType ?? 0,
      };
    }, [shelfBook, remoteBookInfo, bookUrl, bookName, bookAuthor, effectiveSource, bookType]);

  const isCurrentBookmarked = React.useMemo(
    () => bookmarks.some((b) => b.chapterIndex === chapterIndex),
    [bookmarks, chapterIndex],
  );

  const anyOverlayOpen = tocOpen || notesOpen || inspectorOpen || shortcutsOpen;

  // Complete reader keyboard shortcuts (matches SHORTCUT_ITEMS in ReaderHud)
  const shortcutActionsRef = React.useRef({
    handlePrevChapter,
    handleNextChapter,
    handleQuickBookmark,
    handleToggleTts,
    handleToggleFullscreen,
  });
  shortcutActionsRef.current = {
    handlePrevChapter,
    handleNextChapter,
    handleQuickBookmark,
    handleToggleTts,
    handleToggleFullscreen,
  };

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const inInput = Boolean(
        target &&
          (target.tagName === "INPUT" ||
            target.tagName === "TEXTAREA" ||
            target.isContentEditable),
      );
      if (e.key === "Escape") {
        if (shortcutsOpen) {
          e.preventDefault();
          setShortcutsOpen(false);
        } else if (!tocOpen && !notesOpen && !inspectorOpen) {
          if (workbenchOpen) setWorkbenchOpen(false);
          else if (hudOpen) setHudOpen(false);
        }
        return;
      }
      if (inInput || e.metaKey || e.ctrlKey || e.altKey) return;

      if (e.key === "?" || (e.key === "/" && e.shiftKey)) {
        e.preventDefault();
        setWorkbenchOpen(false);
        setShortcutsOpen((v) => !v);
        return;
      }
      if (tocOpen || notesOpen || inspectorOpen || shortcutsOpen) return;

      const lower = e.key.toLowerCase();
      if (e.key === "[") {
        e.preventDefault();
        shortcutActionsRef.current.handlePrevChapter("start");
      } else if (e.key === "]") {
        e.preventDefault();
        shortcutActionsRef.current.handleNextChapter();
      } else if (lower === "c" || lower === "m") {
        e.preventDefault();
        setWorkbenchOpen(false);
        setTocOpen(true);
      } else if (lower === "n") {
        e.preventDefault();
        setWorkbenchOpen(false);
        setNotesOpen(true);
      } else if (lower === "b") {
        e.preventDefault();
        void shortcutActionsRef.current.handleQuickBookmark();
      } else if (lower === "a") {
        e.preventDefault();
        setHudOpen(true);
        setWorkbenchOpen((w) => !w);
      } else if (lower === "t") {
        e.preventDefault();
        shortcutActionsRef.current.handleToggleTts();
      } else if (lower === "f") {
        e.preventDefault();
        void shortcutActionsRef.current.handleToggleFullscreen();
      } else if (lower === "h") {
        e.preventDefault();
        setWorkbenchOpen(false);
        setHudOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hudOpen, workbenchOpen, tocOpen, notesOpen, inspectorOpen, shortcutsOpen]);
  const isTextLike = parsed.kind === "text" || parsed.kind === "html";
  const currentExcerpt = parsed.paragraphs[progressInfo.paraIndex] ?? parsed.paragraphs[0] ?? "";

  if (!bookUrl) {
    return (
      <div className="fixed inset-0 flex items-center justify-center bg-bg px-6">
        <EmptyView
          icon={<BookOpen className="size-5" />}
          title="未指定阅读书籍"
          description="请从私人书房或寻书结果中选择一部作品开始阅读。"
          action={
            <Button variant="primary" onClick={handleBack}>
              <ArrowLeft className="size-4" />
              <span>返回{backLabel}</span>
            </Button>
          }
        />
      </div>
    );
  }

  return (
    <div
      ref={stageContainerRef}
      className="fixed inset-0 overflow-hidden bg-bg text-ink select-text"
    >
      {/* ================================================================
          Zero-Reflow Constant Stage Content (deltaY = 0 always)
         ================================================================ */}
      {effectiveLoadingContent && parsed.kind === "empty" ? (
        <div
          onClick={handleToggleHud}
          className="flex h-full w-full flex-col items-center justify-center gap-3 px-6"
        >
          <Spinner label={`正在展开《${bookName || "书卷"}》…`} />
          {displayChapterTitle ? (
            <p className="font-serif text-xs text-ink-muted">{displayChapterTitle}</p>
          ) : null}
        </div>
      ) : contentError ? (
        <div
          onClick={handleToggleHud}
          className="flex h-full w-full items-center justify-center px-6"
        >
          <EmptyView
            icon={<BookOpen className="size-5" />}
            title="章节正文加载受阻"
            description={contentError}
            action={
              <>
                <Button
                  variant="primary"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    setContentRefreshNonce((n) => n + 1);
                  }}
                >
                  <RefreshCw className="size-3.5" />
                  <span>重试拉取</span>
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    setInspectorOpen(true);
                  }}
                >
                  <ArrowLeftRight className="size-3.5" />
                  <span>切换书源</span>
                </Button>
              </>
            }
          />
        </div>
      ) : parsed.kind === "audio" && parsed.audioUrl ? (
        <div
          onClick={handleToggleHud}
          className="flex h-full w-full flex-col items-center justify-center overflow-y-auto px-6 py-20"
        >
          <div
            onClick={(e) => e.stopPropagation()}
            className="surface-glass flex w-full max-w-md flex-col items-center rounded-3xl p-6 text-center"
          >
            <BookCover
              src={activeBook?.coverUrl}
              referer={effectiveSource}
              title={bookName || "有声书"}
              author={bookAuthor}
              className="w-36"
            />
            <div className="mt-5 flex items-center gap-1.5 text-xs text-accent">
              <Headphones className="size-3.5" />
              <span>有声书播放</span>
            </div>
            <h1 className="mt-1.5 font-serif text-lg font-semibold text-ink">
              {displayChapterTitle || bookName}
            </h1>
            {bookAuthor ? (
              <p className="mt-0.5 text-xs text-ink-muted">{bookAuthor}</p>
            ) : null}
            <audio
              key={parsed.audioUrl}
              controls
              autoPlay
              onEnded={() => {
                if (chapterIndex < chapters.length - 1) handleNextChapter();
              }}
              className="mt-6 w-full"
            >
              <source src={parsed.audioUrl} type={parsed.audioType || "audio/mpeg"} />
            </audio>
            <ChapterEndColophon
              compact
              hasPrev={chapterIndex > 0}
              hasNext={chapterIndex < chapters.length - 1}
              nextChapterTitle={nextChapter?.title}
              onPrevChapter={() => handlePrevChapter("start")}
              onNextChapter={handleNextChapter}
            />
          </div>
        </div>
      ) : parsed.kind === "video" && parsed.videoUrl ? (
        <div
          onClick={handleToggleHud}
          className="flex h-full w-full flex-col items-center justify-center px-6 py-20"
        >
          <div onClick={(e) => e.stopPropagation()} className="w-full max-w-3xl">
            <video
              key={parsed.videoUrl}
              src={parsed.videoUrl}
              controls
              className="max-h-[68vh] w-full rounded-2xl bg-black shadow-lg"
            />
            <ChapterEndColophon
              compact
              hasPrev={chapterIndex > 0}
              hasNext={chapterIndex < chapters.length - 1}
              nextChapterTitle={nextChapter?.title}
              onPrevChapter={() => handlePrevChapter("start")}
              onNextChapter={handleNextChapter}
            />
          </div>
        </div>
      ) : parsed.kind === "file" && parsed.downloadUrl ? (
        <div
          onClick={handleToggleHud}
          className="flex h-full w-full items-center justify-center px-6"
        >
          <EmptyView
            icon={<Download className="size-5" />}
            title={displayChapterTitle || "附件章节"}
            description="该章节为文件下载资源。"
            action={
              <a
                href={parsed.downloadUrl}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => e.stopPropagation()}
                className="inline-flex h-9 items-center gap-2 rounded-full bg-accent px-4 text-xs font-medium text-accent-ink hover:brightness-110"
              >
                <Download className="size-3.5" />
                <span>下载文件</span>
              </a>
            }
          />
        </div>
      ) : parsed.kind === "comic" || readLayout === "scroll" ? (
        <ScrollCanvas
          chapterTitle={displayChapterTitle}
          chapterIndex={chapterIndex}
          totalChapters={chapters.length}
          parsed={parsed}
          notesByPara={notesByPara}
          jumpSignal={jumpSignal}
          hasPrevChapter={chapterIndex > 0}
          hasNextChapter={chapterIndex < chapters.length - 1}
          nextChapterTitle={nextChapter?.title}
          ttsActive={ttsActive}
          ttsParaIndex={ttsActive ? ttsParaIndex : null}
          gesturesDisabled={anyOverlayOpen}
          autoScrollActive={autoScrollActive}
          onStopAutoScroll={() => setAutoScrollActive(false)}
          onProgressChange={setProgressInfo}
          onToggleHud={handleToggleHud}
          onPrevChapter={handlePrevChapter}
          onNextChapter={handleNextChapter}
          onParagraphClickForTts={(idx) => setTtsParaIndex(idx)}
        />
      ) : (
        <PagedCanvas
          chapterTitle={displayChapterTitle}
          chapterIndex={chapterIndex}
          totalChapters={chapters.length}
          parsed={parsed}
          notesByPara={notesByPara}
          jumpSignal={jumpSignal}
          hasPrevChapter={chapterIndex > 0}
          hasNextChapter={chapterIndex < chapters.length - 1}
          nextChapterTitle={nextChapter?.title}
          ttsActive={ttsActive}
          ttsParaIndex={ttsActive ? ttsParaIndex : null}
          gesturesDisabled={anyOverlayOpen}
          panelOpen={hudOpen || workbenchOpen}
          onProgressChange={setProgressInfo}
          onToggleHud={handleToggleHud}
          onPrevChapter={handlePrevChapter}
          onNextChapter={handleNextChapter}
          onParagraphClickForTts={(idx) => setTtsParaIndex(idx)}
        />
      )}

      {/* Non-blocking Cross-Device Progress Sync Pill Banner */}
      <AnimatePresence>
        {remoteSyncBanner ? (
          <motion.div
            key="remote-sync-banner"
            role="status"
            aria-live="polite"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: -14, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: -12, scale: 0.98 }}
            transition={reduced ? { duration: 0.12 } : SPRING_POP}
            onClick={(e) => e.stopPropagation()}
            className="pointer-events-auto fixed inset-x-3 top-3 z-35 mx-auto flex max-w-lg items-center gap-2.5 rounded-2xl border border-line/85 bg-bg/95 px-3.5 py-2.5 shadow-[var(--shadow-lg)] backdrop-blur-xl safe-t sm:top-4 sm:rounded-full sm:px-4"
          >
            <span className="flex size-7 shrink-0 items-center justify-center rounded-full bg-accent-soft text-accent">
              <CloudDownload className="size-3.5" />
            </span>
            <div className="min-w-0 flex-1">
              <p className="truncate text-xs font-medium text-ink">
                其他设备已读至第 {remoteSyncBanner.durChapterIndex + 1} 章
              </p>
              {remoteSyncBanner.durChapterTitle ? (
                <p className="truncate text-[11px] text-ink-muted">
                  {cleanChapterTitle(remoteSyncBanner.durChapterTitle) ||
                    remoteSyncBanner.durChapterTitle}
                </p>
              ) : null}
            </div>
            <div className="flex shrink-0 items-center gap-1.5">
              <Button size="xs" variant="ghost" onClick={handleKeepLocalPosition}>
                留在当前
              </Button>
              <Button size="xs" variant="primary" onClick={handleAcceptRemoteSync}>
                同步跳转
              </Button>
            </div>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* Subtle inline spinner overlay when refreshing an already visible chapter */}
      {loadingContent && parsed.kind !== "empty" ? (
        <div className="surface-glass pointer-events-none fixed top-4 right-4 z-25 rounded-full px-3 py-1.5 safe-t">
          <Spinner label="加载中…" />
        </div>
      ) : null}

      {/* ================================================================
          Zero-Reflow Floating Overlays (HUD, Ink Bar, TTS, Drawers)
         ================================================================ */}
      <ReaderHud
        hudOpen={hudOpen}
        workbenchOpen={workbenchOpen}
        onToggleWorkbench={() => setWorkbenchOpen((w) => !w)}
        onCloseWorkbench={() => setWorkbenchOpen(false)}
        onBack={handleBack}
        backLabel={backLabel}
        bookName={bookName}
        chapterTitle={displayChapterTitle}
        chapterIndex={chapterIndex}
        chapters={chapters}
        onJumpChapter={(idx) => {
          flushProgressSave();
          jumpToPosition(idx, 0, "start");
        }}
        onPrevChapter={() => handlePrevChapter("start")}
        onNextChapter={handleNextChapter}
        page={progressInfo.page}
        totalPages={progressInfo.totalPages}
        chapterRatio={progressInfo.chapterRatio}
        isInShelf={isInShelf}
        savingShelf={savingShelf}
        onToggleShelf={() => void handleAddToShelf()}
        isCurrentBookmarked={isCurrentBookmarked}
        onQuickBookmark={() => void handleQuickBookmark()}
        notesCount={bookmarks.length + bookNotes.length}
        ttsActive={ttsActive}
        autoScrollActive={autoScrollActive}
        isFullscreen={isFullscreen}
        customFonts={customFonts}
        shortcutsOpen={shortcutsOpen}
        onOpenShortcuts={() => {
          setWorkbenchOpen(false);
          setShortcutsOpen(true);
        }}
        onCloseShortcuts={() => setShortcutsOpen(false)}
        onOpenToc={() => {
          setWorkbenchOpen(false);
          setTocOpen(true);
        }}
        onOpenNotes={() => {
          setWorkbenchOpen(false);
          setNotesOpen(true);
        }}
        onToggleTts={handleToggleTts}
        onOpenInspector={() => {
          setWorkbenchOpen(false);
          setInspectorOpen(true);
        }}
        onRefreshChapter={() => setContentRefreshNonce((n) => n + 1)}
        onToggleAutoScroll={handleToggleAutoScroll}
        onToggleFullscreen={() => void handleToggleFullscreen()}
      />

      {isTextLike ? (
        <SelectionInkBar
          containerRef={stageContainerRef}
          bookUrl={bookUrl}
          chapterIndex={chapterIndex}
          chapterTitle={displayChapterTitle}
          disabled={anyOverlayOpen}
          onRequestRefreshChapter={() => setContentRefreshNonce((n) => n + 1)}
        />
      ) : null}

      <TtsPlayerBar
        open={ttsActive}
        onClose={() => setTtsActive(false)}
        paragraphs={parsed.paragraphs}
        chapterIndex={chapterIndex}
        chapterTitle={displayChapterTitle}
        activeParaIndex={ttsParaIndex}
        onActiveParaChange={setTtsParaIndex}
        hasNextChapter={chapterIndex < chapters.length - 1}
        onRequestNextChapter={handleNextChapter}
        hudOpen={hudOpen}
        workbenchOpen={workbenchOpen}
      />

      <TocSheet
        open={tocOpen}
        onClose={() => setTocOpen(false)}
        bookUrl={bookUrl}
        bookName={bookName}
        isInShelf={isInShelf}
        chapters={chapters}
        currentChapterIndex={chapterIndex}
        loadingChapters={loadingChapters}
        onSelectChapter={(idx) => {
          flushProgressSave();
          jumpToPosition(idx, 0, "start");
        }}
        onRefreshToc={() => void fetchToc(true)}
        onAddToShelf={() => void handleAddToShelf()}
      />

      <NotesSheet
        open={notesOpen}
        onClose={() => setNotesOpen(false)}
        bookUrl={bookUrl}
        bookName={bookName}
        bookAuthor={bookAuthor}
        currentChapterIndex={chapterIndex}
        currentChapterTitle={displayChapterTitle}
        currentParaIndex={progressInfo.paraIndex}
        currentExcerpt={currentExcerpt}
        bookmarks={bookmarks}
        onJumpTo={(chIdx, paraIdx) => {
          flushProgressSave();
          jumpToPosition(chIdx, paraIdx, "start");
        }}
      />

      {inspectorBook ? (
        <BookInspectorSheet
          open={inspectorOpen}
          onClose={() => setInspectorOpen(false)}
          book={inspectorBook}
          initialTab="sources"
          onSourceSwitched={handleSourceSwitched}
        />
      ) : null}
    </div>
  );
}
