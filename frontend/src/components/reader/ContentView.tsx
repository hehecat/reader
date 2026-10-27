import { CircleAlert, FileQuestion, RotateCw } from "lucide-react";
import * as React from "react";
import {
  Virtuoso,
  type Components,
  type ContextProp,
  type ListRange,
  type VirtuosoHandle,
} from "react-virtuoso";

import { ChapterEndBlock, type ChapterEndBlockProps } from "@/components/reader/ChapterEndBlock";
import { injectAnnotationMarks } from "@/components/reader/annotation-marks";
import { Button, EmptyState, Spinner } from "@/components/ui";
import type { ReaderParagraph } from "@/hooks/useChapterContent";
import { humanizeError } from "@/lib/errors";
import { NO_ANNOTATIONS, type Annotation } from "@/stores/annotations-store";
import { useReaderUIStore } from "@/stores/reader-ui-store";
import { useSettingsStore, type ReadMode } from "@/stores/settings-store";

/** 虚拟列表阈值: 超过该段落数启用 Virtuoso, 短章节全量渲染避免虚拟化开销 */
export const VIRTUALIZATION_THRESHOLD = 500;

/** 批注抽屉的跳转请求: 滚动到目标段落并闪烁; nonce 区分对同一段落的连续两次跳转 */
export interface FocusRequest {
  paraIndex: number;
  nonce: number;
}

/** 点击判定: 位移超过该像素视为滚动/拖拽, 不触发翻页 */
const TAP_MOVE_LIMIT = 10;
/** 点击判定: 按住超过该毫秒视为长按, 不触发翻页 */
const TAP_TIME_LIMIT = 500;

type TapZone = "prev" | "next" | "none";

/** 按 pos 定位恢复条目: 第一个 pos >= restorePos 的段落; 非有限值 (章末标记) 返回最后一条 */
export function findRestoreItemIndex(items: ReaderParagraph[], restorePos: number): number {
  if (items.length === 0) {
    return 0;
  }
  if (!Number.isFinite(restorePos)) {
    return items.length - 1;
  }
  if (restorePos <= 0) {
    return 0;
  }
  for (let i = 0; i < items.length; i += 1) {
    const item = items[i];
    if (item !== undefined && item.pos >= restorePos) {
      return i;
    }
  }
  return items.length - 1;
}

/**
 * 点击分区 (翻页模式): 左右边缘 15% 与上下 1/3 翻上/下页, 中央不做事 ——
 * 顶栏/底栏常显, 已经没有需要点出来的浮层. 拖拽滚动/框选文字/点在控件上不触发.
 */
function useTapZone(onTap: (zone: TapZone) => void) {
  const downRef = React.useRef<{ x: number; y: number; time: number } | null>(null);
  const tapRef = React.useRef(onTap);
  React.useEffect(() => {
    tapRef.current = onTap;
  });

  const onPointerDown = React.useCallback((event: React.PointerEvent<HTMLElement>) => {
    downRef.current = { x: event.clientX, y: event.clientY, time: Date.now() };
  }, []);

  const onPointerUp = React.useCallback(
    (event: React.PointerEvent<HTMLElement>) => {
      const down = downRef.current;
      downRef.current = null;
      if (down === null) {
        return;
      }
      if (event.pointerType === "mouse" && event.button !== 0) {
        return;
      }
      if (Math.hypot(event.clientX - down.x, event.clientY - down.y) > TAP_MOVE_LIMIT) {
        return;
      }
      if (Date.now() - down.time > TAP_TIME_LIMIT) {
        return;
      }
      const selection = window.getSelection();
      if (selection !== null && !selection.isCollapsed) {
        return;
      }
      // 章末块里有输入框和按钮: 落在控件上的点击交给控件自己, 不当翻页手势
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        target.closest("button, input, textarea, select, a, [role='slider']") !== null
      ) {
        return;
      }
      const rect = event.currentTarget.getBoundingClientRect();
      const x = event.clientX - rect.left;
      const y = event.clientY - rect.top;
      let zone: TapZone = "none";
      if (x < rect.width * 0.15) {
        zone = "prev";
      } else if (x > rect.width * 0.85) {
        zone = "next";
      } else if (y < rect.height / 3) {
        zone = "prev";
      } else if (y > (rect.height * 2) / 3) {
        zone = "next";
      }
      tapRef.current(zone);
    },
    [],
  );

  return { onPointerDown, onPointerUp };
}

/** ←/→ 快捷键: 滚动模式切章, 翻页模式翻页 (抽屉打开或表单聚焦时不响应) */
function useArrowKeys(onArrow: (delta: number) => void) {
  const handlerRef = React.useRef(onArrow);
  React.useEffect(() => {
    handlerRef.current = onArrow;
  });
  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) {
        return;
      }
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
        return;
      }
      const target = event.target;
      if (target instanceof HTMLElement) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable) {
          return;
        }
      }
      const ui = useReaderUIStore.getState();
      if (ui.tocOpen || ui.settingsOpen) {
        return;
      }
      event.preventDefault();
      handlerRef.current(event.key === "ArrowLeft" ? -1 : 1);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}

