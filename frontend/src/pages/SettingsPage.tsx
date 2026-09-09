import { useQuery } from "@tanstack/react-query";

import { ReadingPreferencesCard } from "@/components/settings/ReadingPreferencesCard";
import { TtsPreferencesCard } from "@/components/settings/TtsPreferencesCard";
import { WebdavPanel } from "@/components/settings/WebdavPanel";
import { PageIntro, SettingCard, SettingRow } from "@/components/ui";
import { getSystemInfo } from "@/services/auth";
import { useSettingsStore } from "@/stores/settings-store";
import { version as appVersion } from "../../package.json";

/** 秒 → 「N 天 N 小时」 / 「N 小时 N 分」 / 「N 分 N 秒」 / 「N 秒」 */
function formatUptime(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds));
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  if (days > 0) {
    return `${days} 天 ${hours} 小时`;
  }
  if (hours > 0) {
    return `${hours} 小时 ${minutes} 分`;
  }
  if (minutes > 0) {
    return `${minutes} 分 ${seconds % 60} 秒`;
  }
  return `${seconds} 秒`;
}

/**
 * 设置页: 阅读偏好 + 朗读与音色 (独立卡) + 数据与备份 (WebDAV) + 关于.
 */
export default function SettingsPage() {
  const searchTimeout = useSettingsStore((state) => state.searchTimeout);
  const setSearchTimeout = useSettingsStore((state) => state.setSearchTimeout);
  const systemQuery = useQuery({
    queryKey: ["systemInfo"],
    queryFn: () => getSystemInfo(),
    staleTime: Infinity,
  });
  const systemInfo = systemQuery.data;
  const uptime = systemInfo?.uptimeSeconds;
  const memory = systemInfo?.memory;

  return (
    <div className="mx-auto w-full max-w-5xl px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="READER PREFERENCES"
        title="设置"
        desc="调整阅读体验与数据备份."
      />
      <div className="grid items-start gap-5 lg:grid-cols-2">
        <div className="flex flex-col gap-5">
          <SettingCard title="搜索" desc="多源搜索运行时参数, 保存即生效">
            <SettingRow label="单源搜索超时" value="秒 (3-60): 慢源站响应慢时调高, 想快速跳过慢源调低">
              <input
                type="number"
                min={3}
                max={60}
                value={searchTimeout}
                onChange={(event) => setSearchTimeout(Number(event.target.value))}
                className="w-24 rounded-md border border-border bg-background px-3 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
              />
            </SettingRow>
          </SettingCard>
          <ReadingPreferencesCard />
          <TtsPreferencesCard />
        </div>
        <div className="flex flex-col gap-5">
          <WebdavPanel />
          <SettingCard title="关于" desc="砚台 · 安静地陪你读完每一本">
            <SettingRow label="前端" value={`砚台 React 阅读器 · v${appVersion}`} />
            <SettingRow label="后端" value="warp Rust 后端" />
            {uptime !== undefined ? (
              <SettingRow label="运行时长" value={formatUptime(uptime)} />
            ) : null}
            {memory !== undefined ? (
              <SettingRow
                label="服务器内存"
                value={`已用 ${memory.percent ?? 0}% · 可用 ${memory.availableMb ?? 0} MB / 共 ${memory.totalMb ?? 0} MB`}
              />
            ) : null}
          </SettingCard>
        </div>
      </div>
    </div>
  );
}
