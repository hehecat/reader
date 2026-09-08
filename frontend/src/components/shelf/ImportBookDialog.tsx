import { useQueryClient } from "@tanstack/react-query";
import { CircleAlert, FileUp } from "lucide-react";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  Input,
  Spinner,
  toast,
} from "@/components/ui";
import { BOOKS_QUERY_KEY, errorMessage } from "@/hooks/useBookshelf";
import { saveBook } from "@/services/bookshelf";
import { importBookPreview, uploadLocalBook, type ImportPreview } from "@/services/import";

/** 文件选择框允许的扩展名: 与后端 SUPPORTED_EXTENSIONS(local_sync) 全量对齐 */
const ACCEPT_EXTENSIONS = ".txt,.epub,.mobi,.azw3,.pdf,.fb2,.docx,.cbz,.umd";
const SUPPORTED_EXTENSIONS = ["txt", "epub", "mobi", "azw3", "pdf", "fb2", "docx", "cbz", "umd"];
const SUPPORTED_LABEL = "TXT / EPUB / MOBI / AZW3 / PDF / FB2 / DOCX / CBZ / UMD";

/** pick=选文件 uploading=上传解析中 preview=预览确认 saving=加入书架中 */
type Phase = "pick" | "uploading" | "preview" | "saving";

export interface ImportBookDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * 导入本地书籍: 选文件 → importBookPreview 上传解析(后端不落盘, 取消无需清理)
 * → 预览(可改书名/作者) → uploadLocalBook 正式上传入架; 用户改过书名/作者时再走
 * saveBook 增量补存. 成功后 invalidate 书架查询, 由重拉列表呈现服务端真值.
 */
