import { BookA, Sparkles, StickyNote, Trash2, Underline, X } from "lucide-react";
import * as React from "react";

import { AddPurifyDialog } from "@/components/reader/AddPurifyDialog";
import { ANNOTATION_COLOR_HEX, paragraphOffsetOf } from "@/components/reader/annotation-marks";
import {
  Badge,
  Button,
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Spinner,
  Textarea,
  cn,
} from "@/components/ui";
import { DICT_KIND_LABEL, dictLookup, type DictResult } from "@/services/dict";
import { buildPurifyDraft, type PurifyDraft } from "@/services/purify";
import {
  useAnnotationsStore,
  type Annotation,
  type AnnotationColor,
} from "@/stores/annotations-store";

/**
 * 划选工具条: 正文里划出一段文字 (须落在单一段落内) 后, 选区旁浮出
 * 高亮三色 / 下划线 / 笔记 / 添加净化 / 取消; 点已有批注浮出 改笔记 / 删除.
 * 触屏没有 mouseup 选区, 用 selectionchange 防抖兜底.
 * 批注只写本机 annotations-store (localStorage), 不经后端;
 * 「添加净化」则把整句存成服务端替换规则, 由渲染期净化立即生效.
 */

const COLOR_LABEL: Record<AnnotationColor, string> = {
  yellow: "黄色",
  green: "绿色",
  blue: "蓝色",
};

/** 工具条半宽估值: 用来把弹层夹在视口内 */
const TOOLBAR_HALF_WIDTH = 130;
/** 选区顶部离视口不足该值时, 工具条改放选区下方 */
const FLIP_BELOW_TOP = 110;

interface SelectionTarget {
  paraIndex: number;
  start: number;
  end: number;
  /** 选区原文 (截断到 200 字, 渲染前校验偏移是否仍对得上) */
  quote: string;
  /** 所在段落的纯文本: 「添加净化」按它把选区扩到整句 */
  paraText: string;
  rect: DOMRect;
}

type PopoverState =
  | { mode: "new"; target: SelectionTarget }
  | { mode: "existing"; annotation: Annotation; rect: DOMRect }
  | null;

type NoteDraft =
  | { mode: "new"; target: SelectionTarget }
  | { mode: "edit"; annotation: Annotation }
  | null;

/**
 * 浮层内屏蔽 ←/→: ContentView 的翻页快捷键先看 defaultPrevented,
 * 不拦的话弹窗开着时背后仍在翻页.
 */
function suppressArrowKeys(event: React.KeyboardEvent): void {
  if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
    return;
  }
  const target = event.target;
  if (target instanceof HTMLElement) {
    const tag = target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable) {
      return;
    }
  }
  event.preventDefault();
}

function ToolbarButton({
  label,
  onClick,
  danger = false,
  children,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={cn(
        "flex size-8 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-surface-muted hover:text-foreground",
        danger && "hover:text-danger",
      )}
    >
      {children}
    </button>
  );
}

export interface SelectionToolbarProps {
  /** 正文容器: 只处理容器内的划选与批注点击 */
  containerRef: React.RefObject<HTMLElement | null>;
  bookUrl: string;
  /** 当前章节索引: 写进新批注; 切章时工具条收起 */
  chapterIndex: number;
}

