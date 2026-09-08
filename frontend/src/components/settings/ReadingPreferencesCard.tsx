import { Check, Trash2, Upload } from "lucide-react";
import * as React from "react";

import {
  FONT_FAMILY_OPTIONS,
  Segmented,
  StepperRow,
} from "@/components/reader/SettingsControls";
import { Button, Input, SettingCard, SettingRow, Slider, Switch, cn, toast } from "@/components/ui";
import { useCustomFonts } from "@/hooks/useCustomFonts";
import { deleteFont, uploadFont, type CustomFont } from "@/services/fonts";
import {
  AUTO_SCROLL_SPEED_RANGE,
  FONT_SIZE_RANGE,
  LINE_HEIGHT_RANGE,
  useSettingsStore,
  type FontFamilyMode,
  type ThemeMode,
} from "@/stores/settings-store";

/** 段距范围 (em), 与阅读器设置面板同一口径 */
const PARAGRAPH_GAP_RANGE = { min: 0, max: 2, step: 0.25 } as const;
/** 页面宽度范围 (em, 仅桌面布局生效), 与阅读器设置面板同一口径 */
const CONTENT_WIDTH_RANGE = { min: 30, max: 60, step: 1 } as const;

interface ThemeOption {
  value: ThemeMode;
  label: string;
  background: string;
  /** 选中勾的颜色 (深底色用白勾) */
  checkColor: string;
}

/** 主题色板: 与阅读器抽屉同款色值, 含 sepia/system 全五值 */
const THEME_OPTIONS: ThemeOption[] = [
  { value: "light", label: "浅色", background: "#ffffff", checkColor: "#3f3f46" },
  { value: "dark", label: "深色", background: "#17171b", checkColor: "#f4f4f5" },
  { value: "sepia", label: "羊皮纸", background: "#f6eeda", checkColor: "#433422" },
  { value: "green", label: "护眼绿", background: "#c7edcc", checkColor: "#22382a" },
  {
    value: "system",
    label: "跟随系统",
    background: "linear-gradient(135deg, #ffffff 50%, #17171b 50%)",
    checkColor: "#2563eb",
  },
];

/** 字体行说明: 与阅读器浮动面板同一组四选 (宋体/黑体/楷体/自定义) */
const FONT_HINT: Record<FontFamilyMode, string> = {
  serif: "衬线 · 宋体版式, 适合长篇",
  sans: "无衬线 · 黑体版式, 屏幕清晰",
  kai: "楷体 · 手写风味, 版式松弛",
  custom: "自定义 · 使用填写的 font-family 串",
};

/**
 * 阅读偏好卡: 主题/字号/行距/段距/页宽/字体(含楷体与自定义)/阅读模式/缩进/自动滚动速度/预热,
 * 与阅读器内的设置浮动面板同源 (settings-store), 修改即时生效并持久化在本机.
 */
