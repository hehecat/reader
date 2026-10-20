import { useQuery } from "@tanstack/react-query";
import { Coffee, Leaf, Monitor, Moon, Settings2, Sun, X } from "lucide-react";
import * as React from "react";

import {
  FONT_FAMILY_OPTIONS,
  Field,
  Segmented,
  StepperRow,
} from "@/components/reader/SettingsControls";
import { EngineConfigDialog } from "@/components/settings/EngineConfigDialog";
import {
  Badge,
  Button,
  IconButton,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  cn,
  toast,
} from "@/components/ui";
import { errorMessage } from "@/hooks/useBookshelf";
import {
  getHttpTTSList,
  testGatewayEngine,
  testHttpTts,
  useTtsGatewayStatus,
  type GatewayEngine,
  type GatewayEngineKind,
  type HttpTts,
} from "@/services/httptts";
import { useReaderUIStore } from "@/stores/reader-ui-store";
import {
  AUTO_SCROLL_SPEED_RANGE,
  EDGE_TTS_LOCAL_TEMPLATE,
  FONT_SIZE_RANGE,
  defaultSettings,
  useSettingsStore,
  type FontFamilyMode,
  type ReadMode,
  type ThemeMode,
} from "@/stores/settings-store";

/** 主题五值: 图标在上文字在下 (日间/护眼/夜间/羊皮纸/跟随系统) */
const THEME_OPTIONS = [
  { value: "light", label: "日间", icon: Sun },
  { value: "green", label: "护眼", icon: Leaf },
  { value: "dark", label: "夜间", icon: Moon },
  { value: "sepia", label: "羊皮纸", icon: Coffee },
  { value: "system", label: "跟随系统", icon: Monitor },
] satisfies { value: ThemeMode; label: string; icon: typeof Sun }[];

/** 行距分段: 值取自 store 的 1.4-2.2 范围 */
const LINE_HEIGHT_OPTIONS = [
  { value: 1.6, label: "紧凑" },
  { value: 1.8, label: "适中" },
  { value: 2.2, label: "宽松" },
];

/** 段距分段 (em) */
const PARAGRAPH_GAP_OPTIONS = [
  { value: 0.5, label: "紧凑" },
  { value: 0.75, label: "适中" },
  { value: 1.25, label: "宽松" },
];

/** 页宽分段 (em, 主要影响桌面布局) */
const CONTENT_WIDTH_OPTIONS = [
  { value: 36, label: "窄" },
  { value: 42, label: "中" },
  { value: 48, label: "宽" },
];

const READ_MODE_OPTIONS = [
  { value: "scroll", label: "滚动" },
  { value: "page", label: "翻页" },
] satisfies { value: ReadMode; label: string }[];

/** 已存听书源的查询 key (面板打开时拉取, 5 分钟缓存) */
const HTTP_TTS_SOURCES_QUERY_KEY = ["httpTtsSources"] as const;

const EMPTY_SAVED_SOURCES: HttpTts[] = [];

/**
 * 自定义模板音源管理 (音源选「自定义模板」时由 TtsEngineControls 展开):
 * 模板输入 ({text}/{voice} 占位) + edge-tts 本地预设一键填充 + 短文本连通测试;
 * warp 端有已存听书源 (getHttpTTSList) 时提供下拉直接套用为模板.
 */
