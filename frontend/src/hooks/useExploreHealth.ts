import * as React from "react";

import { getJSON, setJSON } from "@/lib/storage";

/** 书海源可用性记录: 每次探测结果按源地址存本机, TTL 内不再重复探测 */
interface ExploreHealthEntry {
  /** 该源最近一次发现分类探测是否返回内容 */
  ok: boolean;
  /** 记录时间戳(ms) */
  at: number;
}

const STORAGE_KEY = "reader.explore.health.v1";
/** 结果有效期 6 小时: 过期后重新探测(源站可能已恢复/失效) */
export const EXPLORE_HEALTH_TTL_MS = 6 * 60 * 60 * 1000;

export interface UseExploreHealthResult {
  /** 该源是否已判定"无内容"且仍在有效期内(应隐藏) */
  isHidden: (sourceUrl: string) => boolean;
  /** 记录一次探测结果 */
  mark: (sourceUrl: string, ok: boolean) => void;
  /** 已隐藏(无内容)的源数量 */
  hiddenCount: number;
  /** 清空全部记录(重新检测前调用) */
  reset: () => void;
}

/**
 * 书海自动筛选: 记录各书源发现分类是否真有内容, 供列表隐藏"空源".
 * 探测只发生在两处 —— ① 用户切到该源时后端返回空/失败; ② 用户点「重新检测」批量探测;
 * 不在此 hook 里发起任何请求(避免进入书海就打出几百个探测请求).
 */
export function useExploreHealth(): UseExploreHealthResult {
  const [map, setMap] = React.useState<Record<string, ExploreHealthEntry>>(
    () => getJSON<Record<string, ExploreHealthEntry>>(STORAGE_KEY) ?? {},
  );

  const persist = React.useCallback((next: Record<string, ExploreHealthEntry>) => {
    setMap(next);
    setJSON(STORAGE_KEY, next);
  }, []);

  const mark = React.useCallback(
    (sourceUrl: string, ok: boolean) => {
      if (sourceUrl === "") {
        return;
      }
      setMap((prev) => {
        const next = { ...prev, [sourceUrl]: { ok, at: Date.now() } };
        setJSON(STORAGE_KEY, next);
        return next;
      });
    },
    [],
  );

  const isHidden = React.useCallback(
    (sourceUrl: string) => {
      const entry = map[sourceUrl];
      if (entry === undefined) {
        return false;
      }
      if (Date.now() - entry.at > EXPLORE_HEALTH_TTL_MS) {
        return false; // 过期: 放行重新验证
      }
      return !entry.ok;
    },
    [map],
  );

  const hiddenCount = React.useMemo(
    () => Object.values(map).filter((entry) => !entry.ok && Date.now() - entry.at <= EXPLORE_HEALTH_TTL_MS).length,
    [map],
  );

  const reset = React.useCallback(() => {
    persist({});
  }, [persist]);

  return { isHidden, mark, hiddenCount, reset };
}
