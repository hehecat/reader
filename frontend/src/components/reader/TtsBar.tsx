import { ChevronDown, ChevronUp, Pause, Play, Square } from "lucide-react";
import * as React from "react";

import { StepperRow } from "@/components/reader/SettingsControls";
import {
  Button,
  IconButton,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  Tooltip,
} from "@/components/ui";
import type { UseTtsResult } from "@/hooks/useTts";
import { DEFAULT_GATEWAY_VOICE, HTTP_TTS_VOICE_PRESETS } from "@/services/httptts";
import { TTS_RATE_RANGE } from "@/stores/settings-store";

export interface TtsBarProps {
  /** useTts 的返回值: 朗读状态 + 全部控件绑定 */
  tts: UseTtsResult;
}

/** 音色下拉「自定义…」项的哨兵值 (Radix Select 不允许空串 value, 也不能与真实音色重名) */
const CUSTOM_VOICE_VALUE = "__custom__";

/** 音色下拉「引擎默认」项的哨兵值: 对应 ttsHttpVoice 空串 (合成时解析为该引擎默认音色) */
const ENGINE_DEFAULT_VOICE_VALUE = "__engine_default__";

/**
 * 朗读浮动条: 挂在阅读页底部, 只在朗读中/暂停时出现 (idle 直接不渲染).
 * 桌面居中收窄成一条, 移动端全宽贴底并让开安全区; 控件多时自动折行.
 * mb-14 让开常显的章节栏 (py-1.5 + size-9 图标按钮 ≈ 49px), 安全区那截由容器的 ui-safe-b 补.
 * 顺序: 上一段 / 播放暂停 / 停止 / 下一段 → 引擎 (自动/网关引擎/自定义模板/系统音, auto 旁注解析结果)
 * → 速度(仅系统音) → 音色 (网关音色清单 / 模板预设 / 系统语音) → 连读下一章.
 * memo: 阅读页随滚动逐帧重渲染, tts 引用稳定时这条栏不必跟着动.
 */
