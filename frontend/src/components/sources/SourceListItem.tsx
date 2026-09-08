import { BookText, KeyRound, Pencil, SquareTerminal, ToolCase, Trash2 } from "lucide-react";

import { Badge, IconButton, Switch, cn } from "@/components/ui";
import type { SourceStat } from "@/services/sources";
import type { BookSource } from "@/types/api";

export interface SourceListItemProps {
  source: BookSource;
  /** 是否被后端短期缓存标记为失效(近 10 分钟失败) */
  invalid: boolean;
  /** 长期统计失效(尝试≥5 且成功率<20%): 与 invalid 同显「失效」徽标 */
  dead?: boolean;
  /** 本行启用开关的忙碌态(切换请求进行中) */
  busy: boolean;
  /** 置信度统计 (搜索排序依据); 尝试次数过少不显示徽标 */
  stat?: SourceStat;
  /** 批量选择模式: 行首渲染复选框 */
  selectMode?: boolean;
  selected?: boolean;
  onToggleSelected?: (source: BookSource) => void;
  /** 点失效徽标查看失效原因 */
  onInvalidClick?: (source: BookSource) => void;
  onToggleEnabled: (source: BookSource, enabled: boolean) => void;
  /** 提供时在操作区渲染「工作台」入口(跳转可视化调试); 不传则不渲染 */
  onWorkbench?: (source: BookSource) => void;
  /** 书源配置了 loginUrl 时渲染「登录」入口(登录态按用户存库) */
  onLogin?: (source: BookSource) => void;
  onEdit: (source: BookSource) => void;
  onDebug: (source: BookSource) => void;
  onDelete: (source: BookSource) => void;
}

/** 类型徽标文案: BookSource.bookSourceType 0=文本 1=音频 */
const TYPE_LABEL: Record<number, string> = { 0: "文本", 1: "音频" };

/**
 * 书源列表行(父级 ul 提供 divide-y 分隔): 图标 + 名称/类型/失效徽标 + 地址·分组徽标,
 * 右侧调试/编辑/删除与启用开关.
 */
export function SourceListItem({
  source,
  invalid,
  dead,
  busy,
  stat,
  selectMode = false,
  selected = false,
  onToggleSelected,
  onInvalidClick,
  onToggleEnabled,
  onWorkbench,
  onLogin,
  onEdit,
  onDebug,
  onDelete,
}: SourceListItemProps) {
  const name = source.bookSourceName.length > 0 ? source.bookSourceName : source.bookSourceUrl;
  const group = source.bookSourceGroup.trim();

  return (
    <li className="flex items-center gap-3 px-4 py-3 transition-colors duration-150 ease-out hover:bg-surface-muted/60">
      {selectMode ? (
        <input
          type="checkbox"
          className="size-4 shrink-0 accent-[var(--accent)]"
          checked={selected}
          aria-label={`选择 ${name}`}
          onChange={() => onToggleSelected?.(source)}
        />
      ) : null}
      <span
        aria-hidden
        className={cn(
          "hidden size-10 shrink-0 items-center justify-center rounded-lg sm:flex",
          source.enabled ? "bg-accent/10 text-accent" : "bg-surface-muted text-muted-foreground",
        )}
      >
        <BookText className="size-5" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span
            className={cn("truncate text-sm font-medium", !source.enabled && "text-muted-foreground")}
            title={name}
          >
            {name}
          </span>
          {/* 类型是中性元信息: secondary 实底 + 前景色, 不靠颜色区分文本/音频 */}
          <Badge size="sm" variant="muted" className="shrink-0 bg-secondary text-foreground">
            {TYPE_LABEL[source.bookSourceType] ?? `类型${source.bookSourceType}`}
          </Badge>
          {invalid || dead ? (
            invalid && onInvalidClick !== undefined ? (
              <button
                type="button"
                className="shrink-0 cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                title={invalid ? "查看失效原因" : "长期统计失效: 尝试≥5 且成功率<20%"}
                onClick={() => onInvalidClick(source)}
              >
                <Badge size="sm" variant="danger">
                  失效
                </Badge>
              </button>
            ) : (
              <Badge size="sm" variant="danger" className="shrink-0">
                失效
              </Badge>
            )
          ) : null}
          {stat !== undefined && stat.attempts >= 3 ? (
            <Badge
              size="sm"
              variant={stat.confidence >= 0.7 ? "accent" : stat.confidence >= 0.4 ? "muted" : "danger"}
              className="shrink-0"
              title={`置信度 ${(stat.confidence * 100).toFixed(0)}% · 搜索 ${stat.attempts} 次 · 成功率 ${(stat.successRate * 100).toFixed(0)}% · 平均 ${stat.avgLatencyMs}ms`}
            >
              {`${stat.confidence >= 0.7 ? "优" : stat.confidence >= 0.4 ? "中" : "差"} ${(stat.confidence * 100).toFixed(0)}`}
            </Badge>
          ) : null}
        </div>
        {/* 地址与分组徽标并入同一 meta 行: 行高恒定两行, 有无分组不再跳动 */}
        <p className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span className="min-w-0 truncate" title={source.bookSourceUrl}>
            {source.bookSourceUrl}
          </span>
          {group.length > 0 ? (
            <Badge size="sm" variant="outline" className="max-w-32 shrink-0 truncate" title={group}>
              {group}
            </Badge>
          ) : null}
        </p>
      </div>

      <div className="flex shrink-0 items-center gap-0.5">
        {onLogin !== undefined && (source.loginUrl ?? "").trim().length > 0 ? (
          <IconButton size="sm" variant="ghost" tooltip="登录" onClick={() => onLogin(source)}>
            <KeyRound aria-hidden />
          </IconButton>
        ) : null}
        {onWorkbench !== undefined ? (
          <IconButton size="sm" variant="ghost" tooltip="工作台" onClick={() => onWorkbench(source)}>
            <ToolCase aria-hidden />
          </IconButton>
        ) : null}
        <IconButton size="sm" variant="ghost" tooltip="调试" onClick={() => onDebug(source)}>
          <SquareTerminal aria-hidden />
        </IconButton>
        <IconButton size="sm" variant="ghost" tooltip="编辑 JSON" onClick={() => onEdit(source)}>
          <Pencil aria-hidden />
        </IconButton>
        <IconButton
          size="sm"
          variant="ghost"
          tooltip="删除"
          className="text-muted-foreground hover:bg-danger/10 hover:text-danger"
          onClick={() => onDelete(source)}
        >
          <Trash2 aria-hidden />
        </IconButton>
        <Switch
          size="sm"
          className="ml-1"
          checked={source.enabled}
          disabled={busy}
          aria-label={`${source.enabled ? "禁用" : "启用"} ${name}`}
          onCheckedChange={(enabled) => onToggleEnabled(source, enabled)}
        />
      </div>
    </li>
  );
}