/**
 * 阅读器加载态: 转圈 + 可见文字.
 * 只挂在 aria-label 上的话, 明眼读者只能盯着一个没有解释的转圈, 分不清是在加载还是卡死了.
 */
export function ReaderLoading({ label = "正在加载章节…" }: { label?: string }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 text-center text-muted-foreground">
      <Spinner size="lg" label={label} />
      {/* 文案已由 Spinner 的 role=status 播报, 这一行只给眼睛看 */}
      <p aria-hidden className="text-sm">
        {label}
      </p>
    </div>
  );
}

/* ---------- 段落渲染 ---------- */

const paragraphBlockPadding: React.CSSProperties = {
  paddingTop: "calc(var(--reader-paragraph-gap) / 2)",
  paddingBottom: "calc(var(--reader-paragraph-gap) / 2)",
};

/**
 * 段落渲染: memo 化 —— 顶栏进度条随滚动逐帧更新, 不该带着整章段落一起重渲染.
 * 章头 (title) 是「书名 · 作者」小字 + 衬线章节标题, 字号随正文字号缩放.
 * 划选批注: 正文段落带 data-para-index (段落序号, 选区工具条据此定位),
 * 有批注时按偏移把 <mark> 注进 HTML 再交给 dangerouslySetInnerHTML.
 */
const ParagraphItem = React.memo(function ParagraphItem({
  item,
  indent,
  bookName,
  bookAuthor,
  annotations,
}: {
  item: ReaderParagraph;
  indent: boolean;
  bookName: string;
  bookAuthor: string;
  /** 落在本段的批注 (页面层按 paraIndex 分好组; 无批注时是稳定空数组) */
  annotations: Annotation[];
}) {
  // 无批注直接用原串 (零开销); 有批注时切分文本节点包 <mark>, 文本内容不变, 偏移下次仍然成立
  const annotatedHtml = React.useMemo(
    () => (annotations.length === 0 ? item.text : injectAnnotationMarks(item.text, annotations)),
    [item.text, annotations],
  );

  switch (item.type) {
    case "title":
      return (
        <header data-pos={0} className="break-inside-avoid px-5 pt-10 pb-8 break-after-avoid">
          <p className="text-xs text-muted-foreground">
            {bookName} · {bookAuthor}
          </p>
          <h2
            className="mt-2 font-display font-semibold text-balance"
            style={{ fontSize: "calc(var(--reader-font-size) * 1.35)" }}
          >
            {item.text}
          </h2>
        </header>
      );
    case "volumeTitle":
      return (
        <h2
          data-pos={0}
          className="break-inside-avoid px-5 pt-16 pb-3 text-center font-display font-semibold"
          style={{ fontSize: "calc(var(--reader-font-size) * 1.3)" }}
        >
          {item.text}
        </h2>
      );
    case "volumeTag":
      return (
        <p
          className="break-inside-avoid px-5 pb-10 text-center text-sm text-muted-foreground"
          dangerouslySetInnerHTML={{ __html: item.text }}
        />
      );
    case "image":
      return (
        <div
          data-pos={item.pos}
          className="break-inside-avoid px-5 [&_img]:mx-auto [&_img]:block [&_img]:h-auto [&_img]:max-w-full"
          style={paragraphBlockPadding}
          dangerouslySetInnerHTML={{ __html: item.text }}
        />
      );
    case "paragraph":
      return (
        <p
          data-pos={item.pos}
          data-para-index={item.key}
          className="px-5 text-pretty text-foreground/90"
          style={{ ...paragraphBlockPadding, textIndent: indent ? "2em" : undefined }}
          dangerouslySetInnerHTML={{ __html: annotatedHtml }}
        />
      );
  }
});

/** 跳转批注时的段落闪烁: accent 底色 2s 淡出 (WAAPI 一次性动画, 不改类名与内联样式) */
function flashParagraph(element: HTMLElement): void {
  element.animate(
    [
      { backgroundColor: "color-mix(in oklch, var(--accent) 30%, transparent)" },
      { backgroundColor: "transparent" },
    ],
    { duration: 2000, easing: "ease-out" },
  );
}

/* ---------- 滚动模式 ---------- */

/** Virtuoso 的 Footer 只能从 context 取数据; 组件与 components 对象都要模块级稳定, 否则每次渲染重挂 */
function ChapterEndFooter({ context }: ContextProp<ChapterEndBlockProps>) {
  return <ChapterEndBlock {...context} />;
}

const VIRTUOSO_COMPONENTS: Components<ReaderParagraph, ChapterEndBlockProps> = {
  Footer: ChapterEndFooter,
};

interface ListProps {
  items: ReaderParagraph[];
  indent: boolean;
  /** 章头小字: 书名 · 作者 */
  bookName: string;
  bookAuthor: string;
  restorePos: number;
  onPosChange: (pos: number) => void;
  /** 本章阅读比例 (0-1): 滚动模式取滚动进度, 翻页模式取页码占比 */
  onReadRatio: (ratio: number) => void;
  /** 本章批注按段落索引分组: 引用只在批注变化时更新, 保住 ParagraphItem 的 memo */
  annotationsByPara: Map<number, Annotation[]>;
  /** 批注抽屉的跳转请求 */
  focusRequest: FocusRequest | null;
  /** 跳转处理完 (滚动 + 闪烁已发起) 回调, 页面清掉请求 */
  onFocusHandled: () => void;
}