export function ImportBookDialog({ open, onOpenChange }: ImportBookDialogProps) {
  const queryClient = useQueryClient();
  const [phase, setPhase] = React.useState<Phase>("pick");
  /** 预览通过后留存的原始文件, 确认时经 uploadLocalBook 正式入库 */
  const [file, setFile] = React.useState<File | null>(null);
  const [preview, setPreview] = React.useState<ImportPreview | null>(null);
  const [name, setName] = React.useState("");
  const [author, setAuthor] = React.useState("");
  const [error, setError] = React.useState<string | null>(null);
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  const abortRef = React.useRef<AbortController | null>(null);

  /** 中止在途上传(关闭对话框时调用) */
  const abortUpload = () => {
    abortRef.current?.abort();
    abortRef.current = null;
  };

  /** 回到选文件初始态 */
  const reset = () => {
    abortUpload();
    setPhase("pick");
    setFile(null);
    setPreview(null);
    setName("");
    setAuthor("");
    setError(null);
  };

  // 每次打开都从选文件开始, 避免残留上一次的状态
  React.useEffect(() => {
    if (open) reset();
  }, [open]);

  const handleOpenChange = (next: boolean) => {
    if (!next) {
      abortUpload();
      reset();
    }
    onOpenChange(next);
  };

  const pickFile = () => {
    setError(null);
    fileInputRef.current?.click();
  };

  const handleFileChange = async (files: FileList | null) => {
    const picked = files?.[0] ?? null;
    // 清空 input, 保证取消后重选同一文件也能触发 change
    if (fileInputRef.current !== null) fileInputRef.current.value = "";
    if (picked === null) return;

    // 小写扩展名; 无扩展名为空串
    const dot = picked.name.lastIndexOf(".");
    const extension = dot >= 0 ? picked.name.slice(dot + 1).toLowerCase() : "";
    if (!SUPPORTED_EXTENSIONS.includes(extension)) {
      setError(
        extension.length === 0
          ? `文件没有扩展名, 请选择 ${SUPPORTED_LABEL} 书籍文件`
          : `不支持导入 ${extension.toUpperCase()} 格式的书籍文件, 请选择 ${SUPPORTED_LABEL}`,
      );
      return;
    }

    setPhase("uploading");
    setError(null);
    const controller = new AbortController();
    abortRef.current = controller;
    try {
      const result = await importBookPreview(picked, { signal: controller.signal });
      setFile(picked);
      setPreview(result);
      setName(result.name);
      setAuthor(result.author);
      setPhase("preview");
    } catch (uploadError) {
      // 用户主动关闭对话框触发的中止不提示
      if (controller.signal.aborted) return;
      setError(errorMessage(uploadError, "上传书籍文件失败"));
      setPhase("pick");
    } finally {
      abortRef.current = null;
    }
  };

  /** 预览态取消: 后端预览不落盘, 直接回到选文件 */
  const cancelPreview = () => {
    reset();
    if (fileInputRef.current !== null) fileInputRef.current.value = "";
  };

  const chapterCount = preview?.chapterCount ?? 0;

  const handleConfirm = async () => {
    if (preview === null || file === null || chapterCount === 0) return;
    const trimmedName = name.trim();
    if (trimmedName.length === 0) return;
    setPhase("saving");
    try {
      const uploaded = await uploadLocalBook(file);
      const trimmedAuthor = author.trim();
      if (uploaded.name !== trimmedName || uploaded.author !== trimmedAuthor) {
        // uploadLocalBook 按文件名/内容元数据定名, 用户改动经 saveBook 增量补存
        // (warp saveBook 返回 data=null, bookshelf.saveBook 会由入参构造乐观 Book)
        await saveBook({
          bookUrl: uploaded.bookUrl,
          origin: uploaded.origin,
          name: trimmedName,
          author: trimmedAuthor,
        });
      }
      // 书架真值(分组/时间/章节数等)以服务端为准, invalidate 后由 ShelfPage 重拉
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
      toast.success(`《${trimmedName}》已加入书架`);
      onOpenChange(false);
      reset();
    } catch (saveError) {
      setError(errorMessage(saveError, "加入书架失败"));
      setPhase("preview");
    }
  };

  const showPreview = (phase === "preview" || phase === "saving") && preview !== null;
  const canConfirm = name.trim().length > 0 && chapterCount > 0 && phase !== "saving";

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent width="md">
        <DialogHeader>
          <DialogTitle>导入本地书籍</DialogTitle>
          <DialogDescription>上传 TXT / EPUB 文件, 确认后加入书架</DialogDescription>
        </DialogHeader>

        <div className="px-4 md:px-5">
          <input
            ref={fileInputRef}
            type="file"
            accept={ACCEPT_EXTENSIONS}
            className="hidden"
            aria-hidden
            tabIndex={-1}
            onChange={(event) => void handleFileChange(event.target.files)}
          />

          {phase === "pick" ? (
            error !== null ? (
              <EmptyState
                compact
                icon={<CircleAlert aria-hidden />}
                title="无法导入"
                description={error}
                action={
                  <Button size="sm" variant="secondary" onClick={pickFile}>
                    <FileUp aria-hidden />
                    重新选择文件
                  </Button>
                }
              />
            ) : (
              <button
                type="button"
                onClick={pickFile}
                className="flex w-full cursor-pointer flex-col items-center gap-2 rounded-xl border border-dashed border-border px-4 py-10 text-muted-foreground transition-colors hover:border-accent hover:text-foreground"
              >
                <FileUp aria-hidden className="size-8" />
                <span className="text-sm">点击选择书籍文件</span>
                <span className="text-xs">支持 {SUPPORTED_LABEL} 格式</span>
              </button>
            )
          ) : null}

          {phase === "uploading" ? (
            <div className="flex flex-col items-center gap-3 px-4 py-10">
              <Spinner size="lg" label="上传解析中" />
              <p className="text-sm text-muted-foreground">正在上传并解析书籍…</p>
            </div>
          ) : null}

          {showPreview && preview !== null ? (
            <div className="flex flex-col gap-3">
              {error !== null ? (
                <p className="rounded-lg bg-danger/10 px-3 py-2 text-sm text-danger" role="alert">
                  {error}
                </p>
              ) : null}
              <label className="flex flex-col gap-1.5">
                <span className="text-sm text-muted-foreground">书名</span>
                <Input
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  aria-label="书名"
                  placeholder="书名"
                  disabled={phase === "saving"}
                />
              </label>
              <label className="flex flex-col gap-1.5">
                <span className="text-sm text-muted-foreground">作者</span>
                <Input
                  value={author}
                  onChange={(event) => setAuthor(event.target.value)}
                  aria-label="作者"
                  placeholder="作者(可留空)"
                  disabled={phase === "saving"}
                />
              </label>
              <p className="text-sm text-muted-foreground">
                {chapterCount > 0
                  ? `已解析出 ${String(chapterCount)} 个章节`
                  : "未解析出章节, 无法加入书架"}
              </p>
            </div>
          ) : null}
        </div>

        <DialogFooter>
          {showPreview ? (
            <>
              <Button variant="ghost" disabled={phase === "saving"} onClick={cancelPreview}>
                取消
              </Button>
              <Button loading={phase === "saving"} disabled={!canConfirm} onClick={() => void handleConfirm()}>
                加入书架
              </Button>
            </>
          ) : (
            <Button variant="ghost" disabled={phase === "uploading"} onClick={() => handleOpenChange(false)}>
              关闭
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
