import * as React from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { cn } from "@/lib/cn";
import { ensureReaderFontLoaded, resolveReaderFontFamily } from "@/lib/readerFonts";
import type { ReadingNote } from "@/stores/notes";
import { usePrefsStore } from "@/stores/prefs";
import {
  ChapterEndColophon,
  renderHighlightedText,
  type JumpSignal,
  type PageProgressInfo,
  type ParsedChapter,
} from "./PagedCanvas";

export interface ScrollCanvasProps {
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
  gesturesDisabled?: boolean;
  autoScrollActive: boolean;
  onStopAutoScroll: () => void;
  onProgressChange: (info: PageProgressInfo) => void;
  onToggleHud: () => void;
  onPrevChapter: (align?: "start" | "end") => void;
  onNextChapter: () => void;
  onParagraphClickForTts?: (paraIndex: number) => void;
}

const VIRTUAL_THRESHOLD = 200;

interface ScrollVirtuosoContext {
  header: React.ReactNode;
  footer: React.ReactNode;
  columnStyle: React.CSSProperties;
}

const VIRTUOSO_COMPONENTS = {
  Header: ({ context }: { context?: ScrollVirtuosoContext }) =>
    context ? (
      <div style={context.columnStyle} className="mx-auto w-full px-5 sm:px-8">
        {context.header}
      </div>
    ) : null,
  Footer: ({ context }: { context?: ScrollVirtuosoContext }) =>
    context ? (
      <div style={context.columnStyle} className="mx-auto w-full px-5 sm:px-8">
        {context.footer}
      </div>
    ) : null,
};

