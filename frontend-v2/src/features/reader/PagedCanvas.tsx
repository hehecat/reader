import { ChevronLeft, ChevronRight } from "lucide-react";
import * as React from "react";
import { cn } from "@/lib/cn";
import type { InkColor, ReadingNote } from "@/stores/notes";
import { usePrefsStore, type ReaderFont } from "@/stores/prefs";
import { Button } from "@/ui/Button";
import {
  computeCurlGeometry,
  drawCurlLighting,
  pointerToCurlTip,
  progressToArcTip,
  readThemeCurlPalette,
  type CurlMode,
  type CurlPalette,
  type Point,
} from "./pageCurl";

/* ============================================================================
   Shared Reader Types & Rendering Helpers
   ============================================================================ */

export interface ParsedChapter {
  kind: "text" | "html" | "comic" | "audio" | "video" | "file" | "empty";
  paragraphs: string[];
  htmlBlocks?: string[];
  images?: string[];
  audioUrl?: string;
  audioType?: string;
  videoUrl?: string;
  downloadUrl?: string;
}

export interface JumpSignal {
  paraIndex: number;
  align?: "start" | "end";
  nonce: number;
}

export interface PageProgressInfo {
  page: number;
  totalPages: number;
  chapterRatio: number;
  paraIndex: number;
}

export function resolveReaderFontFamily(
  fontFamily: ReaderFont,
  customFontFamily: string,
): string {
  if (fontFamily === "kai") return "var(--font-kai)";
  if (fontFamily === "sans") return "var(--font-sans)";
  if (fontFamily === "custom" && customFontFamily) {
    return `"${customFontFamily}", var(--font-serif)`;
  }
  return "var(--font-serif)";
}

const HIGHLIGHT_MARK_CLASSES: Record<InkColor, string> = {
  amber:
    "rounded-[3px] bg-amber-400/28 px-0.5 text-inherit underline decoration-amber-600/45 decoration-1 underline-offset-4 dark:bg-amber-400/22 dark:decoration-amber-300/45",
  sage:
    "rounded-[3px] bg-emerald-500/22 px-0.5 text-inherit underline decoration-emerald-600/45 decoration-1 underline-offset-4 dark:bg-emerald-400/20 dark:decoration-emerald-300/45",
  vermilion:
    "rounded-[3px] bg-rose-500/20 px-0.5 text-inherit underline decoration-rose-600/45 decoration-1 underline-offset-4 dark:bg-rose-400/22 dark:decoration-rose-300/45",
};

export function renderHighlightedText(
  text: string,
  notes: ReadingNote[] | undefined,
): React.ReactNode {
  if (!notes || notes.length === 0) return text;

  const ranges: Array<{ start: number; end: number; color: InkColor; note?: string }> = [];
  for (const n of notes) {
    const q = n.quote.trim();
    if (!q) continue;
    const idx = text.indexOf(q);
    if (idx !== -1) {
      ranges.push({ start: idx, end: idx + q.length, color: n.color, note: n.note });
    }
  }
  if (ranges.length === 0) return text;

  ranges.sort((a, b) => a.start - b.start || b.end - a.end);

  const nodes: React.ReactNode[] = [];
  let cursor = 0;
  for (let i = 0; i < ranges.length; i += 1) {
    const r = ranges[i]!;
    if (r.start < cursor) continue;
    if (r.start > cursor) {
      nodes.push(text.slice(cursor, r.start));
    }
    nodes.push(
      <mark
        key={`${r.start}-${r.end}-${i}`}
        title={r.note || undefined}
        className={HIGHLIGHT_MARK_CLASSES[r.color]}
      >
        {text.slice(r.start, r.end)}
      </mark>,
    );
    cursor = r.end;
  }
  if (cursor < text.length) {
    nodes.push(text.slice(cursor));
  }
  return nodes;
}

export function ChapterEndColophon({
  hasPrev,
  hasNext,
  nextChapterTitle,
  onPrevChapter,
  onNextChapter,
  compact = false,
}: {
  hasPrev: boolean;
  hasNext: boolean;
  nextChapterTitle?: string | null;
  onPrevChapter: () => void;
  onNextChapter: () => void;
  compact?: boolean;
}) {
  return (
    <div
      onClick={(e) => e.stopPropagation()}
      style={{ breakInside: "avoid" }}
      className={cn(
        "flex flex-col items-center text-center select-none",
        compact ? "mt-8 pt-6 pb-4" : "mt-12 pt-10 pb-8",
      )}
    >
      <div className="flex w-full max-w-[260px] items-center gap-3">
        <span aria-hidden className="h-px flex-1 bg-line-strong/80" />
        <span className="inline-flex items-center justify-center rounded-[4px] border border-accent/60 bg-accent-soft/45 px-2.5 py-0.5 font-serif text-[11px] font-medium tracking-[0.26em] text-accent">
          本章完
        </span>
        <span aria-hidden className="h-px flex-1 bg-line-strong/80" />
      </div>

      {hasNext && nextChapterTitle ? (
        <p className="mt-3 max-w-[300px] truncate font-serif text-xs text-ink-muted">
          下回 · {nextChapterTitle}
        </p>
      ) : !hasNext ? (
        <p className="mt-3 font-serif text-xs text-ink-muted">已读至全书最新章节</p>
      ) : null}

      <div className="mt-4 flex items-center gap-2.5">
        <Button
          size="sm"
          variant="secondary"
          disabled={!hasPrev}
          onClick={onPrevChapter}
        >
          <ChevronLeft className="size-3.5" />
          <span>上一章</span>
        </Button>
        <Button
          size="sm"
          variant="primary"
          disabled={!hasNext}
          onClick={onNextChapter}
        >
          <span>下一章</span>
          <ChevronRight className="size-3.5" />
        </Button>
      </div>
    </div>
  );
}

/* ============================================================================
   PagedCanvas — Realistic Drag-to-Peel Physical Book Curl + Open-Book Spread
   ============================================================================ */

export interface PagedCanvasProps {
  chapterTitle: string;
  chapterIndex: number;
  totalChapters: number;
  parsed: ParsedChapter;
  notesByPara: Map<number, ReadingNote[]>;
  jumpSignal: JumpSignal;
  hasPrevChapter: boolean;
  hasNextChapter: boolean;
  nextChapterTitle?: string | null;
  ttsActive: boolean;
  ttsParaIndex: number | null;
  ttsFollowNonce?: number;
  gesturesDisabled: boolean;
  /** True when HUD or Aa typography workbench is open; blank clicks fold panel first instead of misfiring page turns */
  panelOpen?: boolean;
  onProgressChange: (info: PageProgressInfo) => void;
  onToggleHud: () => void;
  onPrevChapter: (align?: "start" | "end") => void;
  onNextChapter: () => void;
  onParagraphClickForTts?: (paraIndex: number) => void;
}

