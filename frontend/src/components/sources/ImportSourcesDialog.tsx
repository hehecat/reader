import { useMutation } from "@tanstack/react-query";
import { FileJson, Link } from "lucide-react";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Spinner,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
  toast,
} from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import {
  parseBookSourcesText,
  previewRemoteSources,
  type RemoteSourcePreview,
} from "@/services/sources";
import type { BookSource } from "@/types/api";

import { sourceErrorMessage, useSaveBookSources } from "./useSources";

export interface ImportSourcesDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const JSON_PLACEHOLDER = `支持 legado 书源 JSON, 数组或单个对象:
[
  {
    "bookSourceUrl": "https://example.com",
    "bookSourceName": "示例书源",
    "searchUrl": "/search?key={{key}}",
    "ruleSearch": { "bookList": ".item", "name": ".name@text" }
  }
]`;

/** 导入书源对话框: 粘贴 JSON 或读取远程书源文件(先预览条数再确认导入) */
export function ImportSourcesDialog({ open, onOpenChange }: ImportSourcesDialogProps) {
  const [tab, setTab] = React.useState<"json" | "remote">("json");
  const [jsonText, setJsonText] = React.useState("");
  const [remoteUrl, setRemoteUrl] = React.useState("");
  const [remotePreview, setRemotePreview] = React.useState<RemoteSourcePreview | null>(null);
  const save = useSaveBookSources();

  const readRemote = useMutation({
    mutationFn: (url: string) => previewRemoteSources(url),
    onSuccess: (result) => setRemotePreview(result),
    onError: (error) => {
      toast.error(sourceErrorMessage(error, "读取远程书源文件失败"));
    },
  });

  const parsed = React.useMemo(() => parseBookSourcesText(jsonText), [jsonText]);

  const closeAndReset = () => {
    setJsonText("");
    setRemoteUrl("");
    setRemotePreview(null);
    onOpenChange(false);
  };

  const handleOpenChange = (next: boolean) => {
    if (next) onOpenChange(next);
    else closeAndReset();
  };

  const importSources = (list: BookSource[]) => {
    save.mutate(list, {
      onSuccess: () => {
        toast.success(`已导入 ${list.length} 个书源`);
        closeAndReset();
      },
    });
  };

  const jsonCount = parsed.sources.length;
  const remoteCount = remotePreview?.sources.length ?? 0;
  const confirmCount = tab === "json" ? jsonCount : remoteCount;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent width="lg">
        <DialogHeader>
          <DialogTitle>导入书源</DialogTitle>
          <DialogDescription>
            按书源地址去重合并, 同地址的已有书源会被覆盖更新
          </DialogDescription>
        </DialogHeader>

        <div className="px-4 md:px-5">
          <Tabs
            value={tab}
            onValueChange={(value) => setTab(value === "remote" ? "remote" : "json")}
          >
            <TabsList aria-label="导入方式">
              <TabsTrigger value="json">
                <FileJson aria-hidden />
                粘贴 JSON
              </TabsTrigger>
              <TabsTrigger value="remote">
                <Link aria-hidden />
                远程链接
              </TabsTrigger>
            </TabsList>

            <TabsContent value="json" className="mt-3 flex flex-col gap-1.5">
              <Textarea
                rows={12}
                className="font-mono text-xs leading-5"
                placeholder={JSON_PLACEHOLDER}
                value={jsonText}
                onChange={(event) => setJsonText(event.target.value)}
                invalid={parsed.error !== null}
                aria-label="书源 JSON 文本"
                spellCheck={false}
              />
              {parsed.error !== null ? (
                <p className="text-xs text-danger" role="alert" title={parsed.error}>
                  {humanizeError(parsed.error)}
                </p>
              ) : jsonText.trim().length > 0 ? (
                <p className="text-xs text-muted-foreground">
                  解析到{" "}
                  <span className="font-medium text-foreground tabular-nums">{jsonCount}</span>{" "}
                  个书源
                  {parsed.skipped > 0 ? `, ${parsed.skipped} 个条目无法识别将被跳过` : ""}
                </p>
              ) : null}
            </TabsContent>

            <TabsContent value="remote" className="mt-3 flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <Input
                  className="min-w-0 flex-1"
                  type="url"
                  placeholder="https://example.com/sources.json"
                  value={remoteUrl}
                  onChange={(event) => setRemoteUrl(event.target.value)}
                  aria-label="远程书源文件链接"
                  clearable
                  onClear={() => setRemoteUrl("")}
                  prefixIcon={<Link aria-hidden className="size-4 text-muted-foreground" />}
                />
                <Button
                  variant="secondary"
                  disabled={remoteUrl.trim().length === 0}
                  loading={readRemote.isPending}
                  onClick={() => {
                    setRemotePreview(null);
                    readRemote.mutate(remoteUrl.trim());
                  }}
                >
                  读取
                </Button>
              </div>

              {readRemote.isPending ? (
                <p
                  className="flex items-center gap-2 text-xs text-muted-foreground"
                  role="status"
                >
                  <Spinner size="sm" label="读取中" />
                  正在读取远程书源文件...
                </p>
              ) : remotePreview !== null ? (
                <div className="rounded-lg border border-border">
                  <p className="border-b border-border px-3 py-2 text-xs text-muted-foreground">
                    共解析到{" "}
                    <span className="font-medium text-foreground tabular-nums">
                      {remotePreview.sources.length}
                    </span>{" "}
                    个书源
                    {remotePreview.existing.length > 0
                      ? `, 其中 ${remotePreview.existing.length} 个已存在, 导入将覆盖更新`
                      : ""}
                  </p>
                  <ul className="max-h-44 overflow-y-auto px-3 py-2">
                    {remotePreview.sources.map((source) => (
                      <li key={source.bookSourceUrl} className="flex items-baseline gap-2 py-0.5">
                        <span className="shrink-0 text-xs font-medium">
                          {source.bookSourceName.length > 0
                            ? source.bookSourceName
                            : source.bookSourceUrl}
                        </span>
                        <span
                          className="truncate text-xs text-muted-foreground"
                          title={source.bookSourceUrl}
                        >
                          {source.bookSourceUrl}
                        </span>
                        {remotePreview.existing.includes(source.bookSourceUrl) ? (
                          <span className="shrink-0 rounded bg-surface-muted px-1 text-[10px] text-muted-foreground">
                            已存在
                          </span>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </TabsContent>
          </Tabs>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={closeAndReset}>
            取消
          </Button>
          <Button
            disabled={confirmCount === 0}
            loading={save.isPending}
            onClick={() =>
              importSources(tab === "json" ? parsed.sources : (remotePreview?.sources ?? []))
            }
          >
            {confirmCount > 0 ? `导入 ${confirmCount} 个书源` : "导入"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
