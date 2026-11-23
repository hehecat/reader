import { BookOpenText, Check, Copy, MessageSquarePlus, Sparkles, X } from "lucide-react";
import * as React from "react";
import { featuresApi } from "@/api/endpoints";
import type { DictEntry, DictLookupResult } from "@/api/types";
import { cn } from "@/lib/cn";
import { useNotesStore, type InkColor } from "@/stores/notes";
import { notify } from "@/stores/toast";
import { AnimatePresence, motion, SPRING_POP, useReducedMotion } from "@/ui/motion";

export interface SelectionInkBarProps {
  containerRef: React.RefObject<HTMLElement | null>;
  bookUrl: string;
  chapterIndex: number;
  chapterTitle: string;
  disabled?: boolean;
  onRequestRefreshChapter?: () => void;
}

interface SelectionAnchor {
  quote: string;
  paragraphIndex: number;
  x: number;
  y: number;
  placement: "above" | "below";
}

const INK_SWATCHES: readonly { color: InkColor; label: string; dotClass: string }[] = [
  { color: "amber", label: "琥珀高亮", dotClass: "bg-[#d89b28]" },
  { color: "sage", label: "竹青高亮", dotClass: "bg-[#3d8b63]" },
  { color: "vermilion", label: "朱砂高亮", dotClass: "bg-[#c3412f]" },
];

function findParagraphIndex(node: Node | null, root: HTMLElement): number {
  let cur: HTMLElement | null =
    node instanceof HTMLElement ? node : node?.parentElement ?? null;
  while (cur && cur !== root) {
    const attr = cur.getAttribute("data-para-index");
    if (attr !== null) {
      const parsed = Number(attr);
      if (Number.isFinite(parsed) && parsed >= 0) return parsed;
    }
    cur = cur.parentElement;
  }
  return 0;
}

async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fallback below
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