/** 滚动列表 = 段落列表 + 章末块 */
interface ScrollListProps extends ListProps {
  /** 章末块的数据与回调 (页面层 memo 好, 直接当 Virtuoso 的 context 用) */
  endBlock: ChapterEndBlockProps;
  /** 把滚动容器交给上层: 自动滚动要直接驱动它 */
  onScrollerReady: (element: HTMLElement | null) => void;
}

/** 长章节: Virtuoso 虚拟列表, initialTopMostItemIndex 恢复进度, rangeChanged 上报当前段 pos */
function VirtualScrollList({
  items,
  indent,
  bookName,
  bookAuthor,
  restorePos,
  onPosChange,
  onReadRatio,
  annotationsByPara,
  focusRequest,
  onFocusHandled,
  endBlock,
  onScrollerReady,
}: ScrollListProps) {
  const restoreIndex = React.useMemo(
    () => findRestoreItemIndex(items, restorePos),
    [items, restorePos],
  );
  const scrollerRef = React.useRef<HTMLElement | null>(null);
  const frameRef = React.useRef(0);
  const onPosChangeRef = React.useRef(onPosChange);
  const onReadRatioRef = React.useRef(onReadRatio);
  React.useEffect(() => {
    onPosChangeRef.current = onPosChange;
    onReadRatioRef.current = onReadRatio;
  });

  /** 本章阅读比例 = 已滚过高度占可滚动高度的比 (含 Footer 里的章末块) */
  const reportRatio = React.useCallback(() => {
    const el = scrollerRef.current;
    if (el === null) {
      return;
    }
    const max = el.scrollHeight - el.clientHeight;
    onReadRatioRef.current(max > 0 ? Math.min(1, Math.max(0, el.scrollTop / max)) : 0);
  }, []);

  const handleRangeChanged = React.useCallback(
    (range: ListRange) => {
      // 可视区间变化也意味着进度变化 (含 Virtuoso 应用 initialTopMostItemIndex 的那一次)
      reportRatio();
      for (let i = range.startIndex; i <= range.endIndex && i < items.length; i += 1) {
        const item = items[i];
        if (item !== undefined && item.pos >= 0) {
          onPosChangeRef.current(item.pos);
          return;
        }
      }
    },
    [items, reportRatio],
  );

  // Virtuoso 自己持有滚动容器: 拿到它才算得出比例, 自动滚动也才能驱动它
  const [scroller, setScroller] = React.useState<HTMLElement | null>(null);
  const handleScrollerRef = React.useCallback(
    (element: HTMLElement | Window | null) => {
      const next = element instanceof HTMLElement ? element : null;
      scrollerRef.current = next;
      onScrollerReady(next);
      setScroller(next);
    },
    [onScrollerReady],
  );

  React.useEffect(() => {
    if (scroller === null) {
      return;
    }
    const onScroll = () => {
      if (frameRef.current !== 0) {
        return;
      }
      frameRef.current = requestAnimationFrame(() => {
        frameRef.current = 0;
        reportRatio();
      });
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    reportRatio();
    // Virtuoso 应用初始位置/图片撑开高度后再校正一次
    const timer = window.setTimeout(reportRatio, 250);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      window.clearTimeout(timer);
      if (frameRef.current !== 0) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = 0;
      }
    };
  }, [scroller, reportRatio]);

  const virtuosoRef = React.useRef<VirtuosoHandle>(null);

  // 批注抽屉跳转: scrollToIndex 把目标段落滚到视口中部, 等虚拟化挂载完成 (350ms) 再闪烁该段
  React.useEffect(() => {
    if (focusRequest === null) {
      return;
    }
    virtuosoRef.current?.scrollToIndex({ index: focusRequest.paraIndex, align: "center" });
    const timer = window.setTimeout(() => {
      const scroller = scrollerRef.current;
      const target =
        scroller?.querySelector<HTMLElement>(
          `p[data-para-index="${focusRequest.paraIndex}"]`,
        ) ?? null;
      if (target !== null) {
        flashParagraph(target);
      }
      onFocusHandled();
    }, 350);
    return () => window.clearTimeout(timer);
  }, [focusRequest, onFocusHandled]);

  return (
    <div className="mx-auto h-full" style={{ maxWidth: "var(--reader-content-width, 42em)" }}>
      <Virtuoso
        ref={virtuosoRef}
        data={items}
        context={endBlock}
        components={VIRTUOSO_COMPONENTS}
        initialTopMostItemIndex={restoreIndex}
        itemContent={(_position, item) => (
          <ParagraphItem
            item={item}
            indent={indent}
            bookName={bookName}
            bookAuthor={bookAuthor}
            annotations={annotationsByPara.get(item.key) ?? NO_ANNOTATIONS}
          />
        )}
        rangeChanged={handleRangeChanged}
        scrollerRef={handleScrollerRef}
        increaseViewportBy={{ top: 400, bottom: 800 }}
        style={{ height: "100%" }}
      />
    </div>
  );
}

