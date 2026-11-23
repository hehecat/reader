import { useQueryClient } from "@tanstack/react-query";
import {
  AlertCircle,
  BookOpen,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  FileText,
  FileUp,
  RefreshCw,
  Upload,
  X,
} from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";
import { booksApi } from "@/api/endpoints";
import type { BookInfo, ImportBookPreview } from "@/api/types";
import { cn } from "@/lib/cn";
import { buildReaderUrl, cleanIntro, formatBytes } from "@/lib/format";
import { notify } from "@/stores/toast";
import { BookCover } from "@/ui/BookCover";
import { Button, IconButton } from "@/ui/Button";
import { Badge, Spinner } from "@/ui/Controls";
import { AnimatePresence, motion, SPRING_POP, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";
import { Sheet } from "@/ui/Sheet";

export interface ImportBookSheetProps {
  open: boolean;
  onClose: () => void;
  initialFile?: File | null;
  onImported?: (book: BookInfo) => void;
}

const ACCEPTED_EXT = /\.(txt|epub)$/i;

export function ImportBookSheet({
  open,
  onClose,
  initialFile = null,
  onImported,
}: ImportBookSheetProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const reduced = useReducedMotion();
  const inputRef = React.useRef<HTMLInputElement>(null);

  const [file, setFile] = React.useState<File | null>(null);
  const [dragging, setDragging] = React.useState(false);
  const [preview, setPreview] = React.useState<ImportBookPreview | null>(null);
  const [previewLoading, setPreviewLoading] = React.useState(false);
  const [previewError, setPreviewError] = React.useState<string | null>(null);
  const [expandToc, setExpandToc] = React.useState(false);
  const [uploading, setUploading] = React.useState(false);
  const [importedBook, setImportedBook] = React.useState<BookInfo | null>(null);

  const runPreview = React.useCallback(async (target: File) => {
    setFile(target);
    setPreview(null);
    setPreviewError(null);
    setExpandToc(false);
    setImportedBook(null);
    setPreviewLoading(true);
    try {
      const res = await booksApi.previewLocalBook(target);
      setPreview(res);
    } catch (err) {
      setPreviewError(err instanceof Error ? err.message : "文件解析失败，请检查文件编码或格式");
    } finally {
      setPreviewLoading(false);
    }
  }, []);

  const handleSelectFile = React.useCallback(
    (incoming: File | null | undefined) => {
      if (!incoming) return;
      if (!ACCEPTED_EXT.test(incoming.name)) {
        notify.error("仅支持导入 .txt 或 .epub 格式的电子书");
        return;
      }
      void runPreview(incoming);
    },
    [runPreview],
  );

  React.useEffect(() => {
    if (!open) return;
    if (initialFile) {
      handleSelectFile(initialFile);
    }
  }, [open, initialFile, handleSelectFile]);

  const resetAll = React.useCallback(() => {
    setFile(null);
    setPreview(null);
    setPreviewError(null);
    setExpandToc(false);
    setPreviewLoading(false);
    setUploading(false);
    setImportedBook(null);
    if (inputRef.current) inputRef.current.value = "";
  }, []);

  const handleClose = () => {
    resetAll();
    onClose();
  };

  const handleUpload = async () => {
    if (!file) return;
    setUploading(true);
    try {
      const saved = await booksApi.uploadLocalBook(file);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["shelf"] }),
        queryClient.invalidateQueries({ queryKey: ["groups"] }),
      ]);
      setImportedBook(saved);
      notify.success(`已将《${saved.name}》收入书房`);
      onImported?.(saved);
    } catch (err) {
      notify.error(err instanceof Error ? err.message : "上传导入失败");
    } finally {
      setUploading(false);
    }
  };

  const allPreviewTitles = React.useMemo(() => {
    if (!preview) return [];
    if (preview.chapters && preview.chapters.length > 0) {
      return preview.chapters.slice(0, 40).map((ch, idx) => ({
        index: ch.index ?? idx,
        title: ch.title,
      }));
    }
    if (preview.preview && preview.preview.length > 0) {
      return preview.preview.slice(0, 40).map((title, idx) => ({
        index: idx,
        title,
      }));
    }
    return [];
  }, [preview]);

  const previewTitles = expandToc ? allPreviewTitles : allPreviewTitles.slice(0, 12);
  const previewIntro = React.useMemo(() => cleanIntro(preview?.book?.intro), [preview]);

  const stageKey = importedBook ? "imported" : !file ? "dropzone" : "preview";

  return (
    <Sheet
      open={open}
      onClose={handleClose}
      size="md"
      title="导入本地书籍"
      subtitle="支持 TXT / EPUB，自动解析书目信息与章节结构"
      footer={
        importedBook ? (
          <div className="flex items-center justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={resetAll}>
              继续导入
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={() => {
                const targetUrl = importedBook.bookUrl;
                handleClose();
                navigate(buildReaderUrl(targetUrl, 0));
              }}
            >
              <BookOpen className="size-3.5" />
              立即阅读
            </Button>
          </div>
        ) : (
          <div className="flex items-center justify-between gap-2">
            <span className="min-w-0 truncate text-[11px] text-ink-muted tabular-nums">
              {file ? `${file.name} · ${formatBytes(file.size)}` : "未选择文件"}
            </span>
            <div className="flex shrink-0 items-center gap-2">
              <Button variant="ghost" size="sm" onClick={handleClose}>
                取消
              </Button>
              <Button
                variant="primary"
                size="sm"
                loading={uploading}
                disabled={!file || previewLoading}
                onClick={handleUpload}
              >
                <Upload className="size-3.5" />
                收入书房
              </Button>
            </div>
          </div>
        )
      }
    >
      <input
        ref={inputRef}
        type="file"
        accept=".txt,.epub,text/plain,application/epub+zip"
        className="hidden"
        onChange={(e) => {
          const picked = e.target.files?.[0];
          if (picked) handleSelectFile(picked);
          e.currentTarget.value = "";
        }}
      />

      <AnimatePresence mode="wait" initial={false}>
        {stageKey === "imported" && importedBook ? (
          <motion.div
            key="imported"
            initial={reduced ? { opacity: 0 } : { opacity: 0, scale: 0.96 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0 }}
            transition={SPRING_POP}
            className="flex flex-col items-center gap-4 py-8 text-center"
          >
            <motion.div
              initial={reduced ? undefined : { scale: 0.7 }}
              animate={{ scale: 1 }}
              transition={SPRING_POP}
              className="flex size-13 items-center justify-center rounded-2xl bg-success-soft text-success shadow-[var(--shadow-sm)]"
            >
              <CheckCircle2 className="size-6" />
            </motion.div>
            <div className="space-y-1">
              <h3 className="font-serif text-lg font-semibold text-ink">
                《{importedBook.name}》已收入书房
              </h3>
              <p className="text-xs text-ink-muted">
                {importedBook.author || "佚名"} · 本地书库已完成建档，可即刻展卷阅读。
              </p>
            </div>
            <BookCover
              src={importedBook.coverUrl}
              title={importedBook.name}
              author={importedBook.author}
              pageEdges
              interactive={false}
              className="mt-1 w-24"
            />
          </motion.div>
        ) : stageKey === "dropzone" ? (
          <motion.div
            key="dropzone"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            transition={SPRING_SNAPPY}
          >
            <div
              role="button"
              tabIndex={0}
              onClick={() => inputRef.current?.click()}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  inputRef.current?.click();
                }
              }}
              onDragOver={(e) => {
                e.preventDefault();
                setDragging(true);
              }}
              onDragLeave={(e) => {
                e.preventDefault();
                setDragging(false);
              }}
              onDrop={(e) => {
                e.preventDefault();
                e.stopPropagation();
                setDragging(false);
                const dropped = e.dataTransfer.files?.[0];
                if (dropped) handleSelectFile(dropped);
              }}
              className={cn(
                "group relative flex cursor-pointer flex-col items-center justify-center gap-4 overflow-hidden rounded-3xl border-2 border-dashed px-6 py-14 text-center transition-all",
                dragging
                  ? "scale-[1.01] border-accent bg-accent-soft/50 ring-4 ring-accent/15"
                  : "border-line-strong bg-elevated/55 hover:border-accent/55 hover:bg-elevated/90",
              )}
            >
              <motion.div
                animate={dragging && !reduced ? { y: [0, -5, 0] } : { y: 0 }}
                transition={dragging ? { duration: 1.2, repeat: Infinity, ease: "easeInOut" } : undefined}
                className={cn(
                  "flex size-14 items-center justify-center rounded-2xl transition-colors",
                  dragging
                    ? "bg-accent text-accent-ink shadow-md"
                    : "bg-subtle text-ink-secondary group-hover:bg-accent-soft group-hover:text-accent",
                )}
              >
                <FileUp className="size-6" />
              </motion.div>
              <div className="space-y-1.5">
                <p className="font-serif text-base font-medium text-ink">
                  {dragging ? "松手解析电子书" : "拖入电子书文件，或点击选取"}
                </p>
                <p className="text-xs text-ink-muted">
                  自动检测 UTF-8 / GBK 编码与章节结构，确认预览后入架
                </p>
              </div>
              <div className="flex items-center gap-2">
                <Badge tone="accent">.TXT</Badge>
                <Badge tone="accent">.EPUB</Badge>
              </div>
              <Button size="sm" variant="secondary" className="mt-1 pointer-events-none">
                选择本地文件
              </Button>
            </div>
          </motion.div>
        ) : file ? (
          <motion.div
            key="preview"
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0 }}
            transition={SPRING_SNAPPY}
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={(e) => {
              e.preventDefault();
              setDragging(false);
            }}
            onDrop={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setDragging(false);
              const dropped = e.dataTransfer.files?.[0];
              if (dropped) handleSelectFile(dropped);
            }}
            className={cn(
              "space-y-5 rounded-2xl transition-all",
              dragging && "ring-2 ring-accent/50 ring-offset-4 ring-offset-bg",
            )}
          >
            {/* Selected file strip */}
            <div className="flex items-center justify-between gap-3 rounded-2xl border border-line bg-subtle/55 px-3.5 py-2.5">
              <div className="flex min-w-0 items-center gap-2.5">
                <div className="flex size-8 shrink-0 items-center justify-center rounded-xl bg-elevated text-accent shadow-[var(--shadow-sm)]">
                  <FileText className="size-4" />
                </div>
                <div className="min-w-0">
                  <p className="truncate text-[13px] font-medium text-ink">{file.name}</p>
                  <p className="text-[11px] text-ink-muted tabular-nums">
                    {formatBytes(file.size)} · 可直接拖入新文件替换
                  </p>
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => inputRef.current?.click()}
                >
                  换文件
                </Button>
                <IconButton
                  size="sm"
                  aria-label="清除所选文件"
                  onClick={resetAll}
                >
                  <X className="size-3.5" />
                </IconButton>
              </div>
            </div>

            {/* Preview Content */}
            {previewLoading ? (
              <div className="flex flex-col items-center justify-center gap-3 rounded-2xl border border-line bg-elevated/55 py-12">
                <Spinner label="正在解析书目信息与目录结构…" />
                <p className="text-[11px] text-ink-muted">大体积文件可能需要数秒</p>
              </div>
            ) : previewError ? (
              <div className="space-y-3 rounded-2xl border border-danger/30 bg-danger-soft/50 p-4">
                <div className="flex items-start gap-2.5 text-danger">
                  <AlertCircle className="mt-0.5 size-4 shrink-0" />
                  <div className="min-w-0 flex-1 space-y-1">
                    <p className="text-xs font-medium">目录预览解析失败</p>
                    <p className="text-xs opacity-85">{previewError}</p>
                  </div>
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    size="xs"
                    variant="secondary"
                    onClick={() => void runPreview(file)}
                  >
                    <RefreshCw className="size-3" />
                    重新解析
                  </Button>
                  <span className="text-[11px] text-ink-muted">
                    仍可尝试点击右下角「收入书房」直接上传
                  </span>
                </div>
              </div>
            ) : preview ? (
              <div className="space-y-4">
                {/* Parsed Book Card */}
                <div className="flex items-start gap-4 rounded-2xl border border-line bg-elevated/80 p-4 shadow-[var(--shadow-sm)]">
                  <BookCover
                    src={preview.book?.coverUrl}
                    title={preview.name}
                    author={preview.author}
                    pageEdges
                    interactive={false}
                    className="w-20 shrink-0"
                  />
                  <div className="min-w-0 flex-1 space-y-2">
                    <h3 className="font-serif text-lg leading-snug font-semibold text-ink">
                      {preview.name}
                    </h3>
                    <p className="text-xs text-ink-secondary">
                      {preview.author || "佚名"}
                    </p>
                    <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
                      <Badge tone="accent">
                        {(preview.format || "TXT").toUpperCase()}
                      </Badge>
                      <Badge tone="success">共 {preview.chapterCount} 章</Badge>
                      <Badge>{formatBytes(file.size)}</Badge>
                    </div>
                    {previewIntro ? (
                      <p className="line-clamp-2 pt-0.5 text-[11.5px] leading-relaxed text-ink-muted">
                        {previewIntro}
                      </p>
                    ) : null}
                  </div>
                </div>

                {/* Chapter Directory Preview */}
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <h4 className="font-serif text-xs font-semibold tracking-wide text-ink-secondary">
                      目录预览
                    </h4>
                    <span className="text-[11px] text-ink-muted tabular-nums">
                      显示前 {previewTitles.length} / 共 {preview.chapterCount} 章
                    </span>
                  </div>
                  {previewTitles.length > 0 ? (
                    <div className="overflow-hidden rounded-2xl border border-line bg-elevated/60">
                      <div className="max-h-60 divide-y divide-line/70 overflow-y-auto">
                        {previewTitles.map((item) => (
                          <div
                            key={`${item.index}-${item.title}`}
                            className="flex items-center gap-3 px-3.5 py-2 text-xs transition-colors hover:bg-subtle/45"
                          >
                            <span className="w-7 shrink-0 font-mono text-[11px] text-ink-muted tabular-nums">
                              {item.index + 1}
                            </span>
                            <span className="min-w-0 flex-1 truncate text-ink">
                              {item.title}
                            </span>
                          </div>
                        ))}
                      </div>
                      {allPreviewTitles.length > 12 ? (
                        <button
                          type="button"
                          onClick={() => setExpandToc((v) => !v)}
                          className="flex w-full cursor-pointer items-center justify-center gap-1 border-t border-line/70 bg-subtle/40 py-2 text-[11px] font-medium text-ink-secondary transition-colors hover:bg-subtle/80 hover:text-ink"
                        >
                          {expandToc ? (
                            <>
                              <ChevronUp className="size-3.5" />
                              收起目录预览
                            </>
                          ) : (
                            <>
                              <ChevronDown className="size-3.5" />
                              展开更多预览（共 {allPreviewTitles.length} 条）
                            </>
                          )}
                        </button>
                      ) : preview.chapterCount > previewTitles.length ? (
                        <div className="border-t border-line/70 px-3.5 py-2 text-center text-[11px] text-ink-muted">
                          以及其余 {preview.chapterCount - previewTitles.length} 章…
                        </div>
                      ) : null}
                    </div>
                  ) : (
                    <div className="rounded-2xl border border-line bg-subtle/40 px-4 py-6 text-center text-xs text-ink-muted">
                      未切分出独立章节，入库后将以整卷呈现
                    </div>
                  )}
                </div>
              </div>
            ) : null}
          </motion.div>
        ) : null}
      </AnimatePresence>
    </Sheet>
  );
}