export function SelectionInkBar({
  containerRef,
  bookUrl,
  chapterIndex,
  chapterTitle,
  disabled = false,
  onRequestRefreshChapter,
}: SelectionInkBarProps) {
  const addNote = useNotesStore((s) => s.addNote);
  const reduced = useReducedMotion();

  const barRef = React.useRef<HTMLDivElement>(null);
  const noteInputRef = React.useRef<HTMLInputElement>(null);
  const dictAbortRef = React.useRef<AbortController | null>(null);

  const [anchor, setAnchor] = React.useState<SelectionAnchor | null>(null);
  const [panel, setPanel] = React.useState<"none" | "note" | "dict">("none");
  const [noteColor, setNoteColor] = React.useState<InkColor>("amber");
  const [noteDraft, setNoteDraft] = React.useState("");
  const [copied, setCopied] = React.useState(false);
  const [purifying, setPurifying] = React.useState(false);

  const [dictLoading, setDictLoading] = React.useState(false);
  const [dictResult, setDictResult] = React.useState<DictLookupResult | null>(null);
  const [dictError, setDictError] = React.useState<string | null>(null);

  const dismiss = React.useCallback((clearDomSelection = false) => {
    dictAbortRef.current?.abort();
    dictAbortRef.current = null;
    setAnchor(null);
    setPanel("none");
    setNoteDraft("");
    setDictResult(null);
    setDictError(null);
    setDictLoading(false);
    setCopied(false);
    if (clearDomSelection) {
      window.getSelection()?.removeAllRanges();
    }
  }, []);

  React.useEffect(() => {
    if (disabled) dismiss(false);
  }, [disabled, dismiss]);

  React.useEffect(() => {
    dismiss(false);
  }, [bookUrl, chapterIndex, dismiss]);

  React.useEffect(() => {
    return () => {
      dictAbortRef.current?.abort();
    };
  }, []);

  const readSelection = React.useCallback(() => {
    if (disabled) return;
    const root = containerRef.current;
    if (!root) return;
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || sel.rangeCount === 0) return;

    const range = sel.getRangeAt(0);
    if (!root.contains(range.commonAncestorContainer)) return;

    const quote = sel.toString().replace(/\s+/g, " ").trim();
    if (!quote || quote.length > 1200) return;

    const rect = range.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) return;

    const paragraphIndex = findParagraphIndex(range.startContainer, root);
    const placement: "above" | "below" = rect.top < 110 ? "below" : "above";
    const rawX = rect.left + rect.width / 2;
    const x = Math.min(window.innerWidth - 20, Math.max(20, rawX));
    const y = placement === "above" ? rect.top - 12 : rect.bottom + 12;

    setAnchor((prev) => {
      if (prev && prev.quote === quote && prev.paragraphIndex === paragraphIndex) {
        return { ...prev, x, y, placement };
      }
      setPanel("none");
      setNoteDraft("");
      setDictResult(null);
      setDictError(null);
      setCopied(false);
      return { quote, paragraphIndex, x, y, placement };
    });
  }, [containerRef, disabled]);

  React.useEffect(() => {
    const onPointerUp = (e: PointerEvent) => {
      if (barRef.current?.contains(e.target as Node)) return;
      window.setTimeout(readSelection, 16);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        dismiss(true);
        return;
      }
      if (e.shiftKey) {
        window.setTimeout(readSelection, 16);
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      if (barRef.current?.contains(e.target as Node)) return;
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) {
        dismiss(false);
      }
    };
    const onSelectionChange = () => {
      if (panel === "note" || panel === "dict") return;
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) {
        setAnchor(null);
      }
    };

    document.addEventListener("pointerup", onPointerUp);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("selectionchange", onSelectionChange);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      document.removeEventListener("pointerup", onPointerUp);
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("selectionchange", onSelectionChange);
      window.removeEventListener("keyup", onKeyUp);
    };
  }, [dismiss, panel, readSelection]);

  const handleQuickHighlight = (color: InkColor) => {
    if (!anchor) return;
    addNote({
      bookUrl,
      chapterIndex,
      chapterTitle,
      paragraphIndex: anchor.paragraphIndex,
      quote: anchor.quote,
      note: "",
      color,
    });
    notify.success("已添加划线");
    dismiss(true);
  };

  const handleSaveNote = (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!anchor) return;
    addNote({
      bookUrl,
      chapterIndex,
      chapterTitle,
      paragraphIndex: anchor.paragraphIndex,
      quote: anchor.quote,
      note: noteDraft.trim(),
      color: noteColor,
    });
    notify.success(noteDraft.trim() ? "已保存划线批注" : "已添加划线");
    dismiss(true);
  };

  const handleOpenNote = () => {
    setPanel((prev) => (prev === "note" ? "none" : "note"));
    window.setTimeout(() => noteInputRef.current?.focus(), 40);
  };

  const handleLookupDict = async () => {
    if (!anchor) return;
    if (panel === "dict" && !dictLoading) {
      setPanel("none");
      return;
    }
    const word = anchor.quote.slice(0, 32).trim();
    if (!word) return;

    dictAbortRef.current?.abort();
    const controller = new AbortController();
    dictAbortRef.current = controller;

    setPanel("dict");
    setDictLoading(true);
    setDictError(null);
    setDictResult(null);

    try {
      const res = await featuresApi.dictLookup(word, controller.signal);
      if (!controller.signal.aborted) {
        setDictResult(res);
      }
    } catch (err) {
      if (!controller.signal.aborted) {
        setDictError(err instanceof Error ? err.message : "词典查询失败");
      }
    } finally {
      if (!controller.signal.aborted) {
        setDictLoading(false);
      }
    }
  };

  const handleCopy = async () => {
    if (!anchor) return;
    const ok = await copyText(anchor.quote);
    if (ok) {
      setCopied(true);
      notify.success("已复制到剪贴板");
      window.setTimeout(() => dismiss(true), 420);
    } else {
      notify.error("复制失败");
    }
  };

  const handleCreatePurifyRule = async () => {
    if (!anchor || purifying) return;
    setPurifying(true);
    try {
      const snippet = anchor.quote.slice(0, 18);
      await featuresApi.saveReplaceRule({
        name: `净化 · ${snippet}${anchor.quote.length > 18 ? "…" : ""}`,
        group: "阅读器快速净化",
        find: anchor.quote,
        replace: "",
        scopeContent: true,
        scopeTitle: false,
        isRegex: false,
        enabled: true,
      });
      notify.success(
        "已加入正文净化规则",
        onRequestRefreshChapter
          ? { label: "刷新本章", onClick: onRequestRefreshChapter }
          : undefined,
      );
      dismiss(true);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "保存净化规则失败");
    } finally {
      setPurifying(false);
    }
  };

  const vw = typeof window !== "undefined" ? window.innerWidth : 375;
  const halfBar = Math.min(vw / 2 - 12, panel === "none" ? 165 : 185);
  const clampedLeft = anchor
    ? Math.min(vw - halfBar - 12, Math.max(halfBar + 12, anchor.x))
    : 0;

  return (
    <AnimatePresence>
      {anchor ? (
        <motion.div
          ref={barRef}
          key="selection-ink-bar"
          initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.92, y: anchor.placement === "above" ? 6 : -6 }}
          animate={{ opacity: 1, scale: 1, y: 0 }}
          exit={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.94 }}
          transition={reduced ? { duration: 0.1 } : SPRING_POP}
          style={{
            left: clampedLeft,
            top: anchor.y,
            transform:
              anchor.placement === "above"
                ? "translate(-50%, -100%)"
                : "translate(-50%, 0%)",
          }}
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
          className="fixed z-40 max-w-[min(92vw,360px)] select-none"
        >
          <motion.div
            layout={!reduced}
            transition={SPRING_POP}
            className="surface-glass overflow-hidden rounded-2xl"
          >
            {/* Primary Ink Bar */}
            <div className="flex items-center gap-1 px-2 py-1.5">
              <div className="flex items-center gap-1.5 px-1">
                {INK_SWATCHES.map((s) => (
                  <button
                    key={s.color}
                    type="button"
                    title={s.label}
                    aria-label={s.label}
                    onClick={() => handleQuickHighlight(s.color)}
                    className={cn(
                      "size-5 cursor-pointer rounded-full ring-2 ring-elevated transition-transform hover:scale-115 active:scale-95",
                      s.dotClass,
                    )}
                  />
                ))}
              </div>

              <span aria-hidden className="mx-0.5 h-4 w-px bg-line-strong" />

              <InkActionButton
                active={panel === "note"}
                onClick={handleOpenNote}
                icon={<MessageSquarePlus className="size-3.5" />}
                label="批注"
              />
              <InkActionButton
                active={panel === "dict"}
                onClick={() => void handleLookupDict()}
                icon={<BookOpenText className="size-3.5" />}
                label="查词"
              />
              <InkActionButton
                onClick={() => void handleCopy()}
                icon={copied ? <Check className="size-3.5 text-success" /> : <Copy className="size-3.5" />}
                label={copied ? "已复制" : "复制"}
              />
              <InkActionButton
                disabled={purifying}
                onClick={() => void handleCreatePurifyRule()}
                icon={<Sparkles className="size-3.5" />}
                label="净化"
              />
            </div>

            {/* Inline Note Composer */}
            <AnimatePresence initial={false}>
              {panel === "note" ? (
                <motion.form
                  key="note-panel"
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: "auto", opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  transition={{ duration: 0.18 }}
                  onSubmit={handleSaveNote}
                  className="overflow-hidden border-t border-line px-3 py-2.5"
                >
                  <p className="mb-2 line-clamp-2 border-l-2 border-accent/45 pl-2 font-serif text-xs text-ink-secondary">
                    {anchor.quote}
                  </p>
                  <div className="flex items-center gap-2">
                    <div className="flex items-center gap-1">
                      {INK_SWATCHES.map((s) => (
                        <button
                          key={s.color}
                          type="button"
                          aria-label={s.label}
                          onClick={() => setNoteColor(s.color)}
                          className={cn(
                            "size-4 cursor-pointer rounded-full transition-transform",
                            s.dotClass,
                            noteColor === s.color
                              ? "scale-110 ring-2 ring-accent ring-offset-1 ring-offset-elevated"
                              : "opacity-55 hover:opacity-100",
                          )}
                        />
                      ))}
                    </div>
                    <input
                      ref={noteInputRef}
                      value={noteDraft}
                      onChange={(e) => setNoteDraft(e.target.value)}
                      placeholder="写下批注，回车保存…"
                      className="min-w-0 flex-1 rounded-lg border border-line bg-elevated/80 px-2.5 py-1.5 text-xs text-ink outline-none placeholder:text-ink-muted focus:border-accent/60"
                    />
                    <button
                      type="submit"
                      className="shrink-0 cursor-pointer rounded-lg bg-accent px-2.5 py-1.5 text-xs font-medium text-accent-ink hover:brightness-110"
                    >
                      保存
                    </button>
                  </div>
                </motion.form>
              ) : null}

              {/* Inline Dictionary Card */}
              {panel === "dict" ? (
                <motion.div
                  key="dict-panel"
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: "auto", opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  transition={{ duration: 0.2 }}
                  className="overflow-hidden border-t border-line"
                >
                  <div className="flex items-center justify-between px-3 pt-2.5 pb-1.5">
                    <span className="truncate font-serif text-sm font-semibold text-ink">
                      {anchor.quote.slice(0, 32)}
                    </span>
                    <button
                      type="button"
                      onClick={() => setPanel("none")}
                      aria-label="收起词典"
                      className="flex size-5 cursor-pointer items-center justify-center rounded-full text-ink-muted hover:bg-subtle hover:text-ink"
                    >
                      <X className="size-3" />
                    </button>
                  </div>

                  <div
                    className="max-h-56 overflow-y-auto px-3 pb-3 text-xs"
                    onWheel={(e) => e.stopPropagation()}
                    onTouchMove={(e) => e.stopPropagation()}
                  >
                    {dictLoading ? (
                      <div className="flex items-center gap-2 py-4 text-ink-muted">
                        <span className="size-3.5 animate-spin rounded-full border-2 border-line-strong border-t-accent" />
                        <span>查词释义中…</span>
                      </div>
                    ) : dictError ? (
                      <p className="py-3 text-danger">{dictError}</p>
                    ) : dictResult?.status === "building" ? (
                      <p className="py-3 text-ink-muted">离线词典索引正在构建，请稍后再试。</p>
                    ) : dictResult?.status === "unavailable" ? (
                      <p className="py-3 text-ink-muted">当前环境未配置离线词典。</p>
                    ) : !dictResult || dictResult.entries.length === 0 ? (
                      <p className="py-3 text-ink-muted">未查到「{anchor.quote.slice(0, 18)}」的词条释义。</p>
                    ) : (
                      <div className="space-y-2.5">
                        {dictResult.entries.map((entry, idx) => (
                          <DictEntryCard key={`${entry.word}-${entry.source}-${idx}`} entry={entry} />
                        ))}
                      </div>
                    )}
                  </div>
                </motion.div>
              ) : null}
            </AnimatePresence>
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
}