export function SelectionToolbar({ containerRef, bookUrl, chapterIndex }: SelectionToolbarProps) {
  const addAnnotation = useAnnotationsStore((state) => state.add);
  const updateAnnotation = useAnnotationsStore((state) => state.update);
  const removeAnnotation = useAnnotationsStore((state) => state.remove);

  const [popover, setPopover] = React.useState<PopoverState>(null);
  const [noteDraft, setNoteDraft] = React.useState<NoteDraft>(null);
  const [purifyDraft, setPurifyDraft] = React.useState<PurifyDraft | null>(null);
  const [dictState, setDictState] = React.useState<{
    word: string;
    result: DictResult | null;
    loading: boolean;
  } | null>(null);

  /** 选中查词: 取选区前 32 字, 导入期(building)轮询最长 60s */
  const lookupSelection = React.useCallback(async (raw: string) => {
    const word = raw.trim().slice(0, 32);
    if (word === "") {
      return;
    }
    setDictState({ word, result: null, loading: true });
    const started = Date.now();
    try {
      for (;;) {
        const result = await dictLookup(word);
        if (result.status !== "building" || Date.now() - started > 60_000) {
          setDictState({ word, result, loading: false });
          return;
        }
        await new Promise((resolve) => window.setTimeout(resolve, 2000));
      }
    } catch {
      setDictState({ word, result: null, loading: false });
    }
  }, []);
  /** 任一弹窗开着: 期间不重估选区、不因滚动收起工具条, ←/→ 也交给弹窗自己 */
  const dialogOpen = noteDraft !== null || purifyDraft !== null;
  const popoverRef = React.useRef<HTMLDivElement>(null);
  /** 最近一次 pointerdown 的类型: 触屏才启用 selectionchange 兜底 */
  const lastPointerType = React.useRef("mouse");

  /**
   * 选区 → 批注目标: 起止必须落在同一个带 data-para-index 的段落里;
   * 跨段/跨元素的选区直接放弃 (v1 只支持段内批注).
   * 偏移 = 段落 textContent 上的字符索引, 与 injectAnnotationMarks 同口径;
   * 反向框选由 Range 的文档序规范化兜住, 再取 min/max 双保险.
   */
  const captureSelection = React.useCallback((): PopoverState => {
    const container = containerRef.current;
    const selection = window.getSelection();
    if (
      container === null ||
      selection === null ||
      selection.rangeCount === 0 ||
      selection.isCollapsed
    ) {
      return null;
    }
    const range = selection.getRangeAt(0);
    const startNode = range.startContainer;
    const endNode = range.endContainer;
    const startElement = startNode instanceof Element ? startNode : startNode.parentElement;
    const endElement = endNode instanceof Element ? endNode : endNode.parentElement;
    const startPara = startElement?.closest<HTMLElement>("p[data-para-index]") ?? null;
    const endPara = endElement?.closest<HTMLElement>("p[data-para-index]") ?? null;
    if (startPara === null || startPara !== endPara || !container.contains(startPara)) {
      return null;
    }
    const paraIndex = Number(startPara.dataset.paraIndex);
    if (!Number.isInteger(paraIndex)) {
      return null;
    }
    const rawStart = paragraphOffsetOf(startPara, startNode, range.startOffset);
    const rawEnd = paragraphOffsetOf(startPara, endNode, range.endOffset);
    const start = Math.min(rawStart, rawEnd);
    const end = Math.max(rawStart, rawEnd);
    const quote = range.toString();
    if (end <= start || quote.trim() === "") {
      return null;
    }
    const rect = range.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) {
      return null;
    }
    return {
      mode: "new",
      target: {
        paraIndex,
        start,
        end,
        quote: quote.slice(0, 200),
        paraText: startPara.textContent ?? "",
        rect,
      },
    };
  }, [containerRef]);

  // 桌面: mouseup 评估选区; 点在工具条/弹窗里不重估 (工具条自身 onMouseDown 已 preventDefault 保住选区)
  React.useEffect(() => {
    const onMouseUp = (event: MouseEvent) => {
      if (dialogOpen) {
        return;
      }
      const target = event.target;
      if (
        target instanceof Node &&
        (popoverRef.current?.contains(target) === true ||
          (target instanceof Element && target.closest('[role="dialog"]') !== null))
      ) {
        return;
      }
      setPopover(captureSelection());
    };
    const onPointerDown = (event: PointerEvent) => {
      lastPointerType.current = event.pointerType;
    };
    document.addEventListener("mouseup", onMouseUp);
    window.addEventListener("pointerdown", onPointerDown, { capture: true });
    return () => {
      document.removeEventListener("mouseup", onMouseUp);
      window.removeEventListener("pointerdown", onPointerDown, { capture: true });
    };
  }, [captureSelection, dialogOpen]);

  // 触屏: 长按选字后系统手柄还会继续改选区, touchend 之外再用 selectionchange 防抖兜底
  React.useEffect(() => {
    let timer = 0;
    const evaluate = () => {
      if (dialogOpen) {
        return;
      }
      const next = captureSelection();
      if (next !== null) {
        setPopover(next);
      }
    };
    const onTouchEnd = () => {
      if (lastPointerType.current !== "touch") {
        return;
      }
      window.clearTimeout(timer);
      timer = window.setTimeout(evaluate, 150);
    };
    const onSelectionChange = () => {
      if (lastPointerType.current !== "touch") {
        return;
      }
      window.clearTimeout(timer);
      timer = window.setTimeout(evaluate, 350);
    };
    document.addEventListener("touchend", onTouchEnd);
    document.addEventListener("selectionchange", onSelectionChange);
    return () => {
      document.removeEventListener("touchend", onTouchEnd);
      document.removeEventListener("selectionchange", onSelectionChange);
      window.clearTimeout(timer);
    };
  }, [captureSelection, dialogOpen]);

  // 点已有批注 (mark) → 改笔记/删除工具条
  React.useEffect(() => {
    const container = containerRef.current;
    if (container === null) {
      return;
    }
    const onClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }
      const mark = target.closest<HTMLElement>("mark[data-annotation-id]");
      if (mark === null) {
        return;
      }
      const id = mark.dataset.annotationId;
      const annotation = useAnnotationsStore
        .getState()
        .byBook[bookUrl]?.find((item) => item.id === id);
      if (annotation === undefined) {
        return;
      }
      window.getSelection()?.removeAllRanges();
      setPopover({ mode: "existing", annotation, rect: mark.getBoundingClientRect() });
    };
    container.addEventListener("click", onClick);
    return () => container.removeEventListener("click", onClick);
  }, [containerRef, bookUrl]);

  // 滚动/缩放后选区矩形失效, 收起工具条; 切章同理 (批注归属章节变了)
  React.useEffect(() => {
    if (popover === null || dialogOpen) {
      return;
    }
    const hide = () => setPopover(null);
    window.addEventListener("scroll", hide, { capture: true });
    window.addEventListener("resize", hide);
    return () => {
      window.removeEventListener("scroll", hide, { capture: true });
      window.removeEventListener("resize", hide);
    };
  }, [popover, dialogOpen]);

  // 切章收起工具条 (旧选区归属上一章): 渲染阶段派生 state, 不额外跑 effect
  const [seenChapter, setSeenChapter] = React.useState(chapterIndex);
  if (seenChapter !== chapterIndex) {
    setSeenChapter(chapterIndex);
    setPopover(null);
  }

  // Esc 收起工具条 (弹窗开着时交给 Radix 自己的 Esc)
  React.useEffect(() => {
    if (popover === null || dialogOpen) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      setPopover(null);
      window.getSelection()?.removeAllRanges();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [popover, dialogOpen]);

  const dismiss = React.useCallback(() => {
    setPopover(null);
    window.getSelection()?.removeAllRanges();
  }, []);

  const commitNew = React.useCallback(
    (target: SelectionTarget, kind: Annotation["kind"], color: AnnotationColor, note: string) => {
      addAnnotation(bookUrl, {
        chapterIndex,
        paraIndex: target.paraIndex,
        start: target.start,
        end: target.end,
        quote: target.quote,
        kind,
        color,
        note,
      });
    },
    [addAnnotation, bookUrl, chapterIndex],
  );

  const handleSaveNote = React.useCallback(
    (note: string) => {
      if (noteDraft === null) {
        return;
      }
      if (noteDraft.mode === "new") {
        commitNew(noteDraft.target, "note", "yellow", note);
      } else {
        updateAnnotation(bookUrl, noteDraft.annotation.id, { note });
      }
      setNoteDraft(null);
      dismiss();
    },
    [noteDraft, commitNew, updateAnnotation, bookUrl, dismiss],
  );

  const handleDeleteExisting = React.useCallback(() => {
    if (popover?.mode !== "existing") {
      return;
    }
    removeAnnotation(bookUrl, popover.annotation.id);
    setPopover(null);
  }, [popover, removeAnnotation, bookUrl]);

  // 笔记草稿切换时重置输入 (编辑态预填原笔记): 渲染阶段派生 state, 不额外跑 effect
  const [note, setNote] = React.useState("");
  const draftKey =
    noteDraft === null
      ? null
      : noteDraft.mode === "new"
        ? `new:${noteDraft.target.paraIndex}:${noteDraft.target.start}`
        : `edit:${noteDraft.annotation.id}`;
  const [seenDraftKey, setSeenDraftKey] = React.useState<string | null>(null);
  if (draftKey !== seenDraftKey) {
    setSeenDraftKey(draftKey);
    setNote(noteDraft?.mode === "edit" ? noteDraft.annotation.note : "");
  }

  const draftQuote =
    noteDraft === null
      ? ""
      : noteDraft.mode === "new"
        ? noteDraft.target.quote
        : noteDraft.annotation.quote;

  let toolbar: React.ReactNode = null;
  if (popover !== null) {
    const rect = popover.mode === "new" ? popover.target.rect : popover.rect;
    const centerX = rect.left + rect.width / 2;
    const left = Math.min(
      Math.max(centerX, TOOLBAR_HALF_WIDTH + 8),
      Math.max(TOOLBAR_HALF_WIDTH + 8, window.innerWidth - TOOLBAR_HALF_WIDTH - 8),
    );
    const flipBelow = rect.top < FLIP_BELOW_TOP;
    const style: React.CSSProperties = {
      left: `${left}px`,
      top: flipBelow ? `${rect.top + rect.height + 10}px` : `${rect.top - 10}px`,
      transform: flipBelow ? "translateX(-50%)" : "translate(-50%, -100%)",
    };
    toolbar = (
      <div
        ref={popoverRef}
        role="toolbar"
        aria-label={popover.mode === "new" ? "批注选中文字" : "批注操作"}
        className="ui-dialog fixed z-[70] flex items-center gap-1 rounded-xl border border-border bg-surface p-1.5 shadow-xl"
        data-state="open"
        style={style}
        onMouseDown={(event) => event.preventDefault()}
      >
        {popover.mode === "new" ? (
          <>
            {(Object.keys(COLOR_LABEL) as AnnotationColor[]).map((color) => (
              <button
                key={color}
                type="button"
                aria-label={`${COLOR_LABEL[color]}高亮`}
                title={`${COLOR_LABEL[color]}高亮`}
                onClick={() => {
                  commitNew(popover.target, "highlight", color, "");
                  dismiss();
                }}
                className="size-7 shrink-0 cursor-pointer rounded-full border border-black/10 transition-transform hover:scale-110"
                style={{ backgroundColor: ANNOTATION_COLOR_HEX[color] }}
              />
            ))}
            <span aria-hidden className="mx-0.5 h-5 w-px shrink-0 bg-border" />
            <ToolbarButton
              label="下划线"
              onClick={() => {
                commitNew(popover.target, "underline", "yellow", "");
                dismiss();
              }}
            >
              <Underline className="size-4" />
            </ToolbarButton>
            <ToolbarButton label="笔记" onClick={() => setNoteDraft({ mode: "new", target: popover.target })}>
              <StickyNote className="size-4" />
            </ToolbarButton>
            <ToolbarButton
              label="词典"
              onClick={() => {
                void lookupSelection(popover.target.quote);
                dismiss();
              }}
            >
              <BookA className="size-4" />
            </ToolbarButton>
            <ToolbarButton
              label="添加净化"
              onClick={() =>
                setPurifyDraft(
                  buildPurifyDraft(
                    popover.target.paraText,
                    popover.target.start,
                    popover.target.end,
                  ),
                )
              }
            >
              <Sparkles className="size-4" />
            </ToolbarButton>
            <ToolbarButton label="取消" onClick={dismiss}>
              <X className="size-4" />
            </ToolbarButton>
          </>
        ) : (
          <>
            <ToolbarButton
              label={popover.annotation.note === "" ? "添加笔记" : "修改笔记"}
              onClick={() => setNoteDraft({ mode: "edit", annotation: popover.annotation })}
            >
              <StickyNote className="size-4" />
            </ToolbarButton>
            <ToolbarButton label="删除批注" danger onClick={handleDeleteExisting}>
              <Trash2 className="size-4" />
            </ToolbarButton>
            <ToolbarButton label="关闭" onClick={() => setPopover(null)}>
              <X className="size-4" />
            </ToolbarButton>
          </>
        )}
      </div>
    );
  }

  return (
    <>
      {toolbar}
      <Dialog
        open={noteDraft !== null}
        onOpenChange={(open) => {
          if (!open) {
            setNoteDraft(null);
          }
        }}
      >
        <DialogContent width="sm" onKeyDown={suppressArrowKeys}>
          <DialogHeader>
            <DialogTitle>{noteDraft?.mode === "edit" ? "修改笔记" : "添加笔记"}</DialogTitle>
            <DialogDescription className="line-clamp-2">{draftQuote}</DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-2 px-4 md:px-5">
            <Textarea
              value={note}
              onChange={(event) => setNote(event.target.value)}
              placeholder="写点想法…"
              maxLength={500}
              autoSize
            />
            <p className="text-xs leading-5 text-muted-foreground">
              批注与笔记只保存在本机浏览器, 不会同步到服务器.
            </p>
          </div>
          <DialogFooter>
            <DialogClose asChild>
              <Button variant="secondary">取消</Button>
            </DialogClose>
            <Button
              disabled={noteDraft?.mode === "new" && note.trim() === ""}
              onClick={() => handleSaveNote(note.trim())}
            >
              <StickyNote aria-hidden />
              保存笔记
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={dictState !== null}
        onOpenChange={(open) => {
          if (!open) {
            setDictState(null);
          }
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle className="break-all">{dictState?.word ?? ""}</DialogTitle>
            <DialogDescription>离线词典(英汉 ECDICT / 汉语新华字典)</DialogDescription>
          </DialogHeader>
          <div className="max-h-80 overflow-y-auto px-4 pb-2 text-sm md:px-5">
            {dictState?.loading ? (
              <p className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
                <Spinner size="sm" label="查询中" />
                查询中…
              </p>
            ) : dictState?.result?.status === "unavailable" ? (
              <p className="py-6 text-sm text-muted-foreground">
                未安装词典数据(镜像构建时 WITH_DICT=0, 或 READER_DICT_DIR 无数据文件)
              </p>
            ) : (dictState?.result?.entries.length ?? 0) === 0 ? (
              <p className="py-6 text-sm text-muted-foreground">未找到释义</p>
            ) : (
              <ul className="flex flex-col divide-y divide-border/60">
                {dictState?.result?.entries.map((entry, index) => (
                  <li key={`${entry.word}-${index}`} className="flex flex-col gap-1.5 py-3">
                    <div className="flex flex-wrap items-center gap-2">
                      <Badge size="sm" variant="muted">
                        {DICT_KIND_LABEL[entry.kind] ?? entry.kind}
                      </Badge>
                      <span className="font-display text-base font-semibold">{entry.word}</span>
                      {entry.pinyin ? (
                        <span className="text-xs text-accent">{entry.pinyin}</span>
                      ) : null}
                      {entry.phonetic ? (
                        <span className="text-xs text-muted-foreground">/{entry.phonetic}/</span>
                      ) : null}
                    </div>
                    {entry.body ? (
                      <p className="whitespace-pre-line text-sm leading-6 text-foreground/85">
                        {entry.body}
                      </p>
                    ) : null}
                    <span className="text-xs text-muted-foreground/70">来源: {entry.source}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </DialogContent>
      </Dialog>
      <AddPurifyDialog
        draft={purifyDraft}
        onOpenChange={(open) => {
          if (!open) {
            setPurifyDraft(null);
          }
        }}
        onSaved={() => {
          setPurifyDraft(null);
          dismiss();
        }}
      />
    </>
  );
}
