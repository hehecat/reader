import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeftRight, BookMarked, BookOpen, ImagePlus, Play, Trash2 } from "lucide-react";
import * as React from "react";

import { SwitchSourceDialog } from "@/components/book/SwitchSourceDialog";

import {
  Badge,
  Button,
  CoverImage,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from "@/components/ui";
import { toast } from "@/components/ui/Toast";
import {
  LOCAL_ORIGIN,
  bookCover,
  bookGroupNames,
} from "@/hooks/useBookshelf";
import { BOOKS_QUERY_KEY, errorMessage } from "@/hooks/useBookshelf";
import { humanizeError } from "@/lib/errors";
import { saveBook } from "@/services/bookshelf";
import type { Book, BookGroup } from "@/types/api";

const dayFormatter = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
});

export interface BookInfoDialogProps {
  /** 为 null 时弹窗保持关闭 */
  book: Book | null;
  /** 自定义分组, 用于把 book.group 位掩码翻译成组名 */
  groups?: BookGroup[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onStartReading?: (book: Book) => void;
  /** 请求删除(由页面统一确认后执行) */
  onDelete?: (book: Book) => void;
  /** 设为读完: 与卡片菜单动作集一致 */
  onMarkRead?: (book: Book) => void;
}

/** 书籍详情: 封面 + 元信息 + 简介, 底部「开始阅读 / 删除书籍」 */
export function BookInfoDialog({
  book,
  groups = [],
  open,
  onOpenChange,
  onStartReading,
  onDelete,
  onMarkRead,
}: BookInfoDialogProps) {
  const queryClient = useQueryClient();
  const [coverEdit, setCoverEdit] = React.useState(false);
  const [switchOpen, setSwitchOpen] = React.useState(false);
  const [coverDraft, setCoverDraft] = React.useState("");
  const saveCover = useMutation({
    mutationFn: (url: string) =>
      book === null ? Promise.resolve(null) : saveBook({ ...book, customCoverUrl: url }),
    onSuccess: () => {
      toast.success("封面已更新");
      setCoverEdit(false);
      void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
    },
    onError: (error) => toast.error(humanizeError(errorMessage(error, "封面保存失败"))),
  });
  if (book === null) {
    return <Dialog open={false} onOpenChange={onOpenChange} />;
  }

  const unread = book.totalChapterNum - 1 - book.durChapterIndex;
  const intro = book.customIntro ?? book.intro ?? "";
  const groupNames = bookGroupNames(book, groups);

  const meta = [
    { label: "作者", value: book.author || "未知" },
    { label: "书源", value: book.originName || book.origin },
    { label: "分组", value: groupNames.length > 0 ? groupNames.join("、") : "未分组" },
    { label: "最新章节", value: book.latestChapterTitle ?? "—" },
    {
      label: "更新时间",
      value: book.latestChapterTime > 0 ? dayFormatter.format(book.latestChapterTime) : "—",
    },
    {
      label: "阅读进度",
      value:
        book.totalChapterNum > 0
          ? book.durChapterTitle?.trim() || book.durChapterIndex > 0
            ? `${book.durChapterIndex + 1} / ${book.totalChapterNum} 章`
            : `共 ${book.totalChapterNum} 章`
          : "尚未读取目录",
    },
    {
      label: "读到",
      value:
        book.durChapterTitle?.trim() ||
        (book.durChapterIndex > 0 ? `第 ${book.durChapterIndex + 1} 章` : "—"),
    },
  ];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="md">
        <DialogHeader>
          <DialogTitle className="font-display line-clamp-2 break-words">{book.name}</DialogTitle>
          <DialogDescription className="flex flex-wrap items-center gap-1.5">
            {book.type === 1 ? <Badge variant="accent">音频</Badge> : null}
            {book.origin === LOCAL_ORIGIN ? <Badge variant="muted">本地</Badge> : null}
            {unread > 0 ? <Badge variant="accent">未读 {unread > 99 ? "99+" : unread}</Badge> : null}
            {unread <= 0 && book.totalChapterNum > 0 ? <Badge variant="accent">已读完</Badge> : null}
          </DialogDescription>
        </DialogHeader>

        <div className="flex gap-4 px-4 md:px-5">
          <CoverImage
            src={bookCover(book)}
            alt={book.name}
            className="w-24 shrink-0 shadow-md ring-1 ring-border/70 sm:w-28"
          />
          <dl className="grid min-w-0 flex-1 grid-cols-[auto_minmax(0,1fr)] content-start gap-x-3 gap-y-1.5 text-sm">
            {meta.map((item) => (
              <React.Fragment key={item.label}>
                <dt className="shrink-0 text-muted-foreground">{item.label}</dt>
                <dd className="min-w-0 truncate">{item.value}</dd>
              </React.Fragment>
            ))}
          </dl>
        </div>

        <div className="mt-3 flex items-center gap-2 px-4 md:px-5">
          {coverEdit ? (
            <>
              <Input
                aria-label="封面图地址"
                placeholder="封面图地址, 留空恢复源站封面"
                value={coverDraft}
                onChange={(event) => setCoverDraft(event.target.value)}
              />
              <Button
                size="sm"
                loading={saveCover.isPending}
                onClick={() => saveCover.mutate(coverDraft.trim())}
              >
                保存
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setCoverEdit(false)}>
                取消
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                setCoverDraft(book.customCoverUrl ?? "");
                setCoverEdit(true);
              }}
            >
              <ImagePlus aria-hidden />
              换封面
            </Button>
          )}
          {book.origin === LOCAL_ORIGIN ? null : (
            <Button size="sm" variant="secondary" onClick={() => setSwitchOpen(true)}>
              <ArrowLeftRight aria-hidden />
              换源
            </Button>
          )}
        </div>

        <div className="mt-4 px-4 md:px-5">
          <h3 className="text-sm font-medium">简介</h3>
          <p className="mt-1.5 text-sm leading-6 whitespace-pre-line text-muted-foreground">
            {intro.length > 0 ? (
              intro
            ) : (
              <span className="inline-flex items-center gap-1.5">
                <BookOpen aria-hidden className="size-4" />
                暂无简介
              </span>
            )}
          </p>
        </div>

        <DialogFooter>
          <Button variant="danger" onClick={() => onDelete?.(book)}>
            <Trash2 aria-hidden />
            删除书籍
          </Button>
          <Button variant="secondary" disabled={unread <= 0} onClick={() => onMarkRead?.(book)}>
            <BookMarked aria-hidden />
            设为读完
          </Button>
          <Button onClick={() => onStartReading?.(book)}>
            <Play aria-hidden />
            开始阅读
          </Button>
        </DialogFooter>
      </DialogContent>
      {/* 换源后面板里的 bookUrl 已失效, 关闭详情由书架列表反映新源 */}
      <SwitchSourceDialog
        book={book}
        open={switchOpen}
        onOpenChange={setSwitchOpen}
        onSwitched={() => onOpenChange(false)}
      />
    </Dialog>
  );
}