export function ReadingPreferencesCard() {
  const theme = useSettingsStore((state) => state.theme);
  const setTheme = useSettingsStore((state) => state.setTheme);
  const fontSize = useSettingsStore((state) => state.fontSize);
  const setFontSize = useSettingsStore((state) => state.setFontSize);
  const lineHeight = useSettingsStore((state) => state.lineHeight);
  const setLineHeight = useSettingsStore((state) => state.setLineHeight);
  const paragraphGap = useSettingsStore((state) => state.paragraphGap);
  const setParagraphGap = useSettingsStore((state) => state.setParagraphGap);
  const fontFamily = useSettingsStore((state) => state.fontFamily);
  const setFontFamily = useSettingsStore((state) => state.setFontFamily);
  const readMode = useSettingsStore((state) => state.readMode);
  const setReadMode = useSettingsStore((state) => state.setReadMode);
  const indentParagraph = useSettingsStore((state) => state.indentParagraph);
  const setIndentParagraph = useSettingsStore((state) => state.setIndentParagraph);
  const contentWidth = useSettingsStore((state) => state.contentWidth);
  const setContentWidth = useSettingsStore((state) => state.setContentWidth);
  const customFontFamily = useSettingsStore((state) => state.customFontFamily);
  const setCustomFontFamily = useSettingsStore((state) => state.setCustomFontFamily);
  const autoScrollSpeed = useSettingsStore((state) => state.autoScrollSpeed);
  const setAutoScrollSpeed = useSettingsStore((state) => state.setAutoScrollSpeed);
  const preheatOnChapterEnd = useSettingsStore((state) => state.preheatOnChapterEnd);
  const setPreheatOnChapterEnd = useSettingsStore((state) => state.setPreheatOnChapterEnd);
  const preheatOnAdd = useSettingsStore((state) => state.preheatOnAdd);
  const setPreheatOnAdd = useSettingsStore((state) => state.setPreheatOnAdd);
  const { fonts, refresh } = useCustomFonts();
  const [uploading, setUploading] = React.useState(false);
  const fileRef = React.useRef<HTMLInputElement>(null);

  const handleUpload = async (file: File) => {
    setUploading(true);
    try {
      const saved = await uploadFont(file);
      toast.success(`字体「${saved.name}」已上传`);
      refresh();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "上传失败");
    } finally {
      setUploading(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const applyFont = (f: CustomFont) => {
    setCustomFontFamily(`"${f.family}"`);
    setFontFamily("custom");
  };

  return (
    <SettingCard title="阅读偏好" desc="与阅读器内的设置面板同源, 修改即时生效并保存在本机">
      <SettingRow label="主题" value="应用于整个界面与阅读页">
        <div role="radiogroup" aria-label="主题" className="flex items-center gap-2">
          {THEME_OPTIONS.map((option) => {
            const selected = theme === option.value;
            return (
              <button
                key={option.value}
                type="button"
                role="radio"
                aria-checked={selected}
                aria-label={option.label}
                title={option.label}
                onClick={() => setTheme(option.value)}
                className={cn(
                  "relative flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full border border-border transition",
                  selected && "ring-2 ring-accent ring-offset-2 ring-offset-surface",
                )}
                style={{ background: option.background }}
              >
                {selected ? (
                  <Check className="size-3.5" style={{ color: option.checkColor }} />
                ) : null}
              </button>
            );
          })}
        </div>
      </SettingRow>

      <div className="py-4">
        <Slider
          label="字号"
          min={FONT_SIZE_RANGE.min}
          max={FONT_SIZE_RANGE.max}
          step={FONT_SIZE_RANGE.step}
          value={[fontSize]}
          formatValue={(value) => `${value}px`}
          onValueChange={(values) => {
            const next = values[0];
            if (next !== undefined) {
              setFontSize(next);
            }
          }}
        />
      </div>
      <div className="py-4">
        <Slider
          label="行距"
          min={LINE_HEIGHT_RANGE.min}
          max={LINE_HEIGHT_RANGE.max}
          step={LINE_HEIGHT_RANGE.step}
          value={[lineHeight]}
          formatValue={(value) => value.toFixed(2)}
          onValueChange={(values) => {
            const next = values[0];
            if (next !== undefined) {
              setLineHeight(next);
            }
          }}
        />
      </div>
      <div className="py-4">
        <Slider
          label="段距"
          min={PARAGRAPH_GAP_RANGE.min}
          max={PARAGRAPH_GAP_RANGE.max}
          step={PARAGRAPH_GAP_RANGE.step}
          value={[paragraphGap]}
          formatValue={(value) => `${value}em`}
          onValueChange={(values) => {
            const next = values[0];
            if (next !== undefined) {
              setParagraphGap(next);
            }
          }}
        />
      </div>
      {/* 页面宽度只对桌面布局有意义 */}
      <div className="hidden py-4 md:block">
        <Slider
          label="页面宽度"
          min={CONTENT_WIDTH_RANGE.min}
          max={CONTENT_WIDTH_RANGE.max}
          step={CONTENT_WIDTH_RANGE.step}
          value={[contentWidth]}
          formatValue={(value) => `${value}em`}
          onValueChange={(values) => {
            const next = values[0];
            if (next !== undefined) {
              setContentWidth(next);
            }
          }}
        />
      </div>

      <SettingRow label="字体" value={FONT_HINT[fontFamily]}>
        <Segmented<FontFamilyMode>
          ariaLabel="字体"
          columns="grid-cols-4"
          className="max-w-60"
          value={fontFamily}
          options={FONT_FAMILY_OPTIONS}
          onChange={setFontFamily}
        />
      </SettingRow>
      {fontFamily === "custom" ? (
        <div className="py-4">
          <Input
            size="sm"
            value={customFontFamily}
            onChange={(event) => setCustomFontFamily(event.target.value)}
            placeholder={'自定义 font-family 串, 如: "LXGW WenKai", cursive'}
            aria-label="自定义 font-family"
          />
        </div>
      ) : null}
      <div className="space-y-2 py-4">
        <div className="flex items-center justify-between gap-2">
          <span className="text-muted-foreground text-xs">
            上传字体（TTF/OTF/WOFF/WOFF2, ≤40MB）· 按账号存服务端, 选用后作用于正文
          </span>
          <Button
            size="sm"
            variant="secondary"
            disabled={uploading}
            onClick={() => fileRef.current?.click()}
          >
            <Upload aria-hidden className="size-4" />
            {uploading ? "上传中" : "上传字体"}
          </Button>
          <input
            ref={fileRef}
            type="file"
            hidden
            accept=".ttf,.otf,.woff,.woff2"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void handleUpload(f);
            }}
          />
        </div>
        {fonts.map((f) => {
          const active = customFontFamily.includes(f.family);
          return (
            <div
              key={f.id}
              className="border-border/70 bg-surface-muted/40 flex items-center gap-2 rounded-md border px-3 py-2"
            >
              <span className="min-w-0 flex-1 truncate text-sm" title={f.name}>
                {f.name}
                <span className="text-muted-foreground ml-2 text-xs">{Math.round(f.size / 1024)} KB</span>
              </span>
              <Button size="sm" variant={active ? "secondary" : "ghost"} disabled={active} onClick={() => applyFont(f)}>
                {active ? <Check aria-hidden className="size-4" /> : null}
                {active ? "使用中" : "选用"}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="text-muted-foreground hover:bg-danger/10 hover:text-danger"
                onClick={() => {
                  void deleteFont(f.id)
                    .then(() => {
                      if (active) setCustomFontFamily("");
                      refresh();
                      toast.success("字体已删除");
                    })
                    .catch((e) => toast.error(e instanceof Error ? e.message : "删除失败"));
                }}
              >
                <Trash2 aria-hidden className="size-4" />
                删除
              </Button>
            </div>
          );
        })}
      </div>
      <SettingRow
        label="阅读模式"
        value={readMode === "page" ? "翻页 · 左右分页, 像纸书一样" : "滚动 · 上下连续阅读"}
      >
        <Switch
          checked={readMode === "page"}
          onCheckedChange={(page) => setReadMode(page ? "page" : "scroll")}
          aria-label="翻页模式"
        />
      </SettingRow>
      <SettingRow
        label="首行缩进"
        value={indentParagraph ? "正文段落首行缩进两字" : "不缩进, 段落以间距分隔"}
      >
        <Switch
          checked={indentParagraph}
          onCheckedChange={setIndentParagraph}
          aria-label="首行缩进"
        />
      </SettingRow>
      <SettingRow label="自动滚动速度" value="阅读器顶栏「自动滚动」的推进节奏">
        <StepperRow
          className="w-44"
          value={autoScrollSpeed}
          min={AUTO_SCROLL_SPEED_RANGE.min}
          max={AUTO_SCROLL_SPEED_RANGE.max}
          step={AUTO_SCROLL_SPEED_RANGE.step}
          onChange={setAutoScrollSpeed}
          format={(value) => `${value} px/s`}
          decreaseLabel="降低自动滚动速度"
          increaseLabel="提高自动滚动速度"
        />
      </SettingRow>
      <SettingRow
        label="章末自动预热"
        value="读到章末自动后台缓存后续章节, 源站失效后仍可读"
      >
        <Switch
          checked={preheatOnChapterEnd}
          onCheckedChange={setPreheatOnChapterEnd}
          aria-label="章末自动预热"
        />
      </SettingRow>
      <SettingRow label="加架自动预热" value="加入书架后后台缓存整书, 首次打开秒开">
        <Switch checked={preheatOnAdd} onCheckedChange={setPreheatOnAdd} aria-label="加架自动预热" />
      </SettingRow>
    </SettingCard>
  );
}