/** 短章节 (<= VIRTUALIZATION_THRESHOLD 段): 全量渲染 + 原生滚动, data-pos 扫描定位 */
function FullScrollList({
  items,
  indent,
  bookName,
  bookAuthor,
  restorePos,
  onPosChange,
  onReadRatio,
  annotationsByPara,
  focusRequest,
  onFocusHandled,
  endBlock,
  onScrollerReady,
}: ScrollListProps) {
  const scrollRef = React.useRef<HTMLDivElement | null>(null);
  const frameRef = React.useRef(0);
  const onReadRatioRef = React.useRef(onReadRatio);
  React.useEffect(() => {
    onReadRatioRef.current = onReadRatio;
  });

  /** 本章阅读比例 = 已滚过高度占可滚动高度的比 */
  const reportRatio = React.useCallback(() => {
    const el = scrollRef.current;
    if (el === null) {
      return;
    }
    const max = el.scrollHeight - el.clientHeight;
    onReadRatioRef.current(max > 0 ? Math.min(1, Math.max(0, el.scrollTop / max)) : 0);
  }, []);

  // 滚动容器交给上层: 自动滚动直接驱动它
  const attachScroller = React.useCallback(
    (element: HTMLDivElement | null) => {
      scrollRef.current = element;
      onScrollerReady(element);
    },
    [onScrollerReady],
  );

  // 进度恢复: 滚动到第一个 data-pos >= restorePos 的段落; 图片首帧渲染后若用户未滚动再校正一次
  React.useEffect(() => {
    const el = scrollRef.current;
    if (el === null || items.length === 0) {
      return;
    }
    let expectedTop: number | null = null;
    const apply = () => {
      const nodes = el.querySelectorAll<HTMLElement>("[data-pos]");
      if (nodes.length === 0) {
        return;
      }
      let target: HTMLElement | null = null;
      if (!Number.isFinite(restorePos)) {
        target = nodes[nodes.length - 1] ?? null;
      } else {
        for (const node of nodes) {
          if (Number(node.dataset.pos) >= restorePos) {
            target = node;
            break;
          }
        }
      }
      if (target === null) {
        return;
      }
      el.scrollTop += target.getBoundingClientRect().top - el.getBoundingClientRect().top;
      expectedTop = el.scrollTop;
    };
    apply();
    const timer = window.setTimeout(() => {
      if (expectedTop !== null && Math.abs(el.scrollTop - expectedTop) < 8) {
        apply();
      }
    }, 250);
    return () => window.clearTimeout(timer);
  }, [items, restorePos]);

  // 批注抽屉跳转: 目标段落滚到容器中部并闪烁 (rAF 排在进度恢复之后; 恢复的 250ms 校正见滚动位置已变会自动让路)
  React.useEffect(() => {
    if (focusRequest === null) {
      return;
    }
    const frame = requestAnimationFrame(() => {
      const el = scrollRef.current;
      if (el !== null) {
        const target = el.querySelector<HTMLElement>(
          `p[data-para-index="${focusRequest.paraIndex}"]`,
        );
        if (target !== null) {
          el.scrollTop +=
            target.getBoundingClientRect().top -
            el.getBoundingClientRect().top -
            (el.clientHeight - target.clientHeight) / 2;
          flashParagraph(target);
        }
      }
      onFocusHandled();
    });
    return () => cancelAnimationFrame(frame);
  }, [focusRequest, onFocusHandled]);

  // 滚动时取视口顶部第一个段落的 pos + 本章阅读比例 (rAF 节流)
  const handleScroll = React.useCallback(() => {
    if (frameRef.current !== 0) {
      return;
    }
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = 0;
      const el = scrollRef.current;
      if (el === null) {
        return;
      }
      reportRatio();
      const containerTop = el.getBoundingClientRect().top;
      const nodes = el.querySelectorAll<HTMLElement>("[data-pos]");
      for (const node of nodes) {
        if (node.getBoundingClientRect().bottom > containerTop + 1) {
          const pos = Number(node.dataset.pos);
          if (pos >= 0) {
            onPosChange(pos);
          }
          break;
        }
      }
    });
  }, [onPosChange, reportRatio]);

  React.useEffect(
    () => () => {
      if (frameRef.current !== 0) {
        cancelAnimationFrame(frameRef.current);
      }
    },
    [],
  );

  // 进章先报一次比例: 恢复到上次阅读位置后进度条要立刻对上, 不能等读者滚一下才动
  React.useEffect(() => {
    const frame = requestAnimationFrame(reportRatio);
    return () => cancelAnimationFrame(frame);
  }, [items, restorePos, reportRatio]);

  return (
    <div
      ref={attachScroller}
      onScroll={handleScroll}
      className="h-full overflow-y-auto overscroll-contain"
    >
      <div className="mx-auto" style={{ maxWidth: "var(--reader-content-width, 42em)" }}>
        {items.map((item) => (
          <ParagraphItem
            key={item.key}
            item={item}
            indent={indent}
            bookName={bookName}
            bookAuthor={bookAuthor}
            annotations={annotationsByPara.get(item.key) ?? NO_ANNOTATIONS}
          />
        ))}
        <ChapterEndBlock {...endBlock} />
      </div>
    </div>
  );
}

