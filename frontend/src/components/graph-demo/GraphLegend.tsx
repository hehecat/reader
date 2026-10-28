/**
 * 图例: 关系线型 + 门派配色 + 形状含义.
 *
 * 浮在画布右下角的折叠卡(默认收起, 只占一个「图例」按钮): 图例一旦常开就会把画布挤窄,
 * cyber 的 fit 又按容器算缩放, 结果整张图被缩到 60% 什么都不清.
 */
import { ChevronDown, Layers } from "lucide-react";

import { cn } from "@/components/ui/cn";

import type { GraphFaction } from "@/types/graphDemo";

import { NEUTRAL_COLOR, RELATION_ORDER, RELATION_VISUAL } from "./graphTheme";

export interface GraphLegendProps {
  factions: GraphFaction[];
  factionColors: Map<string, string>;
  open: boolean;
  onToggle: () => void;
  /** 当前视图里还活着的势力, 其它淡显(时间滑块切走后) */
  activeFactions?: Set<string>;
}

export function GraphLegend({ factions, factionColors, open, onToggle, activeFactions }: GraphLegendProps) {
  return (
    <div
      className="pointer-events-auto flex max-w-[calc(100vw-1.5rem)] flex-col overflow-hidden rounded-2xl border border-border bg-surface/92 shadow-sm backdrop-blur sm:w-56"
      data-testid="graph-legend"
    >
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className={cn(
          "flex w-full items-center justify-between gap-2 px-3 py-2 text-xs font-semibold text-foreground transition-colors hover:bg-surface-muted",
          open && "border-b border-border",
        )}
      >
        <span className="flex items-center gap-1.5">
          <Layers className="size-3.5 text-muted-foreground" />
          图例
        </span>
        <ChevronDown
          className={cn("size-3.5 text-muted-foreground transition-transform", !open && "-rotate-90")}
        />
      </button>

      {open ? (
        <div className="max-h-[min(58vh,26rem)] space-y-3 overflow-y-auto px-3 py-2.5 text-xs">
          <section>
            <p className="mb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground">关系线型</p>
            <ul className="space-y-1">
              {RELATION_ORDER.map((type) => {
                const visual = RELATION_VISUAL[type];
                return (
                  <li key={type} className="flex items-center gap-2">
                    <span className="relative flex h-3 w-8 shrink-0 items-center">
                      <span
                        className="w-full"
                        style={{
                          borderTopStyle: visual.lineStyle,
                          borderTopWidth: Math.max(1, Math.round(visual.width)),
                          borderTopColor: visual.color,
                        }}
                      />
                      {visual.arrow ? (
                        <span
                          aria-hidden
                          className="absolute -right-1 top-1/2 -translate-y-1/2 text-[9px] leading-none"
                          style={{ color: visual.color }}
                        >
                          ▶
                        </span>
                      ) : null}
                    </span>
                    <span className="text-foreground">{visual.name}</span>
                  </li>
                );
              })}
            </ul>
          </section>

          <section>
            <p className="mb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground">门派配色</p>
            <ul className="space-y-1">
              {factions.length === 0 ? <li className="text-muted-foreground">无势力数据</li> : null}
              {factions.map((faction) => {
                const color = factionColors.get(faction.id) ?? NEUTRAL_COLOR;
                const active = activeFactions ? activeFactions.has(faction.id) : true;
                return (
                  <li
                    key={faction.id}
                    className={cn("flex items-center gap-2", !active && "opacity-40")}
                  >
                    <span
                      className="size-2.5 shrink-0 rounded-full"
                      style={{ backgroundColor: color }}
                    />
                    <span className="truncate text-foreground">{faction.name}</span>
                    <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
                      {faction.kind}
                    </span>
                  </li>
                );
              })}
            </ul>
          </section>

          <section>
            <p className="mb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground">形状</p>
            <ul className="space-y-1 text-muted-foreground">
              <li>★ 主角 · ▢ 势力领袖 · ● 其余人物</li>
              <li>节点大小 ∝ 关系数量; 虚线框 = 势力分组</li>
            </ul>
          </section>
        </div>
      ) : null}
    </div>
  );
}