function InkActionButton({
  icon,
  label,
  active = false,
  disabled = false,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  active?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cn(
        "inline-flex cursor-pointer items-center gap-1 rounded-xl px-2 py-1 text-xs font-medium whitespace-nowrap transition-colors disabled:opacity-45",
        active ? "bg-accent-soft text-accent" : "text-ink-secondary hover:bg-subtle hover:text-ink",
      )}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}

function DictEntryCard({ entry }: { entry: DictEntry }) {
  return (
    <div className="rounded-xl border border-line bg-elevated/70 p-2.5">
      <div className="flex flex-wrap items-baseline gap-1.5">
        <span className="font-serif text-[13px] font-semibold text-ink">{entry.word}</span>
        {entry.pinyin ? (
          <span className="font-mono text-[11px] text-accent">{entry.pinyin}</span>
        ) : null}
        {entry.phonetic ? (
          <span className="font-mono text-[11px] text-ink-muted">/{entry.phonetic}/</span>
        ) : null}
        <span className="ml-auto text-[10px] text-ink-muted">{entry.source}</span>
      </div>
      {entry.body ? (
        <p className="mt-1 whitespace-pre-line text-xs leading-relaxed text-ink-secondary">
          {entry.body}
        </p>
      ) : null}
    </div>
  );
}