const SPREAD_BREAKPOINT = 1060;
const SPREAD_GUTTER = 64;
const SINGLE_GAP = 48;

type BoundaryKind = "within" | "next-chapter" | "prev-chapter" | "hard-end" | "hard-start";

interface ActiveCurlSession {
  mode: CurlMode;
  dir: "next" | "prev";
  cornerY: "top" | "bottom";
  targetPage: number;
  boundary: BoundaryKind;
  hardBoundary: boolean;
  palette: CurlPalette;
  lastTip: Point;
  lastProgress: number;
  animRaf: number | null;
}

function easeOutCubic(t: number): number {
  return 1 - Math.pow(1 - t, 3);
}

export function PagedCanvas({
  chapterTitle,
  chapterIndex,
  totalChapters,
  parsed,
  notesByPara,
  jumpSignal,
  hasPrevChapter,
  hasNextChapter,
  nextChapterTitle,
  ttsActive,
  ttsParaIndex,
  ttsFollowNonce = 0,
  gesturesDisabled,
  panelOpen = false,
  onProgressChange,
  onToggleHud,
  onPrevChapter,
  onNextChapter,
  onParagraphClickForTts,
}: PagedCanvasProps) {
  const fontFamily = usePrefsStore((s) => s.fontFamily);
  const customFontFamily = usePrefsStore((s) => s.customFontFamily);
  const fontSize = usePrefsStore((s) => s.fontSize);
  const lineHeight = usePrefsStore((s) => s.lineHeight);
  const paragraphGap = usePrefsStore((s) => s.paragraphGap);
  const contentWidth = usePrefsStore((s) => s.contentWidth);
  const indentParagraph = usePrefsStore((s) => s.indentParagraph);
  const spreadMode = usePrefsStore((s) => s.spreadMode);
  const pageTurnMode = usePrefsStore((s) => s.pageTurnMode);
  const theme = usePrefsStore((s) => s.theme);
  const noAnim = pageTurnMode === "none";

  const stageRef = React.useRef<HTMLDivElement>(null);
  const bookSheetRef = React.useRef<HTMLDivElement>(null);
  const liveSheetRef = React.useRef<HTMLDivElement>(null);
  const maskRef = React.useRef<HTMLDivElement>(null);
  const trackRef = React.useRef<HTMLDivElement>(null);
  const endSentinelRef = React.useRef<HTMLDivElement>(null);

  // Physical Page-Curl Overlay Refs (zero React re-renders per drag frame)
  const underLayerRef = React.useRef<HTMLDivElement>(null);
  const singlePrevTopRef = React.useRef<HTMLDivElement>(null);
  const flapMaskRef = React.useRef<HTMLDivElement>(null);
  const flapTransformRef = React.useRef<HTMLDivElement>(null);
  const curlCanvasRef = React.useRef<HTMLCanvasElement>(null);
  const curlSessionRef = React.useRef<ActiveCurlSession | null>(null);

  const [stageSize, setStageSize] = React.useState<{ w: number; h: number }>({ w: 0, h: 0 });
  const [page, setPage] = React.useState(0);
  const [totalPages, setTotalPages] = React.useState(1);
  const [layoutVersion, setLayoutVersion] = React.useState(0);
  const [edgeZoneCursor, setEdgeZoneCursor] = React.useState<"default" | "grab" | "grabbing">(
    "default",
  );
  const [boundaryHint, setBoundaryHint] = React.useState<string | null>(null);

  const anchorParaRef = React.useRef(0);
  const lastJumpNonceRef = React.useRef(-1);
  const pageRef = React.useRef(page);
  const totalPagesRef = React.useRef(totalPages);
  pageRef.current = page;
  totalPagesRef.current = totalPages;

  const fontCss = resolveReaderFontFamily(fontFamily, customFontFamily);

  // Measure available stage area
  React.useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const update = () => {
      const rect = el.getBoundingClientRect();
      const w = Math.floor(rect.width);
      const h = Math.floor(rect.height);
      setStageSize((prev) => (prev.w === w && prev.h === h ? prev : { w, h }));
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Re-measure when web fonts finish loading
  React.useEffect(() => {
    let active = true;
    void document.fonts?.ready.then(() => {
      if (active) setLayoutVersion((v) => v + 1);
    });
    return () => {
      active = false;
    };
  }, [fontFamily, customFontFamily]);

  // Compute integer-pixel spread/single geometry + physical sheet margin padding
  const metrics = React.useMemo(() => {
    const w = stageSize.w || (typeof window !== "undefined" ? window.innerWidth : 1024);
    const h = Math.max(
      200,
      stageSize.h || (typeof window !== "undefined" ? window.innerHeight - 136 : 600),
    );
    const isSpread = spreadMode === "auto" && w >= SPREAD_BREAKPOINT;

    if (isSpread) {
      const padX = 34;
      const padY = 22;
      const outerMarginY = 12;
      const availW = Math.max(560, w - 40);
      const availH = Math.max(200, h - outerMarginY * 2);
      const idealTwoCol = Math.round(contentWidth * fontSize * 1.92) + SPREAD_GUTTER;
      const maxSpreadW = Math.min(availW - padX * 2, idealTwoCol);
      const colWidth = Math.max(250, Math.floor((maxSpreadW - SPREAD_GUTTER) / 2));
      const viewportWidth = colWidth * 2 + SPREAD_GUTTER;
      const contentHeight = Math.max(180, availH - padY * 2);
      const sheetW = viewportWidth + padX * 2;
      const sheetH = contentHeight + padY * 2;
      const colGap = SPREAD_GUTTER;
      const stepPx = (colWidth + colGap) * 2;
      return {
        isSpread,
        colWidth,
        colGap,
        viewportWidth,
        contentHeight,
        padX,
        padY,
        sheetW,
        sheetH,
        stepPx,
      };
    }

    const padX = w < 640 ? 12 : 24;
    const padY = w < 640 ? 2 : 8;
    const outerSideMargin = w < 640 ? 0 : 16;
    const maxTextW = Math.max(220, w - outerSideMargin - padX * 2);
    const colWidth = Math.max(220, Math.floor(Math.min(maxTextW, contentWidth * fontSize)));
    const colGap = SINGLE_GAP;
    const viewportWidth = colWidth;
    const contentHeight = Math.max(180, h - padY * 2);
    const sheetW = viewportWidth + padX * 2;
    const sheetH = contentHeight + padY * 2;
    const stepPx = colWidth + colGap;
    return {
      isSpread,
      colWidth,
      colGap,
      viewportWidth,
      contentHeight,
      padX,
      padY,
      sheetW,
      sheetH,
      stepPx,
    };
  }, [stageSize.w, stageSize.h, spreadMode, contentWidth, fontSize]);

  const metricsRef = React.useRef(metrics);
  metricsRef.current = metrics;

  // Cleanup any active curl layers immediately
  const resetCurlDom = React.useCallback(() => {
    const sess = curlSessionRef.current;
    if (sess?.animRaf) {
      window.cancelAnimationFrame(sess.animRaf);
    }
    curlSessionRef.current = null;

    if (liveSheetRef.current) {
      liveSheetRef.current.style.clipPath = "";
    }
    if (underLayerRef.current) {
      underLayerRef.current.style.display = "none";
      underLayerRef.current.style.clipPath = "";
      underLayerRef.current.replaceChildren();
    }
    if (singlePrevTopRef.current) {
      singlePrevTopRef.current.style.display = "none";
      singlePrevTopRef.current.style.clipPath = "";
      singlePrevTopRef.current.replaceChildren();
    }
    if (flapMaskRef.current) {
      flapMaskRef.current.style.display = "none";
      flapMaskRef.current.style.clipPath = "";
    }
    if (flapTransformRef.current) {
      flapTransformRef.current.style.transform = "";
      flapTransformRef.current.replaceChildren();
    }
    if (curlCanvasRef.current) {
      curlCanvasRef.current.style.display = "none";
      const ctx = curlCanvasRef.current.getContext("2d");
      if (ctx) {
        ctx.clearRect(0, 0, curlCanvasRef.current.width, curlCanvasRef.current.height);
      }
    }
    setBoundaryHint(null);
    setEdgeZoneCursor((c) => (c === "grabbing" ? "grab" : c));
  }, []);

  React.useEffect(() => () => resetCurlDom(), [resetCurlDom]);

  // Measure total pages & preserve anchor / apply jump signal
  React.useLayoutEffect(() => {
    const track = trackRef.current;
    if (!track || stageSize.w === 0 || stageSize.h === 0) {
      resetCurlDom();
      return;
    }

    const unit = metrics.colWidth + metrics.colGap;
    const trackRect = track.getBoundingClientRect();
    let totalCols = Math.max(1, Math.round((track.scrollWidth + metrics.colGap) / unit));

    if (endSentinelRef.current) {
      const sentRect = endSentinelRef.current.getBoundingClientRect();
      const relLeft = sentRect.left - trackRect.left;
      if (Number.isFinite(relLeft) && relLeft >= 0) {
        const sentinelCols = Math.max(1, Math.floor((relLeft + 4) / unit) + 1);
        totalCols = Math.max(1, Math.min(totalCols, sentinelCols));
      }
    }

    const nextTotalPages = metrics.isSpread
      ? Math.max(1, Math.ceil(totalCols / 2))
      : totalCols;
    totalPagesRef.current = nextTotalPages;
    setTotalPages(nextTotalPages);

    const findPageForPara = (paraIdx: number): number => {
      if (paraIdx <= 0) return 0;
      const targetEl = track.querySelector<HTMLElement>(`[data-para-index="${paraIdx}"]`);
      if (!targetEl) return 0;
      const elRect = targetEl.getBoundingClientRect();
      const relX = elRect.left - trackRect.left;
      return Math.min(nextTotalPages - 1, Math.max(0, Math.floor((relX + 4) / metrics.stepPx)));
    };

    const isNewJump = lastJumpNonceRef.current !== jumpSignal.nonce;
    let nextPage: number;
    if (isNewJump) {
      lastJumpNonceRef.current = jumpSignal.nonce;
      if (jumpSignal.align === "end") {
        nextPage = nextTotalPages - 1;
        anchorParaRef.current = Math.max(0, parsed.paragraphs.length - 1);
      } else {
        anchorParaRef.current = jumpSignal.paraIndex;
        nextPage = findPageForPara(jumpSignal.paraIndex);
      }
    } else {
      nextPage = findPageForPara(anchorParaRef.current);
    }

    // Sync DOM transform synchronously BEFORE lifting curl overlay so the new chapter never paints at the old page offset
    track.style.transform = `translate3d(${-(nextPage * metrics.stepPx)}px, 0, 0)`;
    pageRef.current = nextPage;
    setPage(nextPage);
    resetCurlDom();
  }, [
    chapterIndex,
    parsed,
    metrics,
    stageSize.w,
    stageSize.h,
    fontCss,
    fontSize,
    lineHeight,
    paragraphGap,
    indentParagraph,
    jumpSignal,
    layoutVersion,
    theme,
    resetCurlDom,
  ]);

  // Report current page & first visible paragraph after page turns
  React.useEffect(() => {
    const track = trackRef.current;
    const mask = maskRef.current;
    let visiblePara = anchorParaRef.current;

    if (track && mask) {
      const trackRect = track.getBoundingClientRect();
      const pageStartX = page * metrics.stepPx;
      const pageEndX = pageStartX + metrics.viewportWidth;
      const paraEls = track.querySelectorAll<HTMLElement>("[data-para-index]");
      for (const el of paraEls) {
        const rect = el.getBoundingClientRect();
        const relLeft = rect.left - trackRect.left;
        const relRight = rect.right - trackRect.left;
        if (relRight > pageStartX + 4 && relLeft < pageEndX - 4) {
          const idx = Number(el.getAttribute("data-para-index"));
          if (Number.isFinite(idx)) {
            visiblePara = idx;
            break;
          }
        }
      }
      anchorParaRef.current = visiblePara;
    }

    const ratio = totalPages <= 1 ? 1 : (page + 1) / totalPages;
    onProgressChange({
      page,
      totalPages,
      chapterRatio: ratio,
      paraIndex: visiblePara,
    });
  }, [page, totalPages, metrics.stepPx, metrics.viewportWidth, onProgressChange]);

  // Build cloned sheet DOM node for targetPage (runs ONCE at curl start, never during drag frames!)
  const createPreviewSheetNode = React.useCallback(
    (targetPage: number, boundary: BoundaryKind): HTMLElement => {
      const m = metricsRef.current;
      const wrapper = document.createElement("div");
      wrapper.style.position = "relative";
      wrapper.style.width = `${m.sheetW}px`;
      wrapper.style.height = `${m.sheetH}px`;
      wrapper.style.padding = `${m.padY}px ${m.padX}px`;
      wrapper.style.backgroundColor = "var(--bg)";
      wrapper.style.overflow = "hidden";
      wrapper.style.userSelect = "none";

      if (m.isSpread) {
        const gutter = document.createElement("div");
        gutter.className =
          "spread-gutter pointer-events-none absolute inset-y-0 left-1/2 z-10 -translate-x-1/2";
        gutter.style.width = `${SPREAD_GUTTER}px`;
        wrapper.appendChild(gutter);
      }

      if (boundary === "within" && trackRef.current) {
        const mask = document.createElement("div");
        mask.style.position = "relative";
        mask.style.width = `${m.viewportWidth}px`;
        mask.style.height = `${m.contentHeight}px`;
        mask.style.overflow = "hidden";

        const clone = trackRef.current.cloneNode(true) as HTMLElement;
        clone.style.transform = `translate3d(${-(targetPage * m.stepPx)}px, 0, 0)`;
        clone.style.transition = "none";
        mask.appendChild(clone);
        wrapper.appendChild(mask);
        return wrapper;
      }

      // Chapter transition / book endpaper preview sheet
      const folio = document.createElement("div");
      folio.className =
        "flex size-full flex-col items-center justify-center gap-2 text-center font-serif select-none";
      const label =
        boundary === "next-chapter"
          ? `下一章${nextChapterTitle ? ` · ${nextChapterTitle}` : ""}`
          : boundary === "prev-chapter"
            ? "上一章"
            : boundary === "hard-end"
              ? "已至全书末页"
              : "已是第一章首页";
      const sub =
        boundary === "next-chapter" || boundary === "prev-chapter"
          ? "继续拖动翻入，松手退回则留在当前页"
          : "轻轻松手即可回落";

      const p1 = document.createElement("p");
      p1.className = "max-w-[78%] truncate text-base font-semibold text-ink";
      p1.textContent = label;
      const p2 = document.createElement("p");
      p2.className = "text-xs text-ink-muted";
      p2.textContent = sub;
      folio.appendChild(p1);
      folio.appendChild(p2);
      wrapper.appendChild(folio);
      return wrapper;
    },
    [nextChapterTitle],
  );

  // Apply a single curl frame directly to DOM + Retina <canvas>
  const renderCurlFrame = React.useCallback((tip: Point) => {
    const sess = curlSessionRef.current;
    if (!sess) return;
    const m = metricsRef.current;
    const geom = computeCurlGeometry(sess.mode, m.sheetW, m.sheetH, sess.cornerY, tip);
    sess.lastTip = geom.tip;
    sess.lastProgress = geom.progress;

    if (sess.mode === "single-prev") {
      if (liveSheetRef.current) liveSheetRef.current.style.clipPath = "";
      if (singlePrevTopRef.current) {
        singlePrevTopRef.current.style.clipPath = geom.topClipCss;
      }
    } else {
      if (liveSheetRef.current) {
        liveSheetRef.current.style.clipPath = geom.topClipCss;
      }
    }

    if (sess.mode === "spread-next" || sess.mode === "spread-prev") {
      if (flapMaskRef.current) {
        flapMaskRef.current.style.clipPath = geom.flapClipCss;
      }
      if (flapTransformRef.current) {
        flapTransformRef.current.style.transform = geom.flapContentMatrix;
      }
    }

    const canvas = curlCanvasRef.current;
    if (canvas) {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const targetW = Math.round(m.sheetW * dpr);
      const targetH = Math.round(m.sheetH * dpr);
      if (canvas.width !== targetW || canvas.height !== targetH) {
        canvas.width = targetW;
        canvas.height = targetH;
      }
      const ctx = canvas.getContext("2d");
      if (ctx) {
        drawCurlLighting(ctx, dpr, geom, sess.palette);
      }
    }

    if (sess.boundary === "next-chapter" && geom.progress > 0.24) {
      setBoundaryHint("松手翻入下一章 · 拖回可放下");
    } else if (sess.boundary === "prev-chapter" && geom.progress > 0.24) {
      setBoundaryHint("松手翻回上一章 · 拖回可放下");
    } else {
      setBoundaryHint(null);
    }
  }, []);

  // Initialize DOM layers for a curl gesture in direction `dir`
  const beginCurlSession = React.useCallback(
    (dir: "next" | "prev", cornerY: "top" | "bottom"): ActiveCurlSession | null => {
      resetCurlDom();
      const m = metricsRef.current;
      const curPage = pageRef.current;
      const totPages = totalPagesRef.current;

      let boundary: BoundaryKind = "within";
      let targetPage = curPage;
      if (dir === "next") {
        if (curPage < totPages - 1) {
          targetPage = curPage + 1;
        } else if (hasNextChapter) {
          boundary = "next-chapter";
        } else {
          boundary = "hard-end";
        }
      } else {
        if (curPage > 0) {
          targetPage = curPage - 1;
        } else if (hasPrevChapter) {
          boundary = "prev-chapter";
        } else {
          boundary = "hard-start";
        }
      }

      const hardBoundary = boundary === "hard-end" || boundary === "hard-start";
      const mode: CurlMode = m.isSpread
        ? dir === "next"
          ? "spread-next"
          : "spread-prev"
        : dir === "next"
          ? "single-next"
          : "single-prev";

      const palette = readThemeCurlPalette(bookSheetRef.current);
      const previewNode = createPreviewSheetNode(targetPage, boundary);

      if (mode === "single-prev") {
        if (singlePrevTopRef.current) {
          singlePrevTopRef.current.replaceChildren(previewNode);
          singlePrevTopRef.current.style.display = "block";
        }
      } else if (mode === "single-next") {
        if (underLayerRef.current) {
          underLayerRef.current.style.clipPath = "";
          underLayerRef.current.replaceChildren(previewNode);
          underLayerRef.current.style.display = "block";
        }
      } else {
        // Dual-page spread: underLayer holds revealed outer half; flapTransform holds incoming opposite half!
        if (underLayerRef.current) {
          underLayerRef.current.style.clipPath =
            mode === "spread-next" ? "inset(0 0 0 50%)" : "inset(0 50% 0 0)";
          underLayerRef.current.replaceChildren(previewNode);
          underLayerRef.current.style.display = "block";
        }
        if (flapMaskRef.current && flapTransformRef.current) {
          const flapNode = previewNode.cloneNode(true) as HTMLElement;
          flapNode.style.clipPath =
            mode === "spread-next" ? "inset(0 50% 0 0)" : "inset(0 0 0 50%)";
          flapTransformRef.current.replaceChildren(flapNode);
          flapMaskRef.current.style.display = "block";
        }
      }

      if (curlCanvasRef.current) {
        curlCanvasRef.current.style.display = "block";
      }

      const initTip = progressToArcTip(mode, m.sheetW, m.sheetH, cornerY, 0.01, 8);
      const sess: ActiveCurlSession = {
        mode,
        dir,
        cornerY,
        targetPage,
        boundary,
        hardBoundary,
        palette,
        lastTip: initTip,
        lastProgress: 0.01,
        animRaf: null,
      };
      curlSessionRef.current = sess;
      renderCurlFrame(initTip);
      return sess;
    },
    [createPreviewSheetNode, hasNextChapter, hasPrevChapter, renderCurlFrame, resetCurlDom],
  );

  // Settle curl smoothly either to `commit = true` (progress -> 1) or `commit = false` ("放下", progress -> 0)
  const settleCurlSession = React.useCallback(
    (commit: boolean, customDurationMs?: number) => {
      const sess = curlSessionRef.current;
      if (!sess) return;
      const m = metricsRef.current;
      const willCommit = commit && !sess.hardBoundary;
      const fromTip = { ...sess.lastTip };
      const toTip = progressToArcTip(
        sess.mode,
        m.sheetW,
        m.sheetH,
        sess.cornerY,
        willCommit ? 0.999 : 0.002,
        4,
      );

      const distRatio = Math.min(
        1,
        Math.hypot(toTip.x - fromTip.x, toTip.y - fromTip.y) / Math.max(1, m.sheetW),
      );
      const duration =
        customDurationMs ?? Math.round(Math.max(190, Math.min(360, 175 + distRatio * 205)));
      const startTs = performance.now();
      const liftSign = sess.cornerY === "bottom" ? -1 : 1;
      const extraArch = willCommit ? Math.min(34, m.sheetH * 0.055) : Math.min(14, m.sheetH * 0.025);

      const step = (now: number) => {
        if (curlSessionRef.current !== sess) return;
        const rawT = Math.min(1, (now - startTs) / duration);
        const e = easeOutCubic(rawT);
        const curTip: Point = {
          x: fromTip.x + (toTip.x - fromTip.x) * e,
          y:
            fromTip.y +
            (toTip.y - fromTip.y) * e +
            liftSign * Math.sin(rawT * Math.PI) * extraArch * distRatio,
        };
        renderCurlFrame(curTip);

        if (rawT < 1) {
          sess.animRaf = window.requestAnimationFrame(step);
          return;
        }

        // Seamless zero-flash handoff on final frame
        if (willCommit) {
          if (sess.boundary === "within") {
            if (trackRef.current) {
              trackRef.current.style.transform = `translate3d(${-(sess.targetPage * m.stepPx)}px, 0, 0)`;
            }
            pageRef.current = sess.targetPage;
            setPage(sess.targetPage);
            resetCurlDom();
          } else if (sess.boundary === "next-chapter") {
            // Keep the chapter transition sheet covering the old chapter until React commits the new chapter's layout (lifted atomically in useLayoutEffect)
            sess.animRaf = null;
            onNextChapter();
          } else if (sess.boundary === "prev-chapter") {
            sess.animRaf = null;
            onPrevChapter("end");
          } else {
            resetCurlDom();
          }
        } else {
          resetCurlDom();
        }
      };

      sess.animRaf = window.requestAnimationFrame(step);
    },
    [onNextChapter, onPrevChapter, renderCurlFrame, resetCurlDom],
  );

  // Programmatic page turn (tap / keyboard / trackpad / TTS follow) with smooth curl arc
  const triggerCurlTurn = React.useCallback(
    (dir: "next" | "prev", cornerY: "top" | "bottom" = "bottom") => {
      if (curlSessionRef.current?.animRaf) {
        // Fast consecutive clicks: finish previous in-flight turn immediately
        const prev = curlSessionRef.current;
        if (prev.boundary === "within" && !prev.hardBoundary) {
          if (trackRef.current) {
            trackRef.current.style.transform = `translate3d(${-(prev.targetPage * metricsRef.current.stepPx)}px, 0, 0)`;
          }
          pageRef.current = prev.targetPage;
          setPage(prev.targetPage);
        }
        resetCurlDom();
      }

      if (noAnim) {
        if (dir === "next") {
          if (pageRef.current < totalPagesRef.current - 1) {
            const nextP = Math.min(totalPagesRef.current - 1, pageRef.current + 1);
            pageRef.current = nextP;
            setPage(nextP);
          } else if (hasNextChapter) {
            onNextChapter();
          }
        } else if (pageRef.current > 0) {
          const prevP = Math.max(0, pageRef.current - 1);
          pageRef.current = prevP;
          setPage(prevP);
        } else if (hasPrevChapter) {
          onPrevChapter("end");
        }
        return;
      }

      if (dir === "next" && pageRef.current >= totalPagesRef.current - 1 && !hasNextChapter) {
        return;
      }
      if (dir === "prev" && pageRef.current <= 0 && !hasPrevChapter) {
        return;
      }

      const sess = beginCurlSession(dir, cornerY);
      if (!sess) return;
      settleCurlSession(true, 340);
    },
    [beginCurlSession, hasNextChapter, hasPrevChapter, noAnim, onNextChapter, onPrevChapter, resetCurlDom, settleCurlSession],
  );

  React.useEffect(() => {
    if (noAnim || panelOpen) {
      if (noAnim) resetCurlDom();
      setEdgeZoneCursor("default");
    }
  }, [noAnim, panelOpen, resetCurlDom]);

  const turnNext = React.useCallback(() => triggerCurlTurn("next", "bottom"), [triggerCurlTurn]);
  const turnPrev = React.useCallback(() => triggerCurlTurn("prev", "bottom"), [triggerCurlTurn]);

  // Follow active TTS paragraph automatically (and when user clicks "定位朗读段")
  React.useEffect(() => {
    if (!ttsActive || ttsParaIndex === null) return;
    const track = trackRef.current;
    if (!track) return;
    const targetEl = track.querySelector<HTMLElement>(`[data-para-index="${ttsParaIndex}"]`);
    if (!targetEl) return;
    const trackRect = track.getBoundingClientRect();
    const elRect = targetEl.getBoundingClientRect();
    const relX = elRect.left - trackRect.left;
    const targetPage = Math.min(
      totalPagesRef.current - 1,
      Math.max(0, Math.floor((relX + 4) / metrics.stepPx)),
    );
    if (targetPage !== pageRef.current) {
      if (targetPage === pageRef.current + 1) {
        triggerCurlTurn("next", "bottom");
      } else {
        resetCurlDom();
        pageRef.current = targetPage;
        setPage(targetPage);
      }
    }
  }, [ttsActive, ttsParaIndex, ttsFollowNonce, metrics.stepPx, resetCurlDom, triggerCurlTurn]);

  // Keyboard navigation
  React.useEffect(() => {
    if (gesturesDisabled) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.isComposing || e.defaultPrevented) return;
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          target.isContentEditable)
      ) {
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      if (e.key === "ArrowRight" || e.key === "PageDown") {
        e.preventDefault();
        turnNext();
      } else if (e.key === "ArrowLeft" || e.key === "PageUp") {
        e.preventDefault();
        turnPrev();
      } else if (e.key === " ") {
        e.preventDefault();
        if (e.shiftKey) turnPrev();
        else turnNext();
      } else if (e.key === "Home") {
        e.preventDefault();
        resetCurlDom();
        pageRef.current = 0;
        setPage(0);
      } else if (e.key === "End") {
        e.preventDefault();
        resetCurlDom();
        const lastP = Math.max(0, totalPagesRef.current - 1);
        pageRef.current = lastP;
        setPage(lastP);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [gesturesDisabled, resetCurlDom, turnNext, turnPrev]);

  // Trackpad two-finger horizontal swipe
  const wheelCoolRef = React.useRef(0);
  const wheelAccumRef = React.useRef(0);
  const wheelResetRef = React.useRef<number | null>(null);

  React.useEffect(() => {
    const el = stageRef.current;
    if (!el || gesturesDisabled) return;

    const onWheel = (e: WheelEvent) => {
      const absX = Math.abs(e.deltaX);
      const absY = Math.abs(e.deltaY);
      if (absX < 6 || absX < absY * 1.15) return;

      e.preventDefault();
      const now = Date.now();
      if (now < wheelCoolRef.current) return;

      wheelAccumRef.current += e.deltaX;
      if (wheelResetRef.current !== null) window.clearTimeout(wheelResetRef.current);
      wheelResetRef.current = window.setTimeout(() => {
        wheelAccumRef.current = 0;
      }, 180);

      if (Math.abs(wheelAccumRef.current) >= 38) {
        const dir = wheelAccumRef.current > 0 ? 1 : -1;
        wheelAccumRef.current = 0;
        wheelCoolRef.current = now + (noAnim ? 220 : 400);
        if (dir > 0) turnNext();
        else turnPrev();
      }
    };

    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      if (wheelResetRef.current !== null) window.clearTimeout(wheelResetRef.current);
    };
  }, [gesturesDisabled, noAnim, turnNext, turnPrev]);

  // Unified Mouse + Touch Physical Drag-to-Peel & Drop-to-Cancel Controller
  const hadSelectionOnDownRef = React.useRef(false);
  const dragRef = React.useRef<{
    active: boolean;
    pointerId: number;
    pointerType: string;
    canDragPeel: boolean;
    locked: "none" | "curl" | "scroll";
    startClientX: number;
    startClientY: number;
    startLocalX: number;
    startLocalY: number;
    lastClientX: number;
    lastTs: number;
    velX: number;
    justDragged: boolean;
  }>({
    active: false,
    pointerId: -1,
    pointerType: "mouse",
    canDragPeel: false,
    locked: "none",
    startClientX: 0,
    startClientY: 0,
    startLocalX: 0,
    startLocalY: 0,
    lastClientX: 0,
    lastTs: 0,
    velX: 0,
    justDragged: false,
  });

  const isInMousePeelZone = (clientX: number, clientY: number): boolean => {
    const sheet = bookSheetRef.current;
    if (!sheet) return false;
    const rect = sheet.getBoundingClientRect();
    const relX = (clientX - rect.left) / Math.max(1, rect.width);
    const relY = (clientY - rect.top) / Math.max(1, rect.height);
    return relX <= 0.35 || relX >= 0.65 || relY <= 0.14 || relY >= 0.85;
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (gesturesDisabled) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;

    const sel = window.getSelection();
    hadSelectionOnDownRef.current = Boolean(
      sel && !sel.isCollapsed && sel.toString().trim().length > 0,
    );

    const target = e.target as HTMLElement | null;
    if (target?.closest("button, a, input, textarea, [data-no-tap]")) return;

    const sheet = bookSheetRef.current;
    if (!sheet) return;
    const rect = sheet.getBoundingClientRect();
    const localX = e.clientX - rect.left;
    const localY = e.clientY - rect.top;

    // Disable drag-peel when HUD/Aa panel is open or when dismissing an active text selection
    const canDragPeel =
      !panelOpen &&
      !hadSelectionOnDownRef.current &&
      (e.pointerType !== "mouse" || (!noAnim && isInMousePeelZone(e.clientX, e.clientY)));

    dragRef.current = {
      active: true,
      pointerId: e.pointerId,
      pointerType: e.pointerType,
      canDragPeel,
      locked: "none",
      startClientX: e.clientX,
      startClientY: e.clientY,
      startLocalX: localX,
      startLocalY: localY,
      lastClientX: e.clientX,
      lastTs: performance.now(),
      velX: 0,
      justDragged: false,
    };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current.active && e.pointerType === "mouse") {
      setEdgeZoneCursor(
        !noAnim && !panelOpen && isInMousePeelZone(e.clientX, e.clientY) ? "grab" : "default",
      );
      return;
    }

    const st = dragRef.current;
    if (!st.active || e.pointerId !== st.pointerId || !st.canDragPeel) return;

    const sheet = bookSheetRef.current;
    if (!sheet) return;
    const rect = sheet.getBoundingClientRect();
    const curLocalX = e.clientX - rect.left;
    const curLocalY = e.clientY - rect.top;
    const dx = e.clientX - st.startClientX;
    const dy = e.clientY - st.startClientY;

    const now = performance.now();
    const dt = Math.max(1, now - st.lastTs);
    const instVel = (e.clientX - st.lastClientX) / dt;
    st.velX = st.velX * 0.55 + instVel * 0.45;
    st.lastClientX = e.clientX;
    st.lastTs = now;

    if (st.locked === "none") {
      const minMove = st.pointerType === "mouse" ? 5 : 9;
      if (Math.hypot(dx, dy) >= minMove) {
        if (st.pointerType !== "mouse" && Math.abs(dy) > Math.abs(dx) * 1.25) {
          st.locked = "scroll";
          return;
        }
        // Determine peel direction: leftward drag or starting near right edge => next; rightward drag from left => prev
        const relStart = st.startLocalX / Math.max(1, rect.width);
        const dir: "next" | "prev" =
          dx < -2 ? "next" : dx > 2 ? "prev" : relStart >= 0.5 ? "next" : "prev";
        const cornerY: "top" | "bottom" = st.startLocalY < rect.height * 0.4 ? "top" : "bottom";

        window.getSelection()?.removeAllRanges();
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // ignore
        }

        if (noAnim) {
          st.locked = "curl";
          st.justDragged = true;
          return;
        }

        beginCurlSession(dir, cornerY);
        st.locked = "curl";
        st.justDragged = true;
        if (st.pointerType === "mouse") setEdgeZoneCursor("grabbing");
      }
      return;
    }

    if (st.locked === "curl" && curlSessionRef.current && !noAnim) {
      const sess = curlSessionRef.current;
      const m = metricsRef.current;
      const tip = pointerToCurlTip({
        mode: sess.mode,
        w: m.sheetW,
        h: m.sheetH,
        cornerY: sess.cornerY,
        startX: st.startLocalX,
        startY: st.startLocalY,
        curX: curLocalX,
        curY: curLocalY,
        hardBoundary: sess.hardBoundary,
      });
      renderCurlFrame(tip);
    }
  };

  const finishPointerDrag = (e: React.PointerEvent<HTMLDivElement>, cancelled: boolean) => {
    const st = dragRef.current;
    if (!st.active || e.pointerId !== st.pointerId) return;
    st.active = false;

    if (st.locked === "curl") {
      try {
        e.currentTarget.releasePointerCapture(e.pointerId);
      } catch {
        // ignore
      }

      const rawDx = e.clientX - st.startClientX;
      if (noAnim) {
        if (!cancelled && rawDx < -48) turnNext();
        else if (!cancelled && rawDx > 48) turnPrev();
      } else if (curlSessionRef.current) {
        const sess = curlSessionRef.current;
        if (cancelled || sess.hardBoundary) {
          settleCurlSession(false);
        } else {
          // Commit if dragged past 28% (unless actively dragging back toward origin) OR fast flick in turn direction
          const forwardVel = sess.dir === "next" ? -st.velX : st.velX;
          const activelyReturning = forwardVel < -0.18;
          const fastFlick = forwardVel > 0.36 && sess.lastProgress > 0.05;
          const pastThreshold = sess.lastProgress >= 0.28 && !activelyReturning;
          settleCurlSession(pastThreshold || fastFlick);
        }
      }

      if (e.pointerType === "mouse") {
        setEdgeZoneCursor(
          !noAnim && !panelOpen && isInMousePeelZone(e.clientX, e.clientY) ? "grab" : "default",
        );
      }
      window.setTimeout(() => {
        dragRef.current.justDragged = false;
      }, 90);
    }
  };

  // Click tap zones (Left 28% / Center 44% / Right 28%); when HUD or Aa panel is open, blank click always folds panel smoothly first
  const handleStageClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if (dragRef.current.justDragged) return;
    if (hadSelectionOnDownRef.current) {
      hadSelectionOnDownRef.current = false;
      return;
    }
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed && sel.toString().trim().length > 0) return;

    const target = e.target as HTMLElement | null;
    if (target?.closest("button, a, input, textarea, [data-no-tap]")) return;

    if (panelOpen) {
      onToggleHud();
      return;
    }

    const rect = e.currentTarget.getBoundingClientRect();
    const relX = (e.clientX - rect.left) / Math.max(1, rect.width);
    const relY = (e.clientY - rect.top) / Math.max(1, rect.height);
    const cornerY: "top" | "bottom" = relY < 0.35 ? "top" : "bottom";

    if (relX < 0.28) {
      triggerCurlTurn("prev", cornerY);
    } else if (relX > 0.72) {
      triggerCurlTurn("next", cornerY);
    } else {
      onToggleHud();
    }
  };

  return (
    <div
      onClick={handleStageClick}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={(e) => finishPointerDrag(e, false)}
      onPointerCancel={(e) => finishPointerDrag(e, true)}
      onPointerLeave={() => {
        if (!dragRef.current.active) setEdgeZoneCursor("default");
      }}
      style={{ touchAction: "pan-y" }}
      className={cn(
        "relative flex h-full w-full flex-col overflow-hidden pt-3 pb-6 safe-t md:pt-8 md:pb-7",
        edgeZoneCursor === "grabbing"
          ? "cursor-grabbing select-none"
          : edgeZoneCursor === "grab"
            ? "cursor-grab"
            : "",
      )}
    >
      {/* Inner Measurable Reading Stage */}
      <div
        ref={stageRef}
        className="relative flex min-h-0 flex-1 items-center justify-center overflow-visible"
      >
        {/* Drag Chapter Boundary Hint Pill */}
        {boundaryHint ? (
          <div className="surface-glass pointer-events-none absolute top-4 left-1/2 z-40 -translate-x-1/2 rounded-full px-3.5 py-1 text-xs text-ink">
            {boundaryHint}
          </div>
        ) : null}

        {/* Physical Book Sheet Stage (houses live page + real-time curl peel layers) */}
        <div
          ref={bookSheetRef}
          style={{
            width: metrics.sheetW,
            height: metrics.sheetH,
          }}
          className={cn(
            "relative overflow-hidden bg-bg",
            metrics.isSpread &&
              "rounded-[8px] border border-line-strong/65 shadow-[0_14px_38px_-14px_rgba(24,18,12,0.18),-3px_0_0_-1px_var(--bg),-4px_0_0_-1px_var(--line),3px_0_0_-1px_var(--bg),4px_0_0_-1px_var(--line)] dark:border-line-strong/55 dark:shadow-[0_18px_46px_-12px_rgba(0,0,0,0.6),-3px_0_0_-1px_var(--bg),-4px_0_0_-1px_var(--line),3px_0_0_-1px_var(--bg),4px_0_0_-1px_var(--line)]",
          )}
        >
          {/* Layer 0: Underneath Target Page (visible only while peeling) */}
          <div
            ref={underLayerRef}
            aria-hidden
            style={{ display: "none" }}
            className="pointer-events-none absolute inset-0 z-5 overflow-hidden bg-bg"
          />

          {/* Layer 1: Live Interactive Reading Sheet */}
          <div
            ref={liveSheetRef}
            style={{
              width: metrics.sheetW,
              height: metrics.sheetH,
              padding: `${metrics.padY}px ${metrics.padX}px`,
            }}
            className="relative z-10 bg-bg"
          >
            {/* Open-Book Center Spine Shadow (Wide-Screen Spread) */}
            {metrics.isSpread ? (
              <div
                aria-hidden
                style={{
                  width: SPREAD_GUTTER,
                }}
                className="spread-gutter pointer-events-none absolute inset-y-0 left-1/2 z-10 -translate-x-1/2"
              />
            ) : null}

            {/* Viewport Mask */}
            <div
              ref={maskRef}
              style={{
                width: metrics.viewportWidth,
                height: metrics.contentHeight,
              }}
              className="relative overflow-hidden"
            >
              {/* Multi-Column Horizontal Track */}
              <div
                ref={trackRef}
                style={{
                  width: metrics.viewportWidth,
                  height: metrics.contentHeight,
                  columnWidth: `${metrics.colWidth}px`,
                  columnGap: `${metrics.colGap}px`,
                  columnFill: "auto",
                  fontFamily: fontCss,
                  fontSize: `${fontSize}px`,
                  lineHeight,
                  transform: `translate3d(${-(page * metrics.stepPx)}px, 0, 0)`,
                  willChange: "transform",
                }}
                className="box-content text-justify tracking-[0.015em] text-ink antialiased"
              >
                {/* Chapter Opening Header */}
                <header
                  style={{
                    breakInside: "avoid",
                    marginBottom: `${Math.max(1.1, paragraphGap * 1.35)}em`,
                  }}
                  className="pt-1 pb-2"
                >
                  <p className="font-mono text-[11px] tracking-[0.14em] text-ink-muted uppercase">
                    第 {chapterIndex + 1} 章{totalChapters > 0 ? ` / ${totalChapters}` : ""}
                  </p>
                  <h1
                    style={{ fontSize: `${Math.round(fontSize * 1.28)}px` }}
                    className="mt-1.5 font-serif leading-snug font-semibold tracking-tight text-ink text-balance"
                  >
                    {chapterTitle || `第 ${chapterIndex + 1} 章`}
                  </h1>
                  <span aria-hidden className="mt-3 block h-[2px] w-7 rounded-full bg-accent/55" />
                </header>

                {/* Body Content */}
                {parsed.kind === "html" && parsed.htmlBlocks ? (
                  parsed.htmlBlocks.map((html, idx) => {
                    const isTtsCurrent = ttsActive && ttsParaIndex === idx;
                    return (
                      <div
                        key={idx}
                        data-para-index={idx}
                        onClick={
                          ttsActive && onParagraphClickForTts
                            ? (e) => {
                                e.stopPropagation();
                                onParagraphClickForTts(idx);
                              }
                            : undefined
                        }
                        style={{
                          marginBottom: `${paragraphGap}em`,
                          textIndent: indentParagraph && !html.includes("<img") ? "2em" : undefined,
                        }}
                        className={cn(
                          "reader-html-block break-words transition-colors duration-200",
                          isTtsCurrent &&
                            "rounded-lg bg-accent-soft/75 px-2 py-1 ring-1 ring-accent/25",
                          ttsActive && "cursor-pointer",
                        )}
                        dangerouslySetInnerHTML={{ __html: html }}
                        onLoadCapture={() => setLayoutVersion((v) => v + 1)}
                      />
                    );
                  })
                ) : (
                  parsed.paragraphs.map((para, idx) => {
                    const isTtsCurrent = ttsActive && ttsParaIndex === idx;
                    const paraNotes = notesByPara.get(idx);
                    return (
                      <p
                        key={idx}
                        data-para-index={idx}
                        onClick={
                          ttsActive && onParagraphClickForTts
                            ? (e) => {
                                e.stopPropagation();
                                onParagraphClickForTts(idx);
                              }
                            : undefined
                        }
                        style={{
                          marginBottom: `${paragraphGap}em`,
                          textIndent: indentParagraph ? "2em" : undefined,
                        }}
                        className={cn(
                          "break-words transition-colors duration-200",
                          isTtsCurrent &&
                            "-mx-1.5 rounded-lg bg-accent-soft/75 px-1.5 py-0.5 ring-1 ring-accent/25",
                          ttsActive && "cursor-pointer",
                        )}
                      >
                        {renderHighlightedText(para, paraNotes)}
                      </p>
                    );
                  })
                )}

                {/* Chapter-End Colophon */}
                <ChapterEndColophon
                  compact
                  hasPrev={hasPrevChapter}
                  hasNext={hasNextChapter}
                  nextChapterTitle={nextChapterTitle}
                  onPrevChapter={() => onPrevChapter("start")}
                  onNextChapter={onNextChapter}
                />

                {/* End-of-flow pagination sentinel */}
                <div ref={endSentinelRef} aria-hidden className="h-px w-px opacity-0" />
              </div>
            </div>
          </div>

          {/* Layer 2A: Unrolling Previous Page Sheet (single-prev mode only) */}
          <div
            ref={singlePrevTopRef}
            aria-hidden
            style={{ display: "none" }}
            className="pointer-events-none absolute inset-0 z-15 overflow-hidden bg-bg"
          />

          {/* Layer 2B: Reflected Backside Flap with Rigid-Transformed Incoming Leaf (spread mode) */}
          <div
            ref={flapMaskRef}
            aria-hidden
            style={{ display: "none" }}
            className="pointer-events-none absolute inset-0 z-20 overflow-hidden"
          >
            <div
              ref={flapTransformRef}
              style={{ transformOrigin: "0 0", willChange: "transform" }}
              className="absolute top-0 left-0 bg-bg"
            />
          </div>

          {/* Layer 3: Retina <canvas> 3D Cylindrical Curl Lighting & Dual Cast Shadows */}
          <canvas
            ref={curlCanvasRef}
            aria-hidden
            style={{
              display: "none",
              width: metrics.sheetW,
              height: metrics.sheetH,
            }}
            className="pointer-events-none absolute inset-0 z-30"
          />
        </div>
      </div>
    </div>
  );
}
