import { ChevronDown, Search as SearchIcon, Square, Trash2 } from "lucide-react";
import { useEffect, useId, useState } from "react";

import { Button, IconButton, Input, cn } from "@/components/ui";

/** 桌面断点(Tailwind md = 48rem)以上才自动聚焦: 移动端不抢焦点、不弹软键盘 */
const DESKTOP_MEDIA = "(min-width: 48rem)";

export interface SearchBarProps {
  value: string;
  onChange: (value: string) => void;
  /** Enter 或点击「搜索」触发; 搜索中再次触发表示换词重搜(由调用方断开旧连接) */
  onSearch: (key: string) => void;
  /** 搜索中按钮变「停止」 */
  onStop: () => void;
  searching: boolean;
  /** 搜索历史; 有历史时搜索栏下常驻一行「历史」折叠条(初始态与结果态都在) */
  history?: readonly string[];
  /** 历史 chips 默认是否展开: 初始态展开, 有结果后收起(仍可用「历史」按钮展开) */
  historyDefaultOpen?: boolean;
  /** 点击历史条目: 回填并搜索 */
  onSelectHistory?: (key: string) => void;
  onClearHistory?: () => void;
}

/** 搜索栏: 大输入框 + 搜索/停止按钮; 下方一行可折叠的搜索历史 chips. */
export function SearchBar({
  value,
  onChange,
  onSearch,
  onStop,
  searching,
  history = [],
  historyDefaultOpen = false,
  onSelectHistory,
  onClearHistory,
}: SearchBarProps) {
  const historyId = useId();
  const [historyOpen, setHistoryOpen] = useState(historyDefaultOpen);
  // 进入/离开初始态时回到该态的默认展开度; 同一态内用户手动展开的选择保持
  useEffect(() => {
    setHistoryOpen(historyDefaultOpen);
  }, [historyDefaultOpen]);
  // 挂载时判定一次: 桌面自动聚焦, 移动端不弹软键盘
  const [autoFocus] = useState(() => window.matchMedia(DESKTOP_MEDIA).matches);
  // 停止→搜索按钮切换防抖: 搜索结束 800ms 内停止按钮留位(禁用),
  // 吸收「搜索恰好在点击瞬间结束→按钮换成提交钮→误触重搜」
  const [stopGrace, setStopGrace] = useState(false);
  useEffect(() => {
    if (searching) {
      setStopGrace(true);
      return;
    }
    const timer = window.setTimeout(() => setStopGrace(false), 800);
    return () => window.clearTimeout(timer);
  }, [searching]);

  return (
    <div className="flex flex-col gap-3">
      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          onSearch(value);
        }}
      >
        <Input
          value={value}
          onChange={(event) => onChange(event.target.value)}
          clearable
          onClear={() => onChange("")}
          placeholder="搜索书名或作者"
          aria-label="搜索关键词"
          prefixIcon={<SearchIcon className="size-4" />}
          className="h-12 flex-1 rounded-xl"
          autoComplete="off"
          spellCheck={false}
          autoFocus={autoFocus}
        />
        {searching || stopGrace ? (
          <Button
            type="button"
            variant="secondary"
            onClick={onStop}
            disabled={!searching}
            className="h-12 rounded-xl px-5"
          >
            <Square className="fill-current" />
            停止
          </Button>
        ) : (
          <Button type="submit" disabled={value.trim().length === 0} className="h-12 rounded-xl px-5">
            <SearchIcon />
            搜索
          </Button>
        )}
      </form>

      {history.length > 0 ? (
        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between gap-2">
            <button
              type="button"
              onClick={() => setHistoryOpen((prev) => !prev)}
              aria-expanded={historyOpen}
              aria-controls={historyId}
              className="-ml-1 inline-flex cursor-pointer items-center gap-1 rounded-md px-1 py-0.5 text-xs font-medium text-muted-foreground transition duration-150 ease-out hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              历史
              <ChevronDown
                aria-hidden
                className={cn(
                  "size-3.5 transition-transform duration-150 ease-out",
                  historyOpen && "rotate-180",
                )}
              />
            </button>
            {onClearHistory ? (
              <IconButton size="sm" variant="ghost" tooltip="清除搜索历史" onClick={onClearHistory}>
                <Trash2 />
              </IconButton>
            ) : null}
          </div>
          {historyOpen ? (
            <div id={historyId} className="flex flex-wrap gap-2">
              {history.map((item) => (
                <button
                  key={item}
                  type="button"
                  onClick={() => {
                    onChange(item);
                    onSelectHistory?.(item);
                  }}
                  className="cursor-pointer rounded-full border border-border bg-surface px-3 py-1.5 text-xs text-foreground transition duration-150 ease-out hover:bg-surface-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none active:scale-97"
                >
                  {item}
                </button>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