interface ScrollModeProps extends ScrollListProps {
  onPrevChapter: () => void;
  onNextChapter: () => void;
  /** 点正文: 切换上下工具条 */
  onToggleChrome?: () => void;
}

function ScrollMode({
  items,
  indent,
  bookName,
  bookAuthor,
  restorePos,
  onPosChange,
  onReadRatio,
  annotationsByPara,
  focusRequest,
  onFocusHandled,
  endBlock,
  onScrollerReady,
  onPrevChapter,
  onNextChapter,
  onToggleChrome,
}: ScrollModeProps) {
  const handleArrow = React.useCallback(
    (delta: number) => {
      if (delta < 0) {
        onPrevChapter();
      } else {
        onNextChapter();
      }
    },
    [onPrevChapter, onNextChapter],
  );
  useArrowKeys(handleArrow);

  // 滚动模式没有边缘翻页区, 任意有效 tap (非控件/无选区/非拖拽) 都切换上下工具条
  const tapProps = useTapZone(() => onToggleChrome?.());

  const listProps: ScrollListProps = {
    items,
    indent,
    bookName,
    bookAuthor,
    restorePos,
    onPosChange,
    onReadRatio,
    annotationsByPara,
    focusRequest,
    onFocusHandled,
    endBlock,
    onScrollerReady,
  };

  return (
    <div className="h-full" {...tapProps}>
      {items.length > VIRTUALIZATION_THRESHOLD ? (
        <VirtualScrollList {...listProps} />
      ) : (
        <FullScrollList {...listProps} />
      )}
    </div>
  );
}

/* ---------- 翻页模式 ---------- */

interface PagedModeProps extends ListProps {
  /** 章末块: 翻页模式把它单独当作最后一页 */
  endBlock: ChapterEndBlockProps;
  onPrevChapter: () => void;
  onNextChapter: () => void;
  /** 点正文中央: 切换上下工具条 (左右/上下边缘留给翻页) */
  onToggleChrome?: () => void;
}

/**
 * 翻页模式: CSS 多列分屏. 内容容器固定为视口高, column-width 为视口宽,
 * 溢出的列即后续「页」, translateX 平移翻页; 页内禁止滚动.
 * 正文列之后另有一页章末块 (章末标记/上下章导航, 自己滚动),
 * 翻过它才进入下一章; 第一页点「上一章」回到上一章末尾.
 */
