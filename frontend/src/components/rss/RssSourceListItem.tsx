import { Pencil, Trash2 } from "lucide-react";

import { Badge, IconButton, Switch, cn } from "@/components/ui";
import type { RssSource } from "@/services/rss";

export interface RssSourceListItemProps {
  source: RssSource;
  /** 当前选中(右侧/下层正在展示其文章) */
  selected: boolean;
  /** 本行启用开关的忙碌态(切换请求进行中) */
  busy: boolean;
  onSelect: (source: RssSource) => void;
  onToggleEnabled: (source: RssSource, enabled: boolean) => void;
  onEdit: (source: RssSource) => void;
  onDelete: (source: RssSource) => void;
}

/** 订阅列表行(父级 ul 提供 divide-y 分隔): 点击主体查看文章; 名称 + 分组徽标 + 地址, 右侧编辑/删除与启用开关 */
export function RssSourceListItem({
  source,
  selected,
  busy,
  onSelect,
  onToggleEnabled,
  onEdit,
  onDelete,
}: RssSourceListItemProps) {
  const name = source.sourceName.length > 0 ? source.sourceName : source.sourceUrl;
  const group = source.sourceGroup.trim();

  return (
    <li
      className={cn(
        "flex items-center gap-2 px-2 py-2.5 transition-colors duration-150 ease-out",
        selected ? "bg-accent/5" : "hover:bg-surface-muted/60",
      )}
    >
      <button
        type="button"
        aria-current={selected || undefined}
        className="min-w-0 flex-1 cursor-pointer rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() => onSelect(source)}
      >
        <span className="flex items-center gap-1.5">
          <span
            className={cn(
              "truncate text-sm font-medium",
              !source.enabled
                ? "text-muted-foreground"
                : selected
                  ? "text-accent"
                  : undefined,
            )}
            title={name}
          >
            {name}
          </span>
          {group.length > 0 ? (
            <Badge size="sm" variant="outline" className="max-w-28 shrink-0 truncate" title={group}>
              {group}
            </Badge>
          ) : null}
          {!source.enabled ? (
            <Badge size="sm" variant="muted" className="shrink-0">
              停用
            </Badge>
          ) : null}
        </span>
        <span className="mt-0.5 block truncate text-xs text-muted-foreground" title={source.sourceUrl}>
          {source.sourceUrl}
        </span>
      </button>

      <span className="flex shrink-0 items-center gap-0.5">
        <IconButton size="sm" variant="ghost" tooltip="编辑" onClick={() => onEdit(source)}>
          <Pencil aria-hidden />
        </IconButton>
        <IconButton
          size="sm"
          variant="ghost"
          tooltip="删除"
          className="text-danger hover:bg-danger/10 hover:text-danger"
          onClick={() => onDelete(source)}
        >
          <Trash2 aria-hidden />
        </IconButton>
        <Switch
          size="sm"
          className="ml-1"
          checked={source.enabled}
          disabled={busy}
          aria-label={`${source.enabled ? "停用" : "启用"} ${name}`}
          onCheckedChange={(enabled) => onToggleEnabled(source, enabled)}
        />
      </span>
    </li>
  );
}
