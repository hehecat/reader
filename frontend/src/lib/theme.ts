import { Coffee, Leaf, Monitor, Moon, Sun, type LucideIcon } from "lucide-react";

import type { ThemeMode } from "@/stores/settings-store";

export interface ThemeOption {
  value: ThemeMode;
  label: string;
  icon: LucideIcon;
}

/**
 * 主题五档的唯一来源: 顶栏 / 阅读器底栏 / 阅读设置面板共用。
 * 三处各写一套口径时, 顶栏只露三档 → 选了羊皮纸或跟随系统的人看不到自己选了什么,
 * 底栏那个二值日夜切换还会把 sepia/system 直接改写成 light/dark。
 */
export const THEME_OPTIONS: ThemeOption[] = [
  { value: "light", label: "日间", icon: Sun },
  { value: "green", label: "护眼", icon: Leaf },
  { value: "dark", label: "夜间", icon: Moon },
  { value: "sepia", label: "羊皮纸", icon: Coffee },
  { value: "system", label: "跟随系统", icon: Monitor },
];

/** 当前主题对应的展示项(未收录的取值回落到日间) */
export function themeOption(theme: ThemeMode): ThemeOption {
  return THEME_OPTIONS.find((option) => option.value === theme) ?? THEME_OPTIONS[0]!;
}