export function ScrollCanvas({
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
  gesturesDisabled = false,
  autoScrollActive,
  onStopAutoScroll,
  onProgressChange,
  onToggleHud,
  onPrevChapter,
  onNextChapter,
  onParagraphClickForTts,
}: ScrollCanvasProps) {
  const fontFamily = usePrefsStore((s) => s.fontFamily);
  const customFontFamily = usePrefsStore((s) => s.customFontFamily);
  const fontSize = usePrefsStore((s) => s.fontSize);
  const lineHeight = usePrefsStore((s) => s.lineHeight);
  const paragraphGap = usePrefsStore((s) => s.paragraphGap);
  const contentWidth = usePrefsStore((s) => s.contentWidth);
  const indentParagraph = usePrefsStore((s) => s.indentParagraph);
  const autoScrollSpeed = usePrefsStore((s) => s.autoScrollSpeed);

  const fontCss = resolveReaderFontFamily(fontFamily, customFontFamily);

  React.useEffect(() => {
    ensureReaderFontLoaded(fontFamily);
  }, [fontFamily]);

  const isComic = parsed.kind === "comic" && (parsed.images?.length ?? 0) > 0;
  const itemCount = isComic
    ? parsed.images!.length
    : parsed.kind === "html" && parsed.htmlBlocks
      ? parsed.htmlBlocks.length
      : parsed.paragraphs.length;
  const useVirtual = !isComic && itemCount > VIRTUAL_THRESHOLD;

  const scrollerRef = React.useRef<HTMLDivElement | null>(null);
  const virtuosoRef = React.useRef<VirtuosoHandle>(null);
  const lastJumpNonceRef = React.useRef(-1);
  const lastParaRef = React.useRef(0);

  const reportScrollMetrics = React.useCallback(() => {
    const el = scrollerRef.current;
    if (!el) return;
    const maxScroll = Math.max(1, el.scrollHeight - el.clientHeight);
    const ratio = el.scrollHeight <= el.clientHeight + 2 ? 1 : Math.min(1, Math.max(0, el.scrollTop / maxScroll));

    let paraIndex = lastParaRef.current;
    if (!useVirtual) {
      const containerTop = el.getBoundingClientRect().top + 80;
      const paraEls = el.querySelectorAll<HTMLElement>("[data-para-index]");
      for (const p of paraEls) {
        if (p.getBoundingClientRect().bottom >= containerTop) {
          const idx = Number(p.getAttribute("data-para-index"));
          if (Number.isFinite(idx)) {
            paraIndex = idx;
            break;
          }
        }
      }
      lastParaRef.current = paraIndex;
    }

    onProgressChange({
      page: 0,
      totalPages: 1,
      chapterRatio: ratio,
      paraIndex,
    });
  }, [onProgressChange, useVirtual]);

  // Apply jump signal (chapter start, end, bookmark, note, cross-device sync)
  React.useEffect(() => {
    if (lastJumpNonceRef.current === jumpSignal.nonce) return;
    lastJumpNonceRef.current = jumpSignal.nonce;
    lastParaRef.current = jumpSignal.paraIndex;

    const timer = window.setTimeout(() => {
      if (useVirtual) {
        if (jumpSignal.align === "end") {
          virtuosoRef.current?.scrollToIndex({
            index: Math.max(0, itemCount - 1),
            align: "end",
            behavior: "auto",
          });
        } else {
          virtuosoRef.current?.scrollToIndex({
            index: Math.min(Math.max(0, itemCount - 1), jumpSignal.paraIndex),
            align: "start",
            behavior: "auto",
          });
        }
      } else if (scrollerRef.current) {
        const el = scrollerRef.current;
        if (jumpSignal.align === "end") {
          el.scrollTop = el.scrollHeight;
        } else if (jumpSignal.paraIndex <= 0) {
          el.scrollTop = 0;
        } else {
          const target = el.querySelector<HTMLElement>(
            `[data-para-index="${jumpSignal.paraIndex}"]`,
          );
          if (target) {
            target.scrollIntoView({ behavior: "auto", block: "start" });
          } else {
            el.scrollTop = 0;
          }
        }
      }
      reportScrollMetrics();
    }, 24);

    return () => window.clearTimeout(timer);
  }, [jumpSignal, itemCount, useVirtual, reportScrollMetrics]);

  // Smoothly follow TTS active paragraph (and re-center on explicit locate request)
  React.useEffect(() => {
    if (!ttsActive || ttsParaIndex === null) return;
    if (useVirtual) {
      virtuosoRef.current?.scrollToIndex({
        index: ttsParaIndex,
        align: "center",
        behavior: "smooth",
      });
    } else if (scrollerRef.current) {
      const target = scrollerRef.current.querySelector<HTMLElement>(
        `[data-para-index="${ttsParaIndex}"]`,
      );
      target?.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [ttsActive, ttsParaIndex, ttsFollowNonce, useVirtual]);

  // Keyboard scrolling in Scroll layout (Space /Shift+Space / PageUp / PageDown / Arrows)
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

      const el = scrollerRef.current;
      if (!el) return;
      const pageStep = Math.max(220, Math.round(el.clientHeight * 0.82));

      if (e.key === " " || e.key === "PageDown" || e.key === "ArrowRight") {
        e.preventDefault();
        const delta = e.key === " " && e.shiftKey ? -pageStep : pageStep;
        el.scrollBy({ top: delta, behavior: "smooth" });
      } else if (e.key === "PageUp" || e.key === "ArrowLeft") {
        e.preventDefault();
        el.scrollBy({ top: -pageStep, behavior: "smooth" });
      } else if (e.key === "ArrowDown" && target !== el && !el.contains(target)) {
        e.preventDefault();
        el.scrollBy({ top: 110, behavior: "smooth" });
      } else if (e.key === "ArrowUp" && target !== el && !el.contains(target)) {
        e.preventDefault();
        el.scrollBy({ top: -110, behavior: "smooth" });
      } else if (e.key === "Home") {
        e.preventDefault();
        el.scrollTo({ top: 0, behavior: "smooth" });
      } else if (e.key === "End") {
        e.preventDefault();
        el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [gesturesDisabled]);

  // Silky requestAnimationFrame auto-scroll with sub-pixel accumulator
  React.useEffect(() => {
    if (!autoScrollActive) return;
    let rafId = 0;
    let lastTs = performance.now();
    let carry = 0;

    const tick = (now: number) => {
      const el = scrollerRef.current;
      const dt = Math.min(100, Math.max(0, now - lastTs));
      lastTs = now;

      if (el) {
        const maxScroll = el.scrollHeight - el.clientHeight;
        if (el.scrollTop >= maxScroll - 1) {
          onStopAutoScroll();
          return;
        }
        carry += (autoScrollSpeed * dt) / 1000;
        if (carry >= 1) {
          const step = Math.floor(carry);
          carry -= step;
          el.scrollTop += step;
        }
      }
      rafId = window.requestAnimationFrame(tick);
    };

    rafId = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(rafId);
  }, [autoScrollActive, autoScrollSpeed, onStopAutoScroll]);

  const hadSelectionOnDownRef = React.useRef(false);

  const handlePointerDown = () => {
    const sel = window.getSelection();
    hadSelectionOnDownRef.current = Boolean(
      sel && !sel.isCollapsed && sel.toString().trim().length > 0,
    );
  };

  const handleCanvasClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if (hadSelectionOnDownRef.current) {
      hadSelectionOnDownRef.current = false;
      return;
    }
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed && sel.toString().trim().length > 0) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest("button, a, input, textarea, [data-no-tap]")) return;
    onToggleHud();
  };

  const renderParagraphRow = (idx: number) => {
    const isTtsCurrent = ttsActive && ttsParaIndex === idx;
    if (parsed.kind === "html" && parsed.htmlBlocks) {
      const html = parsed.htmlBlocks[idx] ?? "";
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
            isTtsCurrent && "rounded-lg bg-accent-soft/75 px-2.5 py-1 ring-1 ring-accent/25",
            ttsActive && "cursor-pointer",
          )}
          dangerouslySetInnerHTML={{ __html: html }}
        />
      );
    }

    const para = parsed.paragraphs[idx] ?? "";
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
            "-mx-2 rounded-lg bg-accent-soft/75 px-2 py-1 ring-1 ring-accent/25",
          ttsActive && "cursor-pointer",
        )}
      >
        {renderHighlightedText(para, paraNotes)}
      </p>
    );
  };

  const chapterHeader = React.useMemo(
    () => (
      <header className="pt-5 pb-5 safe-t md:pt-11">
        <p className="font-mono text-[11px] tracking-[0.14em] text-ink-muted uppercase">
          第 {chapterIndex + 1} 章{totalChapters > 0 ? ` / ${totalChapters}` : ""}
        </p>
        <h1
          style={{ fontSize: `${Math.round(fontSize * 1.32)}px` }}
          className="mt-2 font-serif leading-snug font-semibold tracking-tight text-ink text-balance"
        >
          {chapterTitle || `第 ${chapterIndex + 1} 章`}
        </h1>
        <span aria-hidden className="mt-3.5 block h-[2px] w-8 rounded-full bg-accent/55" />
      </header>
    ),
    [chapterIndex, totalChapters, fontSize, chapterTitle],
  );

  const chapterFooter = React.useMemo(
    () => (
      <div className="pb-24">
        <ChapterEndColophon
          hasPrev={hasPrevChapter}
          hasNext={hasNextChapter}
          nextChapterTitle={nextChapterTitle}
          onPrevChapter={() => onPrevChapter("start")}
          onNextChapter={onNextChapter}
        />
      </div>
    ),
    [hasPrevChapter, hasNextChapter, nextChapterTitle, onPrevChapter, onNextChapter],
  );

  const columnStyle: React.CSSProperties = React.useMemo(
    () => ({
      maxWidth: `${contentWidth}em`,
      fontFamily: fontCss,
      fontSize: `${fontSize}px`,
      lineHeight,
    }),
    [contentWidth, fontCss, fontSize, lineHeight],
  );

  const virtuosoContext = React.useMemo<ScrollVirtuosoContext>(
    () => ({
      header: chapterHeader,
      footer: chapterFooter,
      columnStyle,
    }),
    [chapterHeader, chapterFooter, columnStyle],
  );

  if (isComic) {
    return (
      <div
        ref={(node) => {
          scrollerRef.current = node;
        }}
        onScroll={reportScrollMetrics}
        onPointerDown={handlePointerDown}
        onClick={handleCanvasClick}
        className="h-full w-full overflow-y-auto px-2 sm:px-6"
      >
        <div className="mx-auto max-w-3xl">
          <div className="px-3">{chapterHeader}</div>
          <div className="space-y-2">
            {parsed.images!.map((imgUrl, idx) => (
              <figure
                key={`${imgUrl}-${idx}`}
                data-para-index={idx}
                className="overflow-hidden rounded-lg bg-subtle/60"
              >
                <img
                  src={imgUrl}
                  alt={`第 ${idx + 1} 页`}
                  loading={idx < 3 ? "eager" : "lazy"}
                  decoding="async"
                  className="mx-auto block w-full object-contain"
                />
              </figure>
            ))}
          </div>
          {chapterFooter}
        </div>
      </div>
    );
  }

  if (useVirtual) {
    return (
      <div onPointerDown={handlePointerDown} onClick={handleCanvasClick} className="h-full w-full">
        <Virtuoso
          ref={virtuosoRef}
          scrollerRef={(el) => {
            scrollerRef.current = el instanceof HTMLDivElement ? el : null;
          }}
          style={{ height: "100%" }}
          totalCount={itemCount}
          overscan={600}
          context={virtuosoContext}
          onScroll={reportScrollMetrics}
          rangeChanged={(range) => {
            lastParaRef.current = range.startIndex;
            reportScrollMetrics();
          }}
          components={VIRTUOSO_COMPONENTS}
          itemContent={(idx) => (
            <div
              style={columnStyle}
              className="mx-auto w-full px-4 text-justify tracking-[0.015em] text-ink antialiased sm:px-6"
            >
              {renderParagraphRow(idx)}
            </div>
          )}
        />
      </div>
    );
  }

  return (
    <div
      ref={(node) => {
        scrollerRef.current = node;
      }}
      onScroll={reportScrollMetrics}
      onPointerDown={handlePointerDown}
      onClick={handleCanvasClick}
      className="h-full w-full overflow-y-auto"
    >
      <article
        style={columnStyle}
        className="mx-auto w-full px-4 text-justify tracking-[0.015em] text-ink antialiased sm:px-6"
      >
        {chapterHeader}
        {Array.from({ length: itemCount }, (_, idx) => renderParagraphRow(idx))}
        {chapterFooter}
      </article>
    </div>
  );
}