function HttpTtsSourceControls() {
  const ttsHttpUrl = useSettingsStore((state) => state.ttsHttpUrl);
  const setTtsHttpUrl = useSettingsStore((state) => state.setTtsHttpUrl);
  const ttsHttpVoice = useSettingsStore((state) => state.ttsHttpVoice);
  const [testing, setTesting] = React.useState(false);

  // 已存音源只是便捷入口: 未登录/后端不可用时静默隐藏, 不打扰模板手填
  const savedQuery = useQuery({
    queryKey: HTTP_TTS_SOURCES_QUERY_KEY,
    queryFn: ({ signal }) => getHttpTTSList({ signal }),
    staleTime: 5 * 60_000,
    retry: false,
  });
  const savedSources = savedQuery.data ?? EMPTY_SAVED_SOURCES;

  const handleTest = (): void => {
    setTesting(true);
    void testHttpTts(ttsHttpUrl, ttsHttpVoice)
      .then(
        () => toast.success("神经音源可用"),
        (error: unknown) => toast.error(errorMessage(error, "神经音源测试失败")),
      )
      .finally(() => setTesting(false));
  };

  return (
    <div>
      <Input
        size="sm"
        value={ttsHttpUrl}
        onChange={(event) => setTtsHttpUrl(event.target.value)}
        placeholder="模板 URL, 支持 {text}/{voice} 占位"
        aria-label="神经音源模板"
      />
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" onClick={() => setTtsHttpUrl(EDGE_TTS_LOCAL_TEMPLATE)}>
          edge-tts 本地 (9912)
        </Button>
        <Button size="sm" variant="secondary" loading={testing} onClick={handleTest}>
          测试
        </Button>
      </div>
      {savedSources.length > 0 ? (
        <Select
          value={savedSources.some((source) => source.url === ttsHttpUrl) ? ttsHttpUrl : ""}
          onValueChange={setTtsHttpUrl}
        >
          <SelectTrigger size="sm" aria-label="已存音源" className="mt-2">
            <SelectValue placeholder="套用已存音源…" />
          </SelectTrigger>
          <SelectContent>
            {savedSources.map((source) => (
              <SelectItem key={source.url} value={source.url}>
                <span className="truncate">{source.name}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      ) : null}
    </div>
  );
}

/** 引擎类别徽标文案: 部署形态一目了然 (自建进程/服务 · 免费云 · 需密钥的云) */
const ENGINE_KIND_LABEL: Record<GatewayEngineKind, string> = {
  "self-hosted": "自建",
  "cloud-free": "免费云",
  "cloud-keyed": "密钥云",
};

/** TTS 音源行: 状态圆点 + 名称徽标 + 说明小字, 整行可点选 (disabled 项不可点), 右侧可挂操作 */
function TtsSourceRow({
  selected,
  disabled = false,
  available = true,
  onSelect,
  title,
  children,
  hint,
  action,
}: {
  selected: boolean;
  disabled?: boolean;
  /** 状态圆点: 可用 = accent 实心, 不可用 = 灰 */
  available?: boolean;
  onSelect: () => void;
  title?: string;
  /** 主行: 名称 + 徽标 */
  children: React.ReactNode;
  /** 主行下方小字 (不可用原因 / setup 配置提示) */
  hint?: React.ReactNode;
  /** 右侧操作区 (测试按钮) */
  action?: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-2 rounded-lg border px-2 py-1.5 transition-colors",
        selected ? "border-accent/40 bg-accent/5" : "border-transparent hover:bg-surface-muted",
        disabled && "opacity-60",
      )}
    >
      <button
        type="button"
        role="radio"
        aria-checked={selected}
        disabled={disabled}
        onClick={onSelect}
        title={title}
        className="flex min-w-0 flex-1 cursor-pointer items-start gap-2 text-left disabled:cursor-not-allowed"
      >
        <span
          className={cn("mt-1.5 size-2 shrink-0 rounded-full", available ? "bg-accent" : "bg-muted-foreground/40")}
          aria-hidden
        />
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1">{children}</span>
          {hint ? <span className="mt-0.5 block text-xs text-muted-foreground">{hint}</span> : null}
        </span>
      </button>
      {action ? <div className="flex shrink-0 items-center gap-1">{action}</div> : null}
    </div>
  );
}

/**
 * TTS 引擎配置 (阅读器设置面板与设置页「阅读偏好」卡共用):
 * 网关地址输入 + /engines 能力清单 —— kind 徽标 (自建/免费云/密钥云)、可用圆点、
 * 不可用原因与 setup 配置提示; 行可点选切换音源 (auto 行旁注解析结果),
 * 可用引擎带短文本合成「测试」按钮; 带 config_fields 的引擎另有「配置」按钮 →
 * EngineConfigDialog (v3: 密钥/端点/模板直接在前端修改、保存并真实合成验证).
 * 音源为「自定义模板」时展开模板控件; 网关不可达静默降级为行内提示, 不弹 toast.
 */
export function TtsEngineControls() {
  const gatewayUrl = useSettingsStore((state) => state.ttsGatewayUrl);
  const setGatewayUrl = useSettingsStore((state) => state.setTtsGatewayUrl);
  const provider = useSettingsStore((state) => state.ttsProvider);
  const setProvider = useSettingsStore((state) => state.setTtsProvider);
  const statusQuery = useTtsGatewayStatus(gatewayUrl);
  const [testingId, setTestingId] = React.useState<string | null>(null);
  const [configEngineId, setConfigEngineId] = React.useState<string | null>(null);

  const status = statusQuery.data;
  // 配置弹窗的引擎按 id 从清单派生: 保存作废重拉后, config_fields 与掩码回显保持最新
  const configEngine = status?.engines.find((engine) => engine.id === configEngineId) ?? null;
  // auto 解析结果旁注: 引擎显示名 (清单未就绪时为空)
  const autoName =
    status === undefined
      ? ""
      : (status.engines.find((engine) => engine.id === status.auto)?.name ?? status.auto);

  const handleTest = (engine: GatewayEngine): void => {
    setTestingId(engine.id);
    void testGatewayEngine(gatewayUrl, engine.id)
      .then(
        () => toast.success(`${engine.name} 可用`),
        (error: unknown) => toast.error(errorMessage(error, "引擎测试失败")),
      )
      .finally(() => setTestingId(null));
  };

  return (
    <div>
      <Input
        size="sm"
        value={gatewayUrl}
        onChange={(event) => setGatewayUrl(event.target.value)}
        placeholder="留空 = 同源代理 /tts-gateway; 或填绝对地址指向其他网关"
        aria-label="TTS 网关地址"
      />
      <div className="mt-2 space-y-0.5" role="radiogroup" aria-label="TTS 音源">
        <TtsSourceRow
          selected={provider === "auto"}
          onSelect={() => setProvider("auto")}
          title="网关按资源可用性自动选择: 自建实测够快 → 免费云 → 密钥云"
        >
          <span className="truncate text-xs font-medium">自动 (资源优先)</span>
          {autoName !== "" ? (
            <span className="truncate text-xs text-muted-foreground">当前解析: {autoName}</span>
          ) : null}
        </TtsSourceRow>

        {statusQuery.isPending ? (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            正在探测网关引擎… (首次探测需实测自建引擎实时率, 可能较久)
          </p>
        ) : null}
        {statusQuery.isError ? (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            网关不可达: 请检查上方地址与网关是否已启动 (启动后重新打开面板即重试)
          </p>
        ) : null}

        {(status?.engines ?? []).map((engine) => (
          <TtsSourceRow
            key={engine.id}
            selected={provider === engine.id}
            disabled={!engine.available}
            available={engine.available}
            onSelect={() => setProvider(engine.id)}
            title={engine.available ? engine.setup : undefined}
            hint={
              engine.available ? undefined : (
                <span className="line-clamp-2 break-all">
                  {engine.reason}
                  {engine.setup === "" ? "" : `: ${engine.setup}`}
                </span>
              )
            }
            action={
              <>
                {engine.available ? (
                  <Button
                    size="sm"
                    variant="secondary"
                    loading={testingId === engine.id}
                    onClick={() => handleTest(engine)}
                  >
                    测试
                  </Button>
                ) : null}
                {engine.config_fields.length > 0 ? (
                  <IconButton
                    size="sm"
                    variant="ghost"
                    tooltip="配置"
                    onClick={() => setConfigEngineId(engine.id)}
                  >
                    <Settings2 />
                  </IconButton>
                ) : null}
              </>
            }
          >
            <span className="truncate text-xs font-medium">{engine.name}</span>
            <Badge size="sm" variant="outline">
              {ENGINE_KIND_LABEL[engine.kind]}
            </Badge>

          </TtsSourceRow>
        ))}

        <TtsSourceRow
          selected={provider === "template"}
          onSelect={() => setProvider("template")}
          title="任意 HTTP 合成源: 模板 URL 支持 {text}/{voice} 占位"
        >
          <span className="truncate text-xs font-medium">自定义模板</span>
          <span className="truncate text-xs text-muted-foreground">任意 HTTP 源</span>
        </TtsSourceRow>
        <TtsSourceRow
          selected={provider === "system"}
          onSelect={() => setProvider("system")}
          title="浏览器内置 speechSynthesis, 可调语速"
        >
          <span className="truncate text-xs font-medium">系统音</span>
          <span className="truncate text-xs text-muted-foreground">浏览器内置</span>
        </TtsSourceRow>
      </div>
      {provider === "template" ? (
        <div className="mt-2 border-t border-border/70 pt-2">
          <HttpTtsSourceControls />
        </div>
      ) : null}
      <EngineConfigDialog
        engine={configEngine}
        gatewayUrl={gatewayUrl}
        open={configEngineId !== null}
        onOpenChange={(open) => {
          if (!open) {
            setConfigEngineId(null);
          }
        }}
      />
    </div>
  );
}

/**
 * 阅读设置浮动面板 (砚台原型形态): 移动端底部 sheet, 桌面右下角浮动卡片.
 * 全部写入 settings-store (persist 自动持久化), 改动即时生效.
 * Esc 关闭由 ReaderPage 统一走 reader-ui-store.closeAll.
 */
export function SettingsPanel() {
  const settingsOpen = useReaderUIStore((state) => state.settingsOpen);
  const setSettingsOpen = useReaderUIStore((state) => state.setSettingsOpen);

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
  const customFontFamily = useSettingsStore((state) => state.customFontFamily);
  const setCustomFontFamily = useSettingsStore((state) => state.setCustomFontFamily);
  const readMode = useSettingsStore((state) => state.readMode);
  const setReadMode = useSettingsStore((state) => state.setReadMode);
  const indentParagraph = useSettingsStore((state) => state.indentParagraph);
  const aiAssistant = useSettingsStore((state) => state.aiAssistant);
  const setAiAssistant = useSettingsStore((state) => state.setAiAssistant);
  const setIndentParagraph = useSettingsStore((state) => state.setIndentParagraph);
  const contentWidth = useSettingsStore((state) => state.contentWidth);
  const setContentWidth = useSettingsStore((state) => state.setContentWidth);
  const autoScrollSpeed = useSettingsStore((state) => state.autoScrollSpeed);
  const setAutoScrollSpeed = useSettingsStore((state) => state.setAutoScrollSpeed);

  const handleClose = React.useCallback(() => setSettingsOpen(false), [setSettingsOpen]);
  const handleReset = React.useCallback(() => {
    useSettingsStore.setState(defaultSettings);
  }, []);

  if (!settingsOpen) {
    return null;
  }

  return (
    <>
      <div
        className="ui-overlay fixed inset-0 z-40 bg-black/25"
        data-state="open"
        onClick={handleClose}
        aria-hidden
      />
      <div
        role="dialog"
        aria-label="阅读设置"
        data-state="open"
        className="ui-dialog fixed inset-x-0 bottom-0 z-50 max-h-[85vh] overflow-y-auto overscroll-contain rounded-t-2xl border-t border-border bg-surface p-5 shadow-2xl sm:inset-x-auto sm:right-6 sm:bottom-6 sm:w-80 sm:rounded-2xl sm:border"
      >
        <div className="mb-4 flex items-center justify-between">
          <h3 className="font-display text-base font-semibold">阅读设置</h3>
          <button
            onClick={handleClose}
            aria-label="关闭"
            className="cursor-pointer text-muted-foreground transition-colors hover:text-foreground"
          >
            <X className="size-4" />
          </button>
        </div>

        <Field label="主题">
          <Segmented<ThemeMode>
            ariaLabel="主题"
            value={theme}
            options={THEME_OPTIONS}
            onChange={setTheme}
          />
        </Field>

        <Field label="字号">
          <StepperRow
            value={fontSize}
            min={FONT_SIZE_RANGE.min}
            max={FONT_SIZE_RANGE.max}
            step={FONT_SIZE_RANGE.step}
            onChange={setFontSize}
            format={(value) => `${value}px`}
            decreaseLabel="减小字号"
            increaseLabel="增大字号"
          />
        </Field>

        <Field label="行距">
          <Segmented<number>
            ariaLabel="行距"
            value={lineHeight}
            options={LINE_HEIGHT_OPTIONS}
            onChange={setLineHeight}
          />
        </Field>

        <Field label="段距">
          <Segmented<number>
            ariaLabel="段距"
            value={paragraphGap}
            options={PARAGRAPH_GAP_OPTIONS}
            onChange={setParagraphGap}
          />
        </Field>

        <Field label="页宽">
          <Segmented<number>
            ariaLabel="页宽"
            value={contentWidth}
            options={CONTENT_WIDTH_OPTIONS}
            onChange={setContentWidth}
          />
        </Field>

        <Field label="字体">
          <Segmented<FontFamilyMode>
            ariaLabel="字体"
            columns="grid-cols-4"
            value={fontFamily}
            options={FONT_FAMILY_OPTIONS}
            onChange={setFontFamily}
          />
          {fontFamily === "custom" ? (
            <Input
              size="sm"
              className="mt-2"
              value={customFontFamily}
              onChange={(event) => setCustomFontFamily(event.target.value)}
              placeholder={'font-family 串, 如: "LXGW WenKai", cursive'}
              aria-label="自定义 font-family"
            />
          ) : null}
        </Field>

        <Field label="阅读模式">
          <Segmented<ReadMode>
            ariaLabel="阅读模式"
            columns="grid-cols-2"
            value={readMode}
            options={READ_MODE_OPTIONS}
            onChange={setReadMode}
          />
        </Field>

        <div className="mb-4 flex items-center justify-between gap-3">
          <span className="text-xs font-medium text-muted-foreground">首行缩进</span>
          <Switch
            size="sm"
            checked={indentParagraph}
            onCheckedChange={setIndentParagraph}
            aria-label="首行缩进"
          />
        </div>

        <div className="mb-4 flex items-center justify-between gap-3">
          <div className="min-w-0">
            <p className="text-xs font-medium text-muted-foreground">AI 阅读助手</p>
            <p className="text-xs text-muted-foreground/70">基于已缓存章节问答, 需部署端配置模型</p>
          </div>
          <Switch
            size="sm"
            checked={aiAssistant}
            onCheckedChange={setAiAssistant}
            aria-label="AI 阅读助手"
          />
        </div>

        <Field label="自动滚动速度">
          <StepperRow
            value={autoScrollSpeed}
            min={AUTO_SCROLL_SPEED_RANGE.min}
            max={AUTO_SCROLL_SPEED_RANGE.max}
            step={AUTO_SCROLL_SPEED_RANGE.step}
            onChange={setAutoScrollSpeed}
            format={(value) => `${value} px/s`}
            decreaseLabel="降低自动滚动速度"
            increaseLabel="提高自动滚动速度"
          />
        </Field>

        <Field label="TTS 引擎">
          <TtsEngineControls />
        </Field>

        <button
          type="button"
          onClick={handleReset}
          className="mt-1 w-full cursor-pointer text-center text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          恢复默认
        </button>
      </div>
    </>
  );
}
