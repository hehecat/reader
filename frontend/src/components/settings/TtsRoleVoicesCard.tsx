import { Play, RotateCcw, Trash2 } from "lucide-react";
import * as React from "react";

import {
  Button,
  IconButton,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  SettingCard,
  SettingRow,
  toast,
} from "@/components/ui";
import { DEFAULT_TYPE_VOICES, ROLE_TYPES, ROLE_TYPE_LABEL } from "@/lib/ttsRoles";
import {
  EMPTY_GATEWAY_VOICES,
  fetchGatewayTtsAudioUrl,
  fetchTtsAudioUrl,
  filterChineseVoices,
  useTtsGatewayStatus,
  useTtsGatewayVoices,
} from "@/services/httptts";
import { useSettingsStore } from "@/stores/settings-store";

/** 「引擎默认」哨兵: 选中即清空该类型音色, 朗读时回落到全局音色 */
const ENGINE_DEFAULT = "__default__";

/**
 * 角色音色面板: 先把「角色类型 → 音色」配好(一次配完, 不必等识别出角色),
 * 朗读时识别到的角色自动归入某个类型并沿用该类型的音色; 也能在这里改类型或删角色。
 */
export function TtsRoleVoicesCard() {
  const multiRole = useSettingsStore((state) => state.ttsMultiRole);
  const setMultiRole = useSettingsStore((state) => state.setTtsMultiRole);
  const roleTypes = useSettingsStore((state) => state.ttsRoleTypes);
  const setRoleTypes = useSettingsStore((state) => state.setTtsRoleTypes);
  const typeVoices = useSettingsStore((state) => state.ttsTypeVoices);
  const setTypeVoices = useSettingsStore((state) => state.setTtsTypeVoices);
  const provider = useSettingsStore((state) => state.ttsProvider);
  const gatewayUrl = useSettingsStore((state) => state.ttsGatewayUrl);
  const httpUrl = useSettingsStore((state) => state.ttsHttpUrl);
  const httpVoice = useSettingsStore((state) => state.ttsHttpVoice);
  const [previewing, setPreviewing] = React.useState<string | null>(null);

  // 与阅读器同源: 引擎解析与音色清单复用同一组查询, 避免两处口径漂移
  const status = useTtsGatewayStatus(gatewayUrl).data ?? null;
  const engine =
    provider === "system" || provider === "template"
      ? ""
      : provider === "auto"
        ? (status?.auto ?? "")
        : provider;
  const voicesAll = useTtsGatewayVoices(gatewayUrl, engine).data ?? EMPTY_GATEWAY_VOICES;
  const voices = filterChineseVoices(engine, voicesAll);
  const isTemplate = provider === "template";
  const canPreview = !(provider === "system" || (provider === "auto" && engine === ""));
  const characters = Object.keys(roleTypes).sort((a, b) => a.localeCompare(b, "zh-Hans"));

  const preview = (voice: string) => {
    const text = "这是一句试听, 确认一下这个角色的音色。";
    const resolved = voice === "" ? httpVoice : voice;
    const done = (): void => setPreviewing(null);
    setPreviewing(resolved);
    const attach = (url: string): void => {
      const audio = new Audio(url);
      audio.onended = () => {
        URL.revokeObjectURL(url);
        done();
      };
      audio.onerror = () => {
        URL.revokeObjectURL(url);
        done();
      };
      void audio.play().catch(done);
    };
    const fail = (error: unknown): void => {
      toast.error(error instanceof Error ? error.message : "试听失败");
      done();
    };
    if (isTemplate) {
      fetchTtsAudioUrl(httpUrl, text, resolved).then(attach).catch(fail);
      return;
    }
    fetchGatewayTtsAudioUrl(gatewayUrl, text, resolved, engine).then(attach).catch(fail);
  };

  const voicePicker = (value: string, onPick: (voice: string) => void, label: string) => (
    <Select value={value === "" ? ENGINE_DEFAULT : value} onValueChange={(next) => onPick(next === ENGINE_DEFAULT ? "" : next)}>
      <SelectTrigger size="sm" aria-label={label} className="w-44 min-w-0">
        <span className="min-w-0 flex-1 truncate text-left" title={value === "" ? undefined : value}>
          <SelectValue placeholder="音色" />
        </span>
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ENGINE_DEFAULT}>跟随全局音色</SelectItem>
        {voices
          .filter((entry) => entry.id !== "")
          .map((entry) => (
            // 音色 id 很长(z-h-CN-liaoning-XiaobeiNeural · Female): 单行截断 + 悬浮看全名
            <SelectItem key={entry.id} value={entry.id} title={entry.id}>
              <span className="block max-w-56 truncate">{entry.name || entry.id}</span>
            </SelectItem>
          ))}
        {value !== "" && !voices.some((entry) => entry.id === value) ? (
          <SelectItem value={value} title={value}>
            <span className="block max-w-56 truncate">{value}</span>
          </SelectItem>
        ) : null}
      </SelectContent>
    </Select>
  );

  return (
    <SettingCard
      title="角色音色"
      desc={`先给角色类型配好音色(一次配完); 朗读时识别出的角色会自动归入某类型并使用它的音色。当前引擎: ${engine || "系统音"} · 共 ${voices.length} 个中文音色`}
    >
      <SettingRow
        label="角色朗读"
        value={
          multiRole
            ? canPreview
              ? "已开启: 对白按角色类型切换音色"
              : "已开启: 当前音源(系统音)无法指定音色, 仅在网关/模板音源生效"
            : "关闭时全部用上面选定的音色朗读"
        }
      >
        <Button
          size="sm"
          variant={multiRole ? "secondary" : "primary"}
          onClick={() => setMultiRole(!multiRole)}
        >
          {multiRole ? "关闭" : "开启"}
        </Button>
      </SettingRow>

      {/* ① 类型 → 音色: 固定这些行, 不必等识别出角色 */}
      {ROLE_TYPES.map((type) => {
        // 三态: undefined = 未配置(展示内置默认) / "" = 显式跟随全局 / 具体 id = 用户选的音色
        const configured = typeVoices[type.id];
        const voice = configured === undefined ? (DEFAULT_TYPE_VOICES[type.id] ?? "") : configured;
        return (
          <SettingRow
            key={type.id}
            label={type.label}
            value={voice === "" ? "跟随全局音色" : voice}
          >
            <div className="flex min-w-0 flex-none items-center gap-1">
              {voicePicker(
                voice,
                (next) => setTypeVoices({ ...typeVoices, [type.id]: next }),
                `${type.label} 的音色`,
              )}
              <IconButton
                size="sm"
                variant="ghost"
                aria-label={`试听 ${type.label}`}
                tooltip={canPreview ? "试听" : "系统音源无法试听"}
                disabled={!canPreview || previewing !== null}
                loading={previewing === (voice === "" ? httpVoice : voice)}
                onClick={() => preview(voice)}
              >
                <Play aria-hidden />
              </IconButton>
            </div>
          </SettingRow>
        );
      })}

      {/* ② 已识别角色 → 类型(朗读时自动写入, 这里可改类型或删掉) */}
      {characters.length === 0 ? (
        <p className="py-4 text-xs text-muted-foreground">
          还没有识别到角色。朗读一段对白后, 识别出的角色会出现在这里(类型可改)。
        </p>
      ) : (
        characters.map((name) => (
          <SettingRow
            key={name}
            label={name}
            value={`当前: ${ROLE_TYPE_LABEL[roleTypes[name] ?? ""] ?? "未归类"}`}
          >
            <div className="flex min-w-0 flex-none items-center gap-1">
              <Select
                value={roleTypes[name] ?? ROLE_TYPES[0].id}
                onValueChange={(next) => setRoleTypes({ ...roleTypes, [name]: next })}
              >
                <SelectTrigger size="sm" aria-label={`${name} 的类型`} className="w-32 min-w-0">
                  <SelectValue placeholder="类型" />
                </SelectTrigger>
                <SelectContent>
                  {ROLE_TYPES.map((type) => (
                    <SelectItem key={type.id} value={type.id}>
                      {type.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <IconButton
                size="sm"
                variant="ghost"
                aria-label={`删除 ${name}`}
                tooltip="删除该角色(朗读时回退旁白)"
                onClick={() => {
                  const next = { ...roleTypes };
                  delete next[name];
                  setRoleTypes(next);
                }}
              >
                <Trash2 aria-hidden />
              </IconButton>
            </div>
          </SettingRow>
        ))
      )}

      {characters.length > 0 ? (
        <SettingRow label="清理" value="清空已识别角色, 下次朗读重新归类(类型音色保留)">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setRoleTypes({});
              toast.success("已清空角色列表");
            }}
          >
            <RotateCcw aria-hidden />
            清空角色
          </Button>
        </SettingRow>
      ) : null}
    </SettingCard>
  );
}