function PagedMode({
  items,
  indent,
  bookName,
  bookAuthor,
  restorePos,
  onPosChange,
  onReadRatio,
  annotationsByPara,
  focusRequest,
  onFocusHandled,
  endBlock,
  onPrevChapter,
  onNextChapter,
  onToggleChrome,
}: PagedModeProps) {
  const viewportRef = React.useRef<HTMLDivElement>(null);
  const innerRef = React.useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = React.useState({ width: 0, height: 0 });
  const [page, setPage] = React.useState(0);
  /** 正文列数; 章末块另占一页, 总页数 = 列数 + 1 */
  const [columnCount, setColumnCount] = React.useState(1);
  const totalPages = columnCount + 1;
  const restoreRef = React.useRef(restorePos);
  const onPosChangeRef = React.useRef(onPosChange);
  const onReadRatioRef = React.useRef(onReadRatio);
  React.useEffect(() => {
    onPosChangeRef.current = onPosChange;
    onReadRatioRef.current = onReadRatio;
  });

  // 排版设置变化会改变分页结果, 需重新测量
  const fontSize = useSettingsStore((state) => state.fontSize);
  const lineHeight = useSettingsStore((state) => state.lineHeight);
  const paragraphGap = useSettingsStore((state) => state.paragraphGap);
  const fontFamily = useSettingsStore((state) => state.fontFamily);
  const contentWidth = useSettingsStore((state) => state.contentWidth);

  React.useEffect(() => {
    const el = viewportRef.current;
    if (el === null) {
      return;
    }
    const measure = () => {
      const width = el.clientWidth;
      const height = el.clientHeight;
      setViewport((prev) => (prev.width === width && prev.height === height ? prev : { width, height }));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // 分页测量 + 进度恢复. 放进 rAF: 等父级 useReaderTheme 把新的 --reader-* 变量写到 <html> 后再量
  React.useLayoutEffect(() => {
    const frame = requestAnimationFrame(() => {
      const inner = innerRef.current;
      if (inner === null || viewport.width <= 0 || viewport.height <= 0) {
        return;
      }
      const count = Math.max(1, Math.ceil(inner.scrollWidth / viewport.width - 0.02));
      setColumnCount(count);
      const target = restoreRef.current;
      restoreRef.current = 0;
      if (target > 0) {
        if (!Number.isFinite(target)) {
          setPage(count - 1);
          return;
        }
        const innerLeft = inner.getBoundingClientRect().left;
        const nodes = inner.querySelectorAll<HTMLElement>("[data-pos]");
        for (const node of nodes) {
          if (Number(node.dataset.pos) < target) {
            continue;
          }
          const left = node.getBoundingClientRect().left - innerLeft;
          setPage(Math.min(count - 1, Math.max(0, Math.floor(left / viewport.width))));
          return;
        }
        setPage(count - 1);
        return;
      }
      setPage((current) => Math.min(current, count - 1));
    });
    return () => cancelAnimationFrame(frame);
  }, [viewport, items, indent, fontSize, lineHeight, paragraphGap, fontFamily, contentWidth]);

  // 批注抽屉跳转: 全部段落都在多列 DOM 里, 直接按布局位置算出目标页翻过去, 再闪烁该段
  React.useEffect(() => {
    if (focusRequest === null) {
      return;
    }
    const inner = innerRef.current;
    if (inner !== null && viewport.width > 0) {
      const target = inner.querySelector<HTMLElement>(
        `p[data-para-index="${focusRequest.paraIndex}"]`,
      );
      if (target !== null) {
        const left = target.getBoundingClientRect().left - inner.getBoundingClientRect().left;
        setPage(Math.max(0, Math.floor(left / viewport.width)));
        window.setTimeout(() => flashParagraph(target), 80);
      }
    }
    onFocusHandled();
  }, [focusRequest, viewport.width, onFocusHandled]);

  const goPage = React.useCallback(
    (next: number) => {
      if (next < 0) {
        onPrevChapter();
        return;
      }
      if (next >= totalPages) {
        onNextChapter();
        return;
      }
      setPage(next);
    },
    [totalPages, onPrevChapter, onNextChapter],
  );

  // ←/→ 给的是「相对当前页」的步长, 而 goPage 收绝对页码:
  // 直接把 delta 喂进去会让 → 永远停在第 2 页、← 永远整章跳. 先落到目标页, 再由 goPage 判章内/跨章边界.
  const stepPage = React.useCallback((delta: number) => goPage(page + delta), [goPage, page]);

  useArrowKeys(stepPage);

  const handleTap = React.useCallback(
    (zone: TapZone) => {
      if (zone === "prev") {
        goPage(page - 1);
      } else if (zone === "next") {
        goPage(page + 1);
      } else {
        // 中央区域: 切换上下工具条显隐
        onToggleChrome?.();
      }
    },
    [goPage, page, onToggleChrome],
  );
  const tapProps = useTapZone(handleTap);

  // 翻页后上报当前页首段 pos (列按 DOM 顺序排布, 相对 left 即布局位置); 章末页没有正文段, 保持上一次
  React.useEffect(() => {
    if (page >= columnCount) {
      return;
    }
    const inner = innerRef.current;
    if (inner === null || viewport.width <= 0) {
      return;
    }
    const innerLeft = inner.getBoundingClientRect().left;
    const windowStart = page * viewport.width - 1;
    const windowEnd = (page + 1) * viewport.width;
    const nodes = inner.querySelectorAll<HTMLElement>("[data-pos]");
    for (const node of nodes) {
      const left = node.getBoundingClientRect().left - innerLeft;
      if (left < windowStart) {
        continue;
      }
      if (left >= windowEnd) {
        break;
      }
      const pos = Number(node.dataset.pos);
      if (pos >= 0) {
        onPosChangeRef.current(pos);
      }
      return;
    }
  }, [page, columnCount, viewport.width]);

  // 顶栏进度条: 翻页模式的比例 = 当前页占总页数
  React.useEffect(() => {
    onReadRatioRef.current((page + 1) / totalPages);
  }, [page, totalPages]);

  const translateX = viewport.width > 0 ? -page * viewport.width : 0;
  const atEndPage = page >= columnCount;

  return (
    <div className="relative h-full overflow-hidden" {...tapProps}>
      <div
        ref={viewportRef}
        className="mx-auto h-full w-full overflow-hidden"
        style={{ maxWidth: "var(--reader-content-width, 42em)" }}
      >
        <div
          ref={innerRef}
          className="will-change-transform"
          style={{
            height: viewport.height > 0 ? viewport.height : undefined,
            columnWidth: viewport.width > 0 ? viewport.width : undefined,
            columnGap: 0,
            columnFill: "auto",
            transform: `translate3d(${translateX}px, 0, 0)`,
            visibility: viewport.width > 0 ? "visible" : "hidden",
          }}
        >
          {items.map((item) => (
            <ParagraphItem
              key={item.key}
              item={item}
              indent={indent}
              bookName={bookName}
              bookAuthor={bookAuthor}
              annotations={annotationsByPara.get(item.key) ?? NO_ANNOTATIONS}
            />
          ))}
        </div>
      </div>

      {/* 章末页: 不透明的独立滚动层盖住已被平移出视口的正文列 */}
      {atEndPage ? (
        <div className="absolute inset-0 overflow-y-auto overscroll-contain bg-background">
          <div
            className="mx-auto w-full"
            style={{ maxWidth: "var(--reader-content-width, 42em)" }}
          >
            <ChapterEndBlock {...endBlock} />
          </div>
        </div>
      ) : null}

      <div className="ui-safe-b pointer-events-none absolute inset-x-0 bottom-0 pb-2 text-center text-xs text-muted-foreground/70 tabular-nums">
        {page + 1} / {totalPages}
      </div>
    </div>
  );
}

/**
 * 自动滚动: rAF 匀速推动正文滚动容器, 速度取阅读设置的 autoScrollSpeed (px/s).
 * 读者手动介入 (滚轮/拖动/键盘滚动) 或滚到底时停下, 用 onStop 通知上层关掉开关.
 * 不监听 touchstart: 轻点顶栏的「停止自动滚动」也会先来一次 touchstart,
 * 那样开关会被停掉又被点开; 触摸拖动本身会产生 scroll 事件, 照样能停.
 */
function useAutoScroll(
  scroller: HTMLElement | null,
  enabled: boolean,
  speed: number,
  onStop: () => void,
): void {
  const stopRef = React.useRef(onStop);
  React.useEffect(() => {
    stopRef.current = onStop;
  });

  React.useEffect(() => {
    if (!enabled || scroller === null) {
      return;
    }
    let frame = 0;
    let last = performance.now();
    let carried = 0;
    /** 自己写 scrollTop 的次数: 每次写入回一个 scroll 事件, 多出来的才算读者手动滚动 */
    let driven = 0;
    let stopped = false;

    const stop = () => {
      if (stopped) {
        return;
      }
      stopped = true;
      stopRef.current();
    };
    const onScroll = () => {
      if (driven > 0) {
        driven -= 1;
        return;
      }
      stop();
    };
    const tick = (now: number) => {
      frame = 0;
      if (stopped) {
        return;
      }
      carried += ((now - last) / 1000) * speed;
      last = now;
      const step = Math.floor(carried);
      if (step >= 1) {
        carried -= step;
        const before = scroller.scrollTop;
        driven += 1;
        scroller.scrollTop = before + step;
        if (scroller.scrollTop === before) {
          // 推不动了 = 到底
          driven -= 1;
          stop();
          return;
        }
      }
      frame = requestAnimationFrame(tick);
    };

    scroller.addEventListener("scroll", onScroll, { passive: true });
    scroller.addEventListener("wheel", stop, { passive: true });
    frame = requestAnimationFrame(tick);
    return () => {
      stopped = true;
      if (frame !== 0) {
        cancelAnimationFrame(frame);
      }
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("wheel", stop);
    };
  }, [enabled, scroller, speed]);
}

/* ---------- 正文容器 ---------- */

export interface ContentViewProps {
  items: ReaderParagraph[];
  isPending: boolean;
  isError: boolean;
  errorMessage: string;
  isEmpty: boolean;
  onRetry: () => void;
  readMode: ReadMode;
  /** 章头小字: 书名 · 作者 */
  bookName: string;
  bookAuthor: string;
  /** 进入本章要恢复的位置 (END_OF_CHAPTER = 章末) */
  restorePos: number;
  /** 滚动/翻页时上报当前段 pos */
  onPosChange: (pos: number) => void;
  /** 本章阅读比例 (0-1), 驱动顶栏进度条 */
  onReadRatio: (ratio: number) => void;
  onPrevChapter: () => void;
  onNextChapter: () => void;
  /** 本章的划选批注 (页面已按章节过滤, 引用稳定) */
  annotations: Annotation[];
  /** 批注抽屉的跳转请求 (null = 无) */
  focusRequest: FocusRequest | null;
  /** 跳转处理完回调, 页面清掉请求 */
  onFocusHandled: () => void;
  /** 章末块: 章末标记 + 上下章导航 */
  endBlock: ChapterEndBlockProps;
  /** 自动滚动开关 (只对滚动模式生效) */
  autoScroll: boolean;
  /** 自动滚动到底或被打断时通知上层关掉开关 */
  onAutoScrollStop: () => void;
  /** 只在「未配置书源」这类重试无用的错误时给: 错误态多出「去书源页」的出路 */
  onGoToSources?: () => void;
  /** 空正文时的换源救出口 (ReaderPage 弹候选列表) */
  onSwitchSource?: () => void;
  /** 绕过服务端正文缓存重新抓取本章 (与「重试」= 重新查询区分) */
  onReload?: () => void;
  /** 重新抓取进行中 (按钮转圈并防重复点击) */
  isReloading?: boolean;
  /** 点击正文中央/空白 (非控件、无选区、非拖拽): 由页面切换上下工具条显隐 */
  onToggleChrome?: () => void;
}

/** memo: 顶栏进度条每滚一帧就可能更新一次百分比, 正文子树不必跟着重渲染 */
export const ContentView = React.memo(function ContentView({
  items,
  isPending,
  isError,
  errorMessage,
  isEmpty,
  onRetry,
  readMode,
  bookName,
  bookAuthor,
  restorePos,
  onPosChange,
  onReadRatio,
  onPrevChapter,
  onNextChapter,
  annotations,
  focusRequest,
  onFocusHandled,
  endBlock,
  autoScroll,
  onAutoScrollStop,
  onGoToSources,
  onSwitchSource,
  onReload,
  isReloading = false,
  onToggleChrome,
}: ContentViewProps) {
  const indent = useSettingsStore((state) => state.indentParagraph);
  const autoScrollSpeed = useSettingsStore((state) => state.autoScrollSpeed);

  // 批注按段落索引分组: Map 引用只随 annotations 变化, ParagraphItem 的 memo 才守得住
  const annotationsByPara = React.useMemo(() => {
    const map = new Map<number, Annotation[]>();
    for (const annotation of annotations) {
      const list = map.get(annotation.paraIndex);
      if (list !== undefined) {
        list.push(annotation);
      } else {
        map.set(annotation.paraIndex, [annotation]);
      }
    }
    return map;
  }, [annotations]);

  // 自动滚动直接驱动正文的滚动容器 (Virtuoso 的 scroller 或原生滚动 div)
  const [scroller, setScroller] = React.useState<HTMLElement | null>(null);
  useAutoScroll(
    scroller,
    autoScroll && readMode === "scroll" && !isPending && !isError && items.length > 0,
    autoScrollSpeed,
    onAutoScrollStop,
  );

  // 滚动 ↔ 翻页切换不会重挂载 ContentView (只按章节 key 重挂):
  // 切换模式时从当前阅读位置续读, 而不是回到进入章节时的恢复点
  const posRef = React.useRef(0);
  const [prevMode, setPrevMode] = React.useState(readMode);
  const [switchPos, setSwitchPos] = React.useState<number | null>(null);
  if (prevMode !== readMode) {
    setPrevMode(readMode);
    setSwitchPos(posRef.current);
  }
  const effectiveRestorePos = switchPos ?? restorePos;

  const handlePosChange = React.useCallback(
    (pos: number) => {
      posRef.current = pos;
      onPosChange(pos);
    },
    [onPosChange],
  );

  let body: React.ReactNode;
  if (isPending) {
    body = (
      <div className="flex h-full items-center justify-center">
        <ReaderLoading />
      </div>
    );
  } else if (isError) {
    body = (
      <div className="flex h-full items-center justify-center">
        <EmptyState
          icon={<CircleAlert />}
          title="正文加载失败"
          description={humanizeError(errorMessage, "网络或服务器异常")}
          action={
            <>
              {onGoToSources === undefined ? null : (
                <Button onClick={onGoToSources}>去书源页</Button>
              )}
              <Button variant="secondary" onClick={onRetry}>
                重试
              </Button>
              {onReload === undefined ? null : (
                <Button variant="secondary" loading={isReloading} onClick={onReload}>
                  <RotateCw aria-hidden />
                  重新拉取本章
                </Button>
              )}
            </>
          }
        />
      </div>
    );
  } else if (isEmpty || items.length === 0) {
    body = (
      <div className="flex h-full items-center justify-center">
        <EmptyState
          icon={<FileQuestion />}
          title="本章暂无内容"
          description="书源可能尚未收录本章节, 可以重试或直接阅读下一章."
          action={
            <>
              <Button variant="secondary" onClick={onRetry}>
                重试
              </Button>
              {onReload === undefined ? null : (
                <Button variant="secondary" loading={isReloading} onClick={onReload}>
                  <RotateCw aria-hidden />
                  重新拉取本章
                </Button>
              )}
              {onSwitchSource === undefined ? null : (
                <Button variant="secondary" onClick={onSwitchSource}>
                  换源
                </Button>
              )}
              <Button onClick={onNextChapter}>下一章</Button>
            </>
          }
        />
      </div>
    );
  } else if (readMode === "page") {
    body = (
      <PagedMode
        items={items}
        indent={indent}
        bookName={bookName}
        bookAuthor={bookAuthor}
        restorePos={effectiveRestorePos}
        onPosChange={handlePosChange}
        onReadRatio={onReadRatio}
        annotationsByPara={annotationsByPara}
        focusRequest={focusRequest}
        onFocusHandled={onFocusHandled}
        endBlock={endBlock}
        onPrevChapter={onPrevChapter}
        onNextChapter={onNextChapter}
        onToggleChrome={onToggleChrome}
      />
    );
  } else {
    body = (
      <ScrollMode
        items={items}
        indent={indent}
        bookName={bookName}
        bookAuthor={bookAuthor}
        restorePos={effectiveRestorePos}
        onPosChange={handlePosChange}
        onReadRatio={onReadRatio}
        annotationsByPara={annotationsByPara}
        focusRequest={focusRequest}
        onFocusHandled={onFocusHandled}
        endBlock={endBlock}
        onScrollerReady={setScroller}
        onPrevChapter={onPrevChapter}
        onNextChapter={onNextChapter}
        onToggleChrome={onToggleChrome}
      />
    );
  }

  return (
    <div
      className="h-full text-foreground"
      style={{
        fontSize: "var(--reader-font-size, 18px)",
        lineHeight: "var(--reader-line-height, 1.8)",
        fontFamily: "var(--reader-font-family, inherit)",
      }}
    >
      {body}
    </div>
  );
});
