import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  SettingCard,
  SettingRow,
  toast,
} from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import { clearCache, getCacheInfo, type CacheInfo, type ClearCacheType } from "@/services/cache";

const CACHE_INFO_KEY = ["cacheInfo"] as const;

/** 字节 → 人类可读 (后端给的是近似字节数, 一位小数足够) */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(units.length - 1, Math.max(0, Math.floor(Math.log(bytes) / Math.log(1024))));
  const value = bytes / 1024 ** exponent;
  return `${value >= 10 || exponent === 0 ? Math.round(value) : value.toFixed(1)} ${units[exponent] ?? "B"}`;
}

const CLEAR_LABEL: Record<ClearCacheType, string> = {
  toc: "目录缓存",
  chapters: "正文缓存",
  all: "全部缓存",
};

/** 确认对话框文案: 带上条数, 说清后果 (清理只影响缓存, 不动书架与进度) */
function describeClear(type: ClearCacheType, info: CacheInfo | undefined): string {
  if (type === "toc") {
    return `确定清除${info === undefined ? "" : ` ${info.tocCacheCount} 份`}目录缓存吗? 已缓存的书籍下次打开会重新抓取目录.`;
  }
  if (type === "chapters") {
    return `确定清除${info === undefined ? "" : ` ${info.chapterCount} 章`}正文缓存吗? 已缓存的章节下次阅读会重新抓取.`;
  }
  return "确定清除目录与正文缓存吗? 不会影响书架与阅读进度, 但下次打开书籍会重新抓取.";
}

/**
 * 设置页「缓存」卡片: 服务端目录/正文缓存统计 + 分类清理.
 * 清理是不可逆操作, 三个入口都走确认对话框.
 */
export function CacheCard() {
  const queryClient = useQueryClient();
  const cacheQuery = useQuery({
    queryKey: CACHE_INFO_KEY,
    queryFn: () => getCacheInfo(),
    staleTime: 30_000,
  });
  const info = cacheQuery.data;
  const [pending, setPending] = React.useState<ClearCacheType | null>(null);

  const clear = useMutation({
    mutationFn: (type: ClearCacheType) => clearCache(type),
    onSuccess: (_data, type) => {
      setPending(null);
      toast.success(`已清除${CLEAR_LABEL[type]}`);
      void queryClient.invalidateQueries({ queryKey: CACHE_INFO_KEY });
    },
    onError: (error) => {
      toast.error(
        humanizeError(error instanceof Error ? error.message : String(error), "清除缓存失败"),
      );
    },
  });

  return (
    <>
      <SettingCard title="缓存" desc="服务端保存的目录与正文缓存; 清理后下次阅读会重新抓取">
        {cacheQuery.isPending ? (
          <SettingRow label="缓存统计" value="读取中…" />
        ) : cacheQuery.isError ? (
          <SettingRow label="缓存统计" value="读取失败, 请稍后重试">
            <Button size="sm" variant="secondary" onClick={() => void cacheQuery.refetch()}>
              重试
            </Button>
          </SettingRow>
        ) : (
          <>
            <SettingRow
              label="目录缓存"
              value={`${info?.tocCacheCount ?? 0} 份 · ${formatBytes(info?.tocCacheSize ?? 0)}`}
            >
              <Button size="sm" variant="secondary" onClick={() => setPending("toc")}>
                清目录缓存
              </Button>
            </SettingRow>
            <SettingRow
              label="正文缓存"
              value={`${info?.chapterCount ?? 0} 章 · ${formatBytes(info?.chapterSize ?? 0)}`}
            >
              <Button size="sm" variant="secondary" onClick={() => setPending("chapters")}>
                清正文缓存
              </Button>
            </SettingRow>
            <SettingRow label="缓存总量" value={formatBytes(info?.totalSize ?? 0)}>
              <Button size="sm" variant="secondary" onClick={() => setPending("all")}>
                全部清除
              </Button>
            </SettingRow>
          </>
        )}
      </SettingCard>

      <Dialog
        open={pending !== null}
        onOpenChange={(open) => {
          if (!open && !clear.isPending) {
            setPending(null);
          }
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>清除{CLEAR_LABEL[pending ?? "all"]}</DialogTitle>
            <DialogDescription>{describeClear(pending ?? "all", info)}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" disabled={clear.isPending} onClick={() => setPending(null)}>
              取消
            </Button>
            <Button
              variant="danger"
              loading={clear.isPending}
              onClick={() => {
                if (pending !== null) {
                  clear.mutate(pending);
                }
              }}
            >
              确认清除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