export const TtsBar = React.memo(function TtsBar({ tts }: TtsBarProps) {
  /** 「自定义…」音色: 选中哨兵项后内联展开输入, 回车/确定写入 settings */
  const [customVoiceOpen, setCustomVoiceOpen] = React.useState(false);
  const customVoiceRef = React.useRef<HTMLInputElement | null>(null);

  if (tts.status === "idle") {
    return null;
  }
  const playing = tts.status === "playing";
  const { provider, gatewayStatus, gatewayVoices, resolvedEngine, resolvedEngineName } = tts;
  const isSystem = provider === "system";
  const isTemplate = provider === "template";
  // 网关音源 (auto 或具体引擎 id): 音色清单来自 /voices?engine=<resolved>
  const isGateway = !isSystem && !isTemplate;
  const engines = gatewayStatus?.engines ?? [];

  // 当前音色是否已被可选项覆盖: 否则下拉里补一项, 触发器才能回显自定义音色名
  const voiceInPresets =
    tts.httpVoice === "" ||
    HTTP_TTS_VOICE_PRESETS.some((preset) => preset.value === tts.httpVoice);
  const voiceInGateway =
    tts.httpVoice === "" || gatewayVoices.some((voice) => voice.id === tts.httpVoice);
  // 「引擎默认」项旁回显的默认音色: 默认映射优先, 其次音色清单首个非空项
  const engineDefaultVoice =
    DEFAULT_GATEWAY_VOICE[resolvedEngine] ??
    gatewayVoices.find((voice) => voice.id !== "")?.id ??
    "";
  // 已选引擎不在能力清单里 (网关暂不可达/引擎下线): 补一项, 触发器才不至于空白
  const providerInEngines =
    !isGateway || engines.some((engine) => engine.id === provider);

  const handleProviderChange = (value: string): void => {
    if (value === provider) {
      return;
    }
    // 引擎间音色命名空间不同 (edge 神经名 / kokoro zm_* / tencent 数字 id): 切到网关音源时
    // 音色重置为空串 = 该引擎默认音色; 系统音不消费该值, 模板音色允许自由文本, 都保留原值
    if (value !== "system" && value !== "template") {
      tts.setHttpVoice("");
    }
    tts.setProvider(value);
  };

  const rateStepper = (
    <StepperRow
      className="w-28"
      value={tts.rate}
      min={TTS_RATE_RANGE.min}
      max={TTS_RATE_RANGE.max}
      step={TTS_RATE_RANGE.step}
      onChange={tts.setRate}
      format={(value) => `${value}x`}
      decreaseLabel="降低朗读速度"
      increaseLabel="提高朗读速度"
    />
  );

  /** 「自定义…」内联输入: 网关音色与模板音色两个分支共用 */
  const customVoiceForm = customVoiceOpen ? (
    <form
      className="flex flex-none items-center gap-1"
      onSubmit={(event) => {
        event.preventDefault();
        const value = customVoiceRef.current?.value.trim() ?? "";
        if (value.length > 0) {
          tts.setHttpVoice(value);
        }
        setCustomVoiceOpen(false);
      }}
    >
      <Input
        ref={customVoiceRef}
        size="sm"
        className="w-44"
        defaultValue={tts.httpVoice}
        placeholder="如 zh-CN-YunxiaNeural"
        aria-label="自定义音色名"
        autoFocus
      />
      <Button size="sm" variant="secondary" type="submit">
        确定
      </Button>
    </form>
  ) : null;

  return (
    <div className="ui-safe-b pointer-events-none absolute inset-x-0 bottom-0 z-40 flex justify-center px-2 sm:px-0">
      <div
        role="group"
        aria-label="朗读控制"
        className="pointer-events-auto mb-14 flex w-full flex-wrap items-center gap-x-2.5 gap-y-2 rounded-xl border border-border bg-surface/95 px-3 py-2 shadow-xl backdrop-blur sm:w-auto sm:max-w-xl"
      >
        <div className="flex items-center gap-0.5">
          <IconButton
            size="sm"
            variant="ghost"
            tooltip="上一段"
            aria-label="上一段"
            onClick={() => tts.step(-1)}
          >
            <ChevronUp />
          </IconButton>
          <IconButton
            size="sm"
            variant="primary"
            tooltip={playing ? "暂停" : "继续"}
            aria-label={playing ? "暂停朗读" : "继续朗读"}
            onClick={playing ? tts.pause : tts.resume}
          >
            {playing ? <Pause /> : <Play />}
          </IconButton>
          <IconButton
            size="sm"
            variant="ghost"
            tooltip="停止朗读"
            aria-label="停止朗读"
            onClick={tts.stop}
          >
            <Square className="fill-current" />
          </IconButton>
          <IconButton
            size="sm"
            variant="ghost"
            tooltip="下一段"
            aria-label="下一段"
            onClick={() => tts.step(1)}
          >
            <ChevronDown />
          </IconButton>
        </div>

        {/* 引擎: 自动 = 网关按资源自选 (自建够快 → 免费云 → 密钥云), 网关引擎按能力清单列出
            (不可用项禁用并以 title 说明原因), 自定义模板 / 系统音兜底 */}
        <Select value={provider} onValueChange={handleProviderChange}>
          <SelectTrigger size="sm" aria-label="朗读引擎" className="w-32 min-w-0 flex-none">
            <span className="min-w-0 flex-1 truncate text-left">
              <SelectValue />
            </span>
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="auto">自动 (资源优先)</SelectItem>
            {engines.map((engine) => (
              <SelectItem
                key={engine.id}
                value={engine.id}
                disabled={!engine.available}
                title={engine.available ? undefined : engine.reason || engine.setup}
              >
                <span className="truncate">{engine.name}</span>
              </SelectItem>
            ))}
            {providerInEngines ? null : (
              <SelectItem value={provider}>
                <span className="truncate">{provider}</span>
              </SelectItem>
            )}
            <SelectItem value="template">自定义模板</SelectItem>
            <SelectItem value="system">系统音</SelectItem>
          </SelectContent>
        </Select>
        {/* auto 的解析结果: 能力清单就绪后旁注实际选中的引擎名 */}
        {provider === "auto" && resolvedEngineName !== "" ? (
          <span
            className="-ml-1 max-w-28 flex-none truncate text-xs text-muted-foreground"
            title={`自动解析为 ${resolvedEngineName}`}
          >
            → {resolvedEngineName}
          </span>
        ) : null}

        {/* 速度: 仅系统音生效; 网关/模板语速由合成端决定, 禁用展示并悬浮说明 */}
        {isSystem ? (
          rateStepper
        ) : (
          <Tooltip content="语速由合成引擎决定, 切换到系统音可调速">
            <div className="flex-none">
              <div className="pointer-events-none opacity-40" inert>
                {rateStepper}
              </div>
            </div>
          </Tooltip>
        )}

        {/* 多角色: 仅网关/模板音源生效(系统音源由浏览器决定音色) */}
        <label className="flex flex-none cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
          <Switch
            checked={tts.multiRole}
            onCheckedChange={tts.setMultiRole}
            aria-label="角色朗读"
          />
          角色朗读
        </label>

        {isGateway ? (
          <div className="flex min-w-0 flex-none items-center gap-1.5">
            {/* 网关音色: 引擎默认 + /voices 清单 + 自定义… (回显清单外音色名) */}
            <Select
              value={tts.httpVoice === "" ? ENGINE_DEFAULT_VOICE_VALUE : tts.httpVoice}
              onValueChange={(value) => {
                if (value === CUSTOM_VOICE_VALUE) {
                  setCustomVoiceOpen(true);
                  return;
                }
                setCustomVoiceOpen(false);
                tts.setHttpVoice(value === ENGINE_DEFAULT_VOICE_VALUE ? "" : value);
              }}
            >
              <SelectTrigger size="sm" aria-label="音色" className="w-40 min-w-0 flex-none">
                <span className="min-w-0 flex-1 truncate text-left">
                  <SelectValue placeholder="音色" />
                </span>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ENGINE_DEFAULT_VOICE_VALUE}>
                  <span className="truncate">引擎默认</span>
                  {engineDefaultVoice !== "" ? (
                    <span className="ml-1.5 text-xs text-muted-foreground">{engineDefaultVoice}</span>
                  ) : null}
                </SelectItem>
                {gatewayVoices
                  .filter((voice) => voice.id !== "")
                  .map((voice) => (
                    <SelectItem key={voice.id} value={voice.id}>
                      <span className="truncate">{voice.name}</span>
                    </SelectItem>
                  ))}
                {voiceInGateway ? null : (
                  <SelectItem value={tts.httpVoice}>
                    <span className="truncate">{tts.httpVoice}</span>
                  </SelectItem>
                )}
                <SelectItem value={CUSTOM_VOICE_VALUE}>自定义…</SelectItem>
              </SelectContent>
            </Select>
            {customVoiceForm}
          </div>
        ) : isTemplate ? (
          <div className="flex min-w-0 flex-none items-center gap-1.5">
            {/* 模板音色: 预设四选一 + 自定义… (回显预设外音色名) */}
            <Select
              value={tts.httpVoice}
              onValueChange={(value) => {
                if (value === CUSTOM_VOICE_VALUE) {
                  setCustomVoiceOpen(true);
                  return;
                }
                setCustomVoiceOpen(false);
                tts.setHttpVoice(value);
              }}
            >
              <SelectTrigger size="sm" aria-label="模板音色" className="w-36 min-w-0 flex-none">
                <span className="min-w-0 flex-1 truncate text-left">
                  <SelectValue placeholder="音色" />
                </span>
              </SelectTrigger>
              <SelectContent>
                {HTTP_TTS_VOICE_PRESETS.map((preset) => (
                  <SelectItem key={preset.value} value={preset.value}>
                    <span className="truncate">{preset.label}</span>
                    <span className="ml-1.5 text-xs text-muted-foreground">{preset.value}</span>
                  </SelectItem>
                ))}
                {voiceInPresets ? null : (
                  <SelectItem value={tts.httpVoice}>
                    <span className="truncate">{tts.httpVoice}</span>
                  </SelectItem>
                )}
                <SelectItem value={CUSTOM_VOICE_VALUE}>自定义…</SelectItem>
              </SelectContent>
            </Select>
            {customVoiceForm}
          </div>
        ) : tts.voices.length > 0 ? (
          /* 系统语音: 一个语音都没装时整块隐藏 (只剩默认合成器) */
          <Select value={tts.voiceURI} onValueChange={tts.setVoiceURI}>
            <SelectTrigger size="sm" aria-label="朗读语音" className="w-36 min-w-0 flex-none">
              <span className="min-w-0 flex-1 truncate text-left">
                <SelectValue placeholder="默认语音" />
              </span>
            </SelectTrigger>
            <SelectContent>
              {tts.voices.map((voice) => (
                <SelectItem key={voice.voiceURI} value={voice.voiceURI}>
                  <span className="truncate">{voice.name}</span>
                  <span className="ml-1.5 text-xs text-muted-foreground">{voice.lang}</span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : null}

        <div className="ml-auto flex items-center gap-2">
          <span className="text-xs text-muted-foreground">连读下一章</span>
          <Switch
            size="sm"
            checked={tts.autoNext}
            onCheckedChange={tts.setAutoNext}
            aria-label="连读下一章"
          />
        </div>
      </div>
    </div>
  );
});
