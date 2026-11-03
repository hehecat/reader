import { ChevronRight } from "lucide-react";

import { Tabs, TabsList, TabsTrigger } from "@/components/ui";

import type { SourceGroupItem } from "./useSources";

export interface SourceGroupTabsProps {
  /** buildSourceGroups 聚合出的分组(含「全部」) */
  groups: SourceGroupItem[];
  value: string;
  onChange: (value: string) => void;
  /** 列表容器的 id: tab 通过 aria-controls 指向它 */
  controlsId?: string;
  className?: string;
}

/** 书源分组标签栏: 全部 + 各 bookSourceGroup + 未分组; 轨道按内容收缩(桌面不留死色带), 窄屏横向滚动; 只剩「全部」时整条隐藏 */
export function SourceGroupTabs({
  groups,
  value,
  onChange,
  controlsId,
  className,
}: SourceGroupTabsProps) {
  if (groups.length <= 1) return null;

  return (
    <Tabs value={value} onValueChange={onChange} className={className}>
      <div className="relative">
        <TabsList aria-label="书源分组" className="scroll-x-implicit h-12 pr-7 sm:pr-1">
          {groups.map((group) => (
            <TabsTrigger key={group.value} value={group.value} aria-controls={controlsId}>
              {group.name}
              <span aria-hidden className="text-xs tabular-nums">
                {group.count}
              </span>
            </TabsTrigger>
          ))}
        </TabsList>
        {/* 窄屏可滚动提示: 只提示可横向滑动, 不吃点击(pointer-events-none) */}
        <span
          aria-hidden
          className="pointer-events-none absolute inset-y-0 right-0 flex w-7 items-center justify-end rounded-r-xl bg-surface-muted pr-1 text-muted-foreground sm:hidden"
        >
          <ChevronRight className="size-4" />
        </span>
      </div>
    </Tabs>
  );
}
