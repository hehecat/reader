import {
  ArrowLeft,
  CheckSquare,
  ChevronRight,
  CircleAlert,
  CloudUpload,
  Download,
  File,
  FileArchive,
  Folder,
  Lock,
  RotateCcw,
  RotateCw,
  Square,
  Trash2,
  Upload,
} from "lucide-react";
import * as React from "react";

import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  IconButton,
  SkeletonList,
  cn,
  toast,
  Tooltip,
} from "@/components/ui";
import {
  downloadWebdavFile,
  type WebdavFile,
} from "@/services/webdav";

import {
  useBackupWebdav,
  useDeleteWebdavFiles,
  useRestoreWebdav,
  useUploadWebdavFiles,
  useWebdavAccess,
  useWebdavFiles,
  webdavErrorMessage,
} from "./useWebdav";

const dateFormatter = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});

const EMPTY_SELECTION: ReadonlySet<string> = new Set();

function formatFileSize(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** "/a/b" → "/a", "/a" → "/", 根目录保持不变 */
function parentPath(path: string): string {
  const trimmed = path.length > 1 && path.endsWith("/") ? path.slice(0, -1) : path;
  const index = trimmed.lastIndexOf("/");
  return index <= 0 ? "/" : trimmed.slice(0, index);
}

/** "/" → 根一段; "/a/b" → 根 + a + b(末段即当前目录) */
function breadcrumbSegments(path: string): Array<{ label: string; path: string }> {
  const parts = path.split("/").filter((part) => part.length > 0);
  const segments = [{ label: "/", path: "/" }];
  parts.forEach((part, index) => {
    segments.push({ label: part, path: `/${parts.slice(0, index + 1).join("/")}` });
  });
  return segments;
}

function isZipFile(file: WebdavFile): boolean {
  return !file.isDirectory && file.name.toLowerCase().endsWith(".zip");
}

/**
 * WebDAV 备份面板: 开关状态说明 + 一键备份 + 文件列表(上传/下载/恢复/删除/批量删除).
 *
 * 后端没有外部 WebDAV 配置通道(存储就是服务端 storage/data/<用户名>/webdav 目录,
 * 开关是用户 enable_webdav), 所以这里不提供 url/账号/密码表单, 只展示只读说明与状态.
 */
export function WebdavPanel() {
  const access = useWebdavAccess();
  const [currentPath, setCurrentPath] = React.useState("/");
  const { files, isLoading, isFetching, error, refetch } = useWebdavFiles(
    currentPath,
    access.enabled,
  );
  const backup = useBackupWebdav();
  const restore = useRestoreWebdav();
  const deleteFiles = useDeleteWebdavFiles();
  const upload = useUploadWebdavFiles();

  const [selected, setSelected] = React.useState<ReadonlySet<string>>(EMPTY_SELECTION);
  const [pendingRestore, setPendingRestore] = React.useState<WebdavFile | null>(null);
  const [pendingDelete, setPendingDelete] = React.useState<WebdavFile[] | null>(null);
  const [downloadingPath, setDownloadingPath] = React.useState<string | null>(null);
  const fileInputRef = React.useRef<HTMLInputElement>(null);

  const selectedFiles = React.useMemo(
    () => files.filter((file) => selected.has(file.path)),
    [files, selected],
  );

  const segments = React.useMemo(() => breadcrumbSegments(currentPath), [currentPath]);

  const toggleSelect = (file: WebdavFile) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(file.path)) {
        next.delete(file.path);
      } else {
        next.add(file.path);
      }
      return next;
    });
  };

  const goToPath = (path: string) => {
    setCurrentPath(path);
    setSelected(EMPTY_SELECTION);
  };

  const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const picked = Array.from(event.target.files ?? []);
    // 允许连续选同一批文件也能触发 change
    event.target.value = "";
    if (picked.length === 0) return;
    upload.mutate({ files: picked, path: currentPath });
  };

  const handleDownload = async (file: WebdavFile) => {
    setDownloadingPath(file.path);
    try {
      await downloadWebdavFile(file);
    } catch (downloadError) {
      toast.error(webdavErrorMessage(downloadError, "下载失败"));
    } finally {
      setDownloadingPath(null);
    }
  };

  const confirmRestore = () => {
    if (pendingRestore === null) return;
    // 成功后 hook 内会清缓存并广播登录失效跳转 /login, 对话框随页面卸载
    restore.mutate(pendingRestore.path);
  };

  const confirmDelete = () => {
    if (pendingDelete === null) return;
    deleteFiles.mutate(
      pendingDelete.map((file) => file.path),
      {
        onSuccess: () => {
          setPendingDelete(null);
          setSelected(EMPTY_SELECTION);
        },
      },
    );
  };

  const statusBadge = access.isLoading ? (
    <Badge variant="muted" size="sm">
      检测中
    </Badge>
  ) : access.enabled ? (
    <Badge variant="accent" size="sm">
      已开启
    </Badge>
  ) : (
    <Badge variant="danger" size="sm">
      未开启
    </Badge>
  );

  return (
    <section
      aria-labelledby="webdav-panel-title"
      className="flex flex-col gap-3 rounded-xl border border-border bg-surface p-5"
    >
      <header className="flex flex-wrap items-center gap-2">
        <CloudUpload aria-hidden className="size-5 shrink-0 text-accent" />
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h2 id="webdav-panel-title" className="font-display text-lg font-semibold">
              数据与备份
            </h2>
            {statusBadge}
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">
            WebDAV 同步书架与阅读进度, 可随时恢复或管理备份文件
          </p>
        </div>
        <div className="ml-auto flex items-center gap-1.5">
          <IconButton
            variant="ghost"
            size="sm"
            aria-label="刷新文件列表"
            tooltip="刷新"
            disabled={!access.enabled || isLoading || isFetching}
            onClick={refetch}
          >
            <RotateCw aria-hidden className={cn(isFetching && "ui-spin")} />
          </IconButton>
          <Button
            size="sm"
            variant="secondary"
            disabled={!access.enabled}
            loading={upload.isPending}
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload aria-hidden />
            上传文件
          </Button>
          <Button
            size="sm"
            disabled={!access.enabled}
            loading={backup.isPending}
            onClick={() => backup.mutate()}
          >
            <CloudUpload aria-hidden />
            一键备份
          </Button>
        </div>
      </header>

      <div className="flex items-center gap-2 text-xs leading-5 text-muted-foreground">
        <code className="w-fit max-w-full truncate rounded bg-surface-muted px-1 py-0.5 text-foreground">
          {window.location.origin}/reader3/webdav/
        </code>
        <Tooltip
          side="bottom"
          contentClassName="max-w-64 text-xs leading-5"
          content={
            <>
              一键备份写入根目录 legado/backup-&lt;时间戳&gt;.zip, 不覆盖旧备份. 认证 = HTTP
              Basic: 用户名=登录名, 密码=登录密码. secure 模式每人仅自己的空间(需 WebDAV
              权限); 非 secure 单 default 空间免认证. 地址同源动态, 随访问域名变化.
            </>
          }
        >
          <button
            type="button"
            aria-label="WebDAV 使用说明"
            className="shrink-0 cursor-help outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <CircleAlert aria-hidden className="size-3.5 text-muted-foreground" />
          </button>
        </Tooltip>
      </div>

      <input
        ref={fileInputRef}
        type="file"
        multiple
        tabIndex={-1}
        className="hidden"
        aria-hidden
        onChange={handleFileChange}
      />

      {access.enabled ? (
        <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted-foreground">
          <span className="shrink-0">当前:</span>
          <nav
            aria-label="WebDAV 目录路径"
            className="flex min-w-0 flex-wrap items-center gap-x-1"
          >
            {segments.map((segment, index) => (
              <React.Fragment key={segment.path}>
                {index > 0 ? (
                  <ChevronRight aria-hidden className="size-3 shrink-0 text-muted-foreground/60" />
                ) : null}
                {index === segments.length - 1 ? (
                  <span
                    aria-current="location"
                    className="min-w-0 truncate font-medium text-foreground"
                  >
                    {segment.label}
                  </span>
                ) : (
                  <button
                    type="button"
                    className="shrink-0 cursor-pointer rounded outline-none transition duration-150 ease-out hover:text-accent hover:underline focus-visible:ring-2 focus-visible:ring-ring"
                    onClick={() => goToPath(segment.path)}
                  >
                    {segment.label}
                  </button>
                )}
              </React.Fragment>
            ))}
          </nav>
          <span className="ml-auto shrink-0">上传写入此目录; 一键备份固定写入 legado/</span>
        </div>
      ) : null}

      {!access.enabled ? (
        access.isLoading ? (
          <SkeletonList count={2} className="py-2" />
        ) : access.error !== null ? (
          <EmptyState
            compact
            icon={<CircleAlert aria-hidden />}
            title="无法获取 WebDAV 状态"
            description={webdavErrorMessage(access.error, "网络异常或登录态已失效")}
            action={
              <Button size="sm" variant="secondary" onClick={access.refetch}>
                <RotateCw aria-hidden />
                重试
              </Button>
            }
          />
        ) : (
          <EmptyState
            compact
            icon={<Lock aria-hidden />}
            title="WebDAV 功能未开启"
            description="当前账号未开启 WebDAV 功能, 请联系管理员在用户管理中开启后再使用备份与恢复."
          />
        )
      ) : isLoading ? (
        <SkeletonList count={3} className="py-2" />
      ) : error !== null ? (
        <EmptyState
          compact
          icon={<CircleAlert aria-hidden />}
          title="文件列表加载失败"
          description={webdavErrorMessage(error, "网络异常或登录态已失效")}
          action={
            <Button size="sm" variant="secondary" onClick={refetch}>
              <RotateCw aria-hidden />
              重试
            </Button>
          }
        />
      ) : files.length === 0 && currentPath === "/" ? (
        <EmptyState
          compact
          icon={<FileArchive aria-hidden />}
          title="暂无备份文件"
          description="点击「一键备份」即在 legado/ 目录生成 backup-<UTC 时间戳>.zip; 也可上传已有的备份 zip 后恢复."
        />
      ) : (
        <>
          {selected.size > 0 ? (
            <div className="flex items-center gap-2 rounded-lg bg-surface-muted px-3 py-2 text-xs text-muted-foreground">
              <span>
                已选
                <span className="mx-1 font-medium text-foreground tabular-nums">
                  {selectedFiles.length}
                </span>
                项
              </span>
              <Button
                size="sm"
                variant="danger"
                className="ml-auto"
                disabled={selectedFiles.length === 0}
                onClick={() => setPendingDelete(selectedFiles)}
              >
                <Trash2 aria-hidden />
                批量删除
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setSelected(EMPTY_SELECTION)}>
                取消选择
              </Button>
            </div>
          ) : null}

          <ul className="flex flex-col divide-y divide-border">
            {currentPath !== "/" ? (
              <li>
                <button
                  type="button"
                  className="flex w-full cursor-pointer items-center gap-2 px-3 py-2.5 text-sm hover:bg-surface-muted"
                  onClick={() => goToPath(parentPath(currentPath))}
                >
                  <ArrowLeft aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  <span className="truncate">返回上级</span>
                  <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                    {parentPath(currentPath)}
                  </span>
                </button>
              </li>
            ) : null}
            {files.map((file) => {
              const isSelected = selected.has(file.path);
              return (
                <li key={file.path} className="flex items-center gap-2 px-3 py-2.5">
                  <button
                    type="button"
                    role="checkbox"
                    aria-checked={isSelected}
                    aria-label={`选择 ${file.name}`}
                    className="relative grid size-4 shrink-0 cursor-pointer place-items-center after:absolute after:-inset-3.5 after:content-['']"
                    onClick={() => toggleSelect(file)}
                  >
                    {isSelected ? (
                      <CheckSquare aria-hidden className="size-4 text-accent" />
                    ) : (
                      <Square aria-hidden className="size-4 text-muted-foreground" />
                    )}
                  </button>
                  {file.isDirectory ? (
                    <Folder aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  ) : isZipFile(file) ? (
                    <FileArchive aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  ) : (
                    <File aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  )}
                  {file.isDirectory ? (
                    <button
                      type="button"
                      className="min-w-0 cursor-pointer truncate text-sm hover:text-accent hover:underline"
                      onClick={() => goToPath(file.path)}
                    >
                      {file.name}
                    </button>
                  ) : (
                    <span className="min-w-0 truncate text-sm">{file.name}</span>
                  )}
                  <span className="ml-auto hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:inline">
                    {file.isDirectory ? "-" : formatFileSize(file.size)}
                  </span>
                  <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums md:inline">
                    {dateFormatter.format(file.lastModified)}
                  </span>
                  <span className="flex shrink-0 items-center gap-0.5">
                    {file.isDirectory ? null : (
                      <IconButton
                        variant="ghost"
                        size="sm"
                        tooltip="下载"
                        disabled={downloadingPath === file.path}
                        onClick={() => void handleDownload(file)}
                      >
                        <Download aria-hidden className={cn(downloadingPath === file.path && "ui-spin")} />
                      </IconButton>
                    )}
                    {isZipFile(file) ? (
                      <IconButton
                        variant="ghost"
                        size="sm"
                        tooltip="恢复"
                        onClick={() => setPendingRestore(file)}
                      >
                        <RotateCcw aria-hidden />
                      </IconButton>
                    ) : null}
                    <IconButton
                      variant="ghost"
                      size="sm"
                      tooltip="删除"
                      className="hover:text-danger"
                      onClick={() => setPendingDelete([file])}
                    >
                      <Trash2 aria-hidden />
                    </IconButton>
                  </span>
                </li>
              );
            })}
          </ul>
          {files.length === 0 ? (
            <p className="text-center text-xs text-muted-foreground">该目录为空</p>
          ) : null}
        </>
      )}

      <Dialog
        open={pendingRestore !== null}
        onOpenChange={(open) => {
          if (!open && !restore.isPending) setPendingRestore(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>从备份恢复</DialogTitle>
            <DialogDescription>
              将用备份包「{pendingRestore?.name ?? ""}」覆盖当前的书架、书源、分组、书签、
              RSS 订阅、替换规则等全部数据, 备份里没有的内容会丢失. 恢复完成后需要重新登录.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={restore.isPending}
              onClick={() => setPendingRestore(null)}
            >
              取消
            </Button>
            <Button variant="danger" loading={restore.isPending} onClick={confirmRestore}>
              <RotateCcw aria-hidden />
              确认恢复
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open && !deleteFiles.isPending) setPendingDelete(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>删除文件</DialogTitle>
            <DialogDescription>
              {pendingDelete !== null && pendingDelete.length === 1
                ? `确定删除「${pendingDelete[0]?.name ?? ""}」吗? 删除后不可恢复.`
                : `确定删除选中的 ${pendingDelete?.length ?? 0} 个文件/目录吗? 删除后不可恢复.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={deleteFiles.isPending}
              onClick={() => setPendingDelete(null)}
            >
              取消
            </Button>
            <Button variant="danger" loading={deleteFiles.isPending} onClick={confirmDelete}>
              <Trash2 aria-hidden />
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
