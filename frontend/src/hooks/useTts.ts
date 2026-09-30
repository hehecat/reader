import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { toast } from "@/components/ui";
import type { ReaderParagraph } from "@/hooks/useChapterContent";
import {
  DEFAULT_GATEWAY_VOICE,
  EMPTY_GATEWAY_VOICES,
  TTS_GATEWAY_STATUS_QUERY_KEY,
  fetchGatewayTtsAudioUrl,
  fetchTtsAudioUrl,
  filterChineseVoices,
  ttsGatewayVoicesQueryKey,
  useTtsGatewayStatus,
  useTtsGatewayVoices,
  type GatewayStatus,
  type GatewayVoice,
} from "@/services/httptts";
import { analyzeChapter, assignRoleTypes, mergeAdjacent, voiceForSegment } from "@/lib/ttsRoles";
import { useSettingsStore, type TtsProvider } from "@/stores/settings-store";

/** 朗读状态: idle = 没在朗读 (顶栏图标据此显示激活态, TtsBar 据此决定是否出现) */
export type TtsStatus = "idle" | "playing" | "paused";

/** 当前朗读段落的高亮类: 字面量写在这里, Tailwind 才会生成对应工具类 */
const HIGHLIGHT_CLASS = "bg-accent/10";

/** 打断式起播前留给 cancel 的时间 (ms): Chrome 里 cancel 之后立刻 speak 会被吞掉 */
const INTERRUPT_DELAY_MS = 80;

/** 没有匹配语音时兜底的语言标签 */
const FALLBACK_LANG = "zh-CN";

/** 一条待朗读片段: key = ReaderParagraph.key, 与正文 DOM 的 data-para-index 同源 */
interface SpeechItem {
  key: number;
  text: string;
  /** 多角色模式: 说话人(空串 = 旁白); 单音色模式不填 */
  speaker?: string;
  /** 多角色模式: 该片段使用的音色(角色绑定); 缺省回落全局音色 */
  voice?: string;
}

/** 神经音色 (网关/模板) 预取的下一段音频: 播到该段时命中即免等待; 作废时由 promise 结算后补 revoke */
interface PrefetchEntry {
  cursor: number;
  promise: Promise<string>;
}

export interface UseTtsOptions {
  /** 本章段落 (与正文同源: useChapterContent 的 items) */
  items: ReaderParagraph[];
  /** 正文容器: 定位起读段落 + 高亮当前朗读段落 */
  containerRef: React.RefObject<HTMLElement | null>;
  /** 当前章节索引: 换章后从章首续读 */
  chapterIndex: number;
  /** 章节总数: 判断还有没有下一章可连读 */
  chapterCount: number;
  /** 换书标识 (bookUrl): 变了就彻底停掉朗读 */
  resetKey: string;
  /** 连读到下一章 (useReaderProgress.stepChapter(1), 越界提示由它负责) */
  onNextChapter: () => void;
  /** 与自动滚动互斥: 起播时关掉自动滚动 */
  onStopAutoScroll: () => void;
}

export interface UseTtsResult {
  /** 浏览器是否提供 Web Speech API (系统音引擎与神经音色回退都依赖它) */
  supported: boolean;
  status: TtsStatus;
  /** 正在朗读的段落 key (null = 没有) */
  activeKey: number | null;
  /** TTS 音源: auto = 网关自动选择, system = 浏览器系统音, template = 自定义模板, 其余 = 网关引擎 id */
  provider: TtsProvider;
  setProvider: (provider: TtsProvider) => void;
  /** 网关能力清单: null = 网关不可达或还在探测 (引擎下拉退化为只有 自动/模板/系统音) */
  gatewayStatus: GatewayStatus | null;
  /** 解析后引擎的音色清单 (网关音源且清单就绪后才有值) */
  gatewayVoices: GatewayVoice[];
  /** 解析后的网关引擎 id: auto 取能力清单的 auto 值; system/template 音源为空串 */
  resolvedEngine: string;
  /** 解析后引擎的显示名 (auto 时在引擎下拉旁以小字展示) */
  resolvedEngineName: string;
  /** 可用系统语音 (zh-* 排前); 空数组 = 系统没有装语音 */
  voices: SpeechSynthesisVoice[];
  /** 速度倍率 0.6-2.0 (仅系统音生效; settings-store 持久化) */
  rate: number;
  setRate: (rate: number) => void;
  /** 选定系统语音的 voiceURI, 空串 = 默认语音 */
  voiceURI: string;
  setVoiceURI: (voiceURI: string) => void;
  /** 音色名 (网关引擎音色 id 或模板音色名), 空串 = 引擎默认音色 */
  httpVoice: string;
  setHttpVoice: (voice: string) => void;
  /** 读完本章自动接着读下一章 */
  autoNext: boolean;
  setAutoNext: (autoNext: boolean) => void;
  /** 多角色朗读开关(仅网关/模板音源生效) */
  multiRole: boolean;
  setMultiRole: (multiRole: boolean) => void;
  /** 从当前视口首段起读 */
  start: () => void;
  /** 顶栏图标: 没在朗读就起读, 正在朗读就停 */
  toggle: () => void;
  pause: () => void;
  resume: () => void;
  stop: () => void;
  /** 上一段/下一段 (delta = ±1); 越过末段走章末逻辑 */
  step: (delta: number) => void;
}

/** 段落纯文本: DOMParser 产出的文档是惰性的 (不加载图片/不跑脚本), textContent 顺手解掉实体 */
function plainText(html: string): string {
  const body = new DOMParser().parseFromString(html, "text/html").body;
  return (body.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** 可朗读队列: 只读正文段落 (章标题/图片/卷标记跳过), 顺序与渲染顺序一致 */
function buildQueue(items: ReaderParagraph[]): SpeechItem[] {
  const queue: SpeechItem[] = [];
  for (const item of items) {
    if (item.type !== "paragraph") {
      continue;
    }
    const text = plainText(item.text);
    if (text !== "") {
      queue.push({ key: item.key, text });
    }
  }
  return queue;
}

/**
 * 起读段落 = 可视区内「起头」的第一段 (data-para-index):
 * 滚动模式是视口首段, 翻页模式是当前页首段 —— 别的页被 translateX 平移出容器;
 * 跨列断段只剩尾巴落在当前页时, 并集盒仍探进可视区, 用左边缘判定把它排除.
 */
function firstVisibleParagraphKey(root: HTMLElement | null): number | null {
  if (root === null) {
    return null;
  }
  const view = root.getBoundingClientRect();
  if (view.width <= 0 || view.height <= 0) {
    return null;
  }
  for (const node of root.querySelectorAll<HTMLElement>("[data-para-index]")) {
    const rect = node.getBoundingClientRect();
    if (
      rect.left >= view.left - 1 &&
      rect.left < view.right - 1 &&
      rect.bottom > view.top + 1 &&
      rect.top < view.bottom - 1
    ) {
      const key = Number(node.dataset.paraIndex);
      return Number.isFinite(key) ? key : null;
    }
  }
  return null;
}

/** zh-* 语音排在前 (读的多是中文书), 同语区按名称 */
function sortVoices(list: SpeechSynthesisVoice[]): SpeechSynthesisVoice[] {
  return [...list].sort((a, b) => {
    const aZh = a.lang.toLowerCase().startsWith("zh");
    const bZh = b.lang.toLowerCase().startsWith("zh");
    if (aZh !== bZh) {
      return aZh ? -1 : 1;
    }
    return a.name.localeCompare(b.name);
  });
}

/** 本次 utterance 用的语音: 指定 URI 已失效或没指定时回落中文语音, 都没有就交给系统默认 */
function pickVoice(uri: string): SpeechSynthesisVoice | null {
  const list = window.speechSynthesis.getVoices();
  if (list.length === 0) {
    return null;
  }
  if (uri !== "") {
    const matched = list.find((voice) => voice.voiceURI === uri);
    if (matched !== undefined) {
      return matched;
    }
  }
  return list.find((voice) => voice.lang.toLowerCase().startsWith("zh")) ?? null;
}

/** 系统语音列表: getVoices 首次调用常是空的, 得等 voiceschanged 再收一次 */
function useVoices(supported: boolean): SpeechSynthesisVoice[] {
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);

  useEffect(() => {
    if (!supported) {
      return;
    }
    const load = (): void => {
      const list = window.speechSynthesis.getVoices();
      if (list.length > 0) {
        setVoices(sortVoices(list));
      }
    };
    load();
    window.speechSynthesis.addEventListener("voiceschanged", load);
    return () => {
      window.speechSynthesis.removeEventListener("voiceschanged", load);
    };
  }, [supported]);

  return voices;
}

/**
 * 语音朗读 (多引擎): 网关音源 (provider = auto 或引擎 id) 解析引擎与音色后逐段
 * fetch 网关 /tts → blob → Audio 播放, template 音源走自定义模板 URL, system 音源
 * 纯前端 Web Speech API 逐句 speak; 当前段播放中预取下一段. 段结束 (onended/onend)
 * 推进到下一段; 单段 fetch/播放失败 → 该段回退系统音并一次性 toast; 起读段落取正文
 * 可视区首段; 当前段落打 data-tts-active + accent 底色并滚进视口; 章末按「连读下一章」
 * 开关决定自动切章还是停下提示. 引擎全程读 ref (章节数据/回调/游标), 换章与滚动都不会重挂它.
 */
export function useTts(options: UseTtsOptions): UseTtsResult {
  const { items, containerRef, chapterIndex, resetKey } = options;
  const supported = typeof window !== "undefined" && "speechSynthesis" in window;
  const voices = useVoices(supported);

  const provider = useSettingsStore((state) => state.ttsProvider);
  const setProvider = useSettingsStore((state) => state.setTtsProvider);
  const rate = useSettingsStore((state) => state.ttsRate);
  const setRate = useSettingsStore((state) => state.setTtsRate);
  const voiceURI = useSettingsStore((state) => state.ttsVoiceURI);
  const setVoiceURI = useSettingsStore((state) => state.setTtsVoiceURI);
  const httpVoice = useSettingsStore((state) => state.ttsHttpVoice);
  const multiRole = useSettingsStore((state) => state.ttsMultiRole);
  const setTtsMultiRole = useSettingsStore((state) => state.setTtsMultiRole);
  const setHttpVoice = useSettingsStore((state) => state.setTtsHttpVoice);
  // 模板/网关地址只作「改动即重念当前段」的触发器, 取值走 getState
  const httpUrl = useSettingsStore((state) => state.ttsHttpUrl);
  const gatewayUrl = useSettingsStore((state) => state.ttsGatewayUrl);
  const autoNext = useSettingsStore((state) => state.ttsAutoNext);
  const setAutoNext = useSettingsStore((state) => state.setTtsAutoNext);

  const queryClient = useQueryClient();
  // 网关能力清单与解析引擎的音色: 引擎/音色下拉消费; 合成链路从同一份缓存解析 auto 与默认音色
  const gatewayStatus = useTtsGatewayStatus(gatewayUrl).data ?? null;
  const resolvedEngine = useMemo(() => {
    if (provider === "system" || provider === "template") {
      return "";
    }
    if (provider !== "auto") {
      return provider;
    }
    return gatewayStatus?.auto ?? "";
  }, [provider, gatewayStatus]);
  // 音色列表按引擎只留中文(其余语言用不上): edge 用 zh- 前缀, kokoro 用 zf_/zm_ 或「中文」名;
  // 其他引擎不过滤 (此前一刀切 zh- 把 kokoro 音色全滤空导致无声)
  const gatewayVoicesAll = useTtsGatewayVoices(gatewayUrl, resolvedEngine).data ?? EMPTY_GATEWAY_VOICES;
  // 只保留中文音色(与设置面板共用同一过滤口径, 见 filterChineseVoices)
  const gatewayVoices = useMemo(
    () => filterChineseVoices(resolvedEngine, gatewayVoicesAll),
    [gatewayVoicesAll, resolvedEngine],
  );
  const resolvedEngineName = useMemo(() => {
    if (resolvedEngine === "") {
      return "";
    }
    return gatewayStatus?.engines.find((entry) => entry.id === resolvedEngine)?.name ?? resolvedEngine;
  }, [resolvedEngine, gatewayStatus]);

  /**
   * 队列构建: 多角色模式(且非系统音源)按说话人分段并绑定音色, 否则沿用段落级单音色队列。
   * 系统音源由浏览器决定音色, 无法逐段指定 —— 多角色只对网关/模板音源生效。
   * 新出现的角色自动从当前引擎音色清单分配, 用户已手改的绑定保留。
   */
  const buildRoleQueue = useCallback(
    (source: ReaderParagraph[]): SpeechItem[] => {
      if (!multiRole || provider === "system") {
        return buildQueue(source);
      }
      const { segments, characters } = analyzeChapter(source);
      const merged = mergeAdjacent(segments);
      // 绑定必须经 getState 读、写也走 getState: 若把 roleVoices 放进本回调依赖,
      // 下面的写入会让回调立刻重建、播放队列被替换 —— 表现就是"朗读永远停在第一句"。
      const settings = useSettingsStore.getState();
      const storedTypes = settings.ttsRoleTypes;
      const roleTypes = assignRoleTypes(characters, storedTypes);
      // 只在真有新角色时写回(写入走 getState, 不进入本回调依赖, 否则会重建队列打断播放)
      if (Object.keys(roleTypes).length !== Object.keys(storedTypes).length) {
        settings.setTtsRoleTypes(roleTypes);
      }
      const typeVoices = settings.ttsTypeVoices;
      return merged.map((segment) => ({
        key: segment.paragraphKey,
        text: segment.text,
        speaker: segment.speaker,
        voice: voiceForSegment(segment, roleTypes, typeVoices, httpVoice),
      }));
    },
    [multiRole, provider, gatewayVoices, httpVoice],
  );

  const [status, setStatus] = useState<TtsStatus>("idle");
  const [activeKey, setActiveKey] = useState<number | null>(null);

  // 异步回调链 (utterance/fetch/Audio 事件) 里章节数据与回调都从 ref 取最新值, 免得闭包里是旧章节
  const optionsRef = useRef(options);
  useEffect(() => {
    optionsRef.current = options;
  });

  const statusRef = useRef<TtsStatus>("idle");
  const queueRef = useRef<SpeechItem[]>([]);
  const cursorRef = useRef(0);
  /** 每次起播/取消自增: 丢弃作废回调 (cancel 会补一次 onend, http 的 fetch/事件也会迟到) */
  const generationRef = useRef(0);
  /** 已经起播的章节: 换章续读靠它去重 */
  const chapterRef = useRef(chapterIndex);
  /** 持有当前 utterance: Chrome 会回收没有引用的 utterance, 朗读中途静音 */
  const utteranceRef = useRef<SpeechSynthesisUtterance | null>(null);
  /** http 引擎当前播放的 Audio (src = blob objectURL) */
  const audioRef = useRef<HTMLAudioElement | null>(null);
  /** audioRef 持有的 objectURL: 播完/作废时释放 */
  const audioUrlRef = useRef<string | null>(null);
  /** 当前段播放中预取的下一段音频 */
  const prefetchRef = useRef<PrefetchEntry | null>(null);
  /** 「回退系统音」提示一次播放会话只弹一遍 (start/stop 清零) */
  const fallbackNotifiedRef = useRef(false);
  /** playFrom 的转发口: 异步回调里推进用, 避开自引用闭包 */
  const playRef = useRef<(cursor: number, interrupt?: boolean) => void>(() => {});

  const applyStatus = useCallback((next: TtsStatus) => {
    statusRef.current = next;
    setStatus(next);
  }, []);

  /** 释放当前 Audio 与它的 objectURL */
  const releaseAudio = useCallback(() => {
    const audio = audioRef.current;
    if (audio !== null) {
      audio.onended = null;
      audio.onerror = null;
      audio.pause();
      audioRef.current = null;
    }
    if (audioUrlRef.current !== null) {
      URL.revokeObjectURL(audioUrlRef.current);
      audioUrlRef.current = null;
    }
  }, []);

  /** 作废预取: promise 结算后补 revoke objectURL (失败静默) */
  const clearPrefetch = useCallback(() => {
    const entry = prefetchRef.current;
    prefetchRef.current = null;
    if (entry !== null) {
      entry.promise.then((url) => URL.revokeObjectURL(url), () => {});
    }
  }, []);

  const stop = useCallback(() => {
    generationRef.current += 1;
    utteranceRef.current = null;
    if (supported) {
      window.speechSynthesis.cancel();
    }
    releaseAudio();
    clearPrefetch();
    queueRef.current = [];
    cursorRef.current = 0;
    fallbackNotifiedRef.current = false;
    applyStatus("idle");
    setActiveKey(null);
  }, [supported, applyStatus, releaseAudio, clearPrefetch]);

  /** 章末: 连读开着且还有下一章 → 交给 goChapter, 新章正文到位后由续读 effect 接上 */
  const finishChapter = useCallback(() => {
    const { chapterIndex: index, chapterCount, onNextChapter } = optionsRef.current;
    if (useSettingsStore.getState().ttsAutoNext && index + 1 < chapterCount) {
      onNextChapter();
      return;
    }
    stop();
    toast.info("本章朗读完毕");
  }, [stop]);

  /** 合成用引擎: auto 优先取能力清单缓存的解析值, 清单未就绪时交网关服务端解析 ("auto") */
  const resolveSynthEngine = useCallback(
    (current: TtsProvider): string => {
      if (current !== "auto") {
        return current;
      }
      const status = queryClient.getQueryData<GatewayStatus>(TTS_GATEWAY_STATUS_QUERY_KEY);
      return status?.auto ?? "auto";
    },
    [queryClient],
  );

  /** 合成用音色: 显式音色 > 引擎默认映射 > 音色清单缓存首个非空项 > 空串 (网关自会兜底默认) */
  const resolveSynthVoice = useCallback(
    (engine: string, storedVoice: string): string => {
      if (storedVoice !== "") {
        return storedVoice;
      }
      const mapped = DEFAULT_GATEWAY_VOICE[engine];
      if (mapped !== undefined && mapped !== "") {
        return mapped;
      }
      const list = queryClient.getQueryData<GatewayVoice[]>(ttsGatewayVoicesQueryKey(engine));
      return list?.find((voice) => voice.id !== "")?.id ?? "";
    },
    [queryClient],
  );

  /** 合成一段音频: template 走模板 URL, 其余走网关 (engine = provider|auto, voice = 显式音色或引擎默认) */
  const fetchSegmentAudio = useCallback(
    (item: SpeechItem): Promise<string> => {
      const { ttsProvider, ttsHttpUrl, ttsHttpVoice, ttsGatewayUrl } = useSettingsStore.getState();
      // 多角色: 片段自带音色(角色绑定)优先; 单音色模式回落到全局音色
      if (ttsProvider === "template") {
        return fetchTtsAudioUrl(ttsHttpUrl, item.text, item.voice ?? ttsHttpVoice);
      }
      const engine = resolveSynthEngine(ttsProvider);
      const voice = item.voice ?? resolveSynthVoice(engine, ttsHttpVoice);
      return fetchGatewayTtsAudioUrl(ttsGatewayUrl, item.text, voice, engine);
    },
    [resolveSynthEngine, resolveSynthVoice],
  );

  /** 预取下一段音频 (当前段就绪即发起); 失败静默 —— 真正播到该段会就地重试并走回退 */
  const beginPrefetch = useCallback(
    (cursor: number) => {
      const item = queueRef.current[cursor];
      if (item === undefined) {
        return;
      }
      const existing = prefetchRef.current;
      if (existing !== null && existing.cursor === cursor) {
        return;
      }
      clearPrefetch();
      const promise = fetchSegmentAudio(item);
      // 立刻挂 noop 兜底: 未被消费的预取失败不冒 unhandledrejection (消费方还会再挂正式处理器)
      promise.catch(() => {});
      prefetchRef.current = { cursor, promise };
    },
    [clearPrefetch, fetchSegmentAudio],
  );

  /** 系统音念一段: engine=system 主路径与 http 段失败回退共用; interrupt 时等 cancel 落定再排队 */
  const speakSystemSegment = useCallback(
    (item: SpeechItem, generation: number, interrupt: boolean) => {
      const speak = (): void => {
        // 等待期间被停止/再次跳段打断就作废
        if (generation !== generationRef.current) {
          return;
        }
        const utterance = new SpeechSynthesisUtterance(item.text);
        const settings = useSettingsStore.getState();
        utterance.rate = settings.ttsRate;
        const voice = pickVoice(settings.ttsVoiceURI);
        if (voice === null) {
          utterance.lang = FALLBACK_LANG;
        } else {
          utterance.voice = voice;
          utterance.lang = voice.lang;
        }
        utterance.onend = () => {
          if (generation !== generationRef.current) {
            return;
          }
          playRef.current(cursorRef.current + 1);
        };
        utterance.onerror = (event) => {
          if (generation !== generationRef.current) {
            return;
          }
          // 自己 cancel 派生的中断不算故障
          if (event.error === "interrupted" || event.error === "canceled") {
            return;
          }
          stop();
          toast.error("朗读中断, 已停止");
        };
        utteranceRef.current = utterance;
        window.speechSynthesis.speak(utterance);
      };

      if (interrupt) {
        window.setTimeout(speak, INTERRUPT_DELAY_MS);
      } else {
        speak();
      }
    },
    [stop],
  );

  /** http 单段失败 (fetch/解码/播放): 该段回退系统音, 一次播放会话内只提示一遍 */
  const fallbackToSystem = useCallback(
    (item: SpeechItem, generation: number) => {
      if (!supported) {
        stop();
        toast.error("神经音色不可用, 且当前浏览器不支持系统音");
        return;
      }
      if (!fallbackNotifiedRef.current) {
        fallbackNotifiedRef.current = true;
        toast.info("神经音色不可用, 回退系统音");
      }
      speakSystemSegment(item, generation, false);
    },
    [supported, stop, speakSystemSegment],
  );

  /** 神经音色播一段 (网关引擎或自定义模板): 命中预取直接消费, 否则现 fetch; 音频就绪交给 Audio 并预取下一段 */
  const playHttpSegment = useCallback(
    (cursor: number, item: SpeechItem, generation: number) => {
      const entry = prefetchRef.current;
      let urlPromise: Promise<string>;
      if (entry !== null && entry.cursor === cursor) {
        prefetchRef.current = null;
        urlPromise = entry.promise;
      } else {
        urlPromise = fetchSegmentAudio(item);
      }
      void urlPromise.then(
        (objectUrl) => {
          // 等待期间被停止/跳段: 音频直接作废
          if (generation !== generationRef.current) {
            URL.revokeObjectURL(objectUrl);
            return;
          }
          const audio = new Audio(objectUrl);
          audioRef.current = audio;
          audioUrlRef.current = objectUrl;
          audio.onended = () => {
            if (generation !== generationRef.current) {
              return;
            }
            releaseAudio();
            playRef.current(cursor + 1);
          };
          audio.onerror = () => {
            if (generation !== generationRef.current) {
              return;
            }
            releaseAudio();
            fallbackToSystem(item, generation);
          };
          beginPrefetch(cursor + 1);
          // 合成等待中被暂停: 音频就位不起播, 由 resume 续上
          if (statusRef.current === "paused") {
            return;
          }
          void audio.play().catch(() => {
            if (generation !== generationRef.current) {
              return;
            }
            releaseAudio();
            fallbackToSystem(item, generation);
          });
        },
        () => {
          if (generation !== generationRef.current) {
            return;
          }
          fallbackToSystem(item, generation);
        },
      );
    },
    [releaseAudio, beginPrefetch, fallbackToSystem, fetchSegmentAudio],
  );

  /**
   * 念第 cursor 段, 按引擎分派. interrupt = 打断式起播 (手动跳段/换章/换引擎换语音):
   * 掐掉在念的 utterance/在播的 Audio, 与目标段不匹配的预取作废 (匹配的留给播放流程复用).
   */
  const playFrom = useCallback(
    (cursor: number, interrupt = false) => {
      if (!supported) {
        return;
      }
      const queue = queueRef.current;
      if (cursor >= queue.length) {
        finishChapter();
        return;
      }
      const index = Math.max(0, cursor);
      const item = queue[index];
      if (item === undefined) {
        finishChapter();
        return;
      }

      if (interrupt) {
        generationRef.current += 1;
        window.speechSynthesis.cancel();
        releaseAudio();
        if (prefetchRef.current !== null && prefetchRef.current.cursor !== index) {
          clearPrefetch();
        }
      }
      const generation = ++generationRef.current;

      cursorRef.current = index;
      setActiveKey(item.key);
      applyStatus("playing");

      if (useSettingsStore.getState().ttsProvider === "system") {
        speakSystemSegment(item, generation, interrupt);
      } else {
        playHttpSegment(index, item, generation);
      }
    },
    [
      supported,
      finishChapter,
      applyStatus,
      releaseAudio,
      clearPrefetch,
      playHttpSegment,
      speakSystemSegment,
    ],
  );

  useEffect(() => {
    playRef.current = playFrom;
  }, [playFrom]);

  const start = useCallback(() => {
    if (!supported) {
      toast.info("当前浏览器不支持语音朗读");
      return;
    }
    const current = optionsRef.current;
    const queue = buildRoleQueue(current.items);
    if (queue.length === 0) {
      toast.info("本章没有可朗读的正文");
      return;
    }
    // 与自动滚动互斥: 朗读接管滚动 (高亮段落自己会滚进视口)
    current.onStopAutoScroll();
    queueRef.current = queue;
    chapterRef.current = current.chapterIndex;
    fallbackNotifiedRef.current = false;

    // 视口首段可能是章标题这类不可朗读的条目, 取它之后第一段可朗读的
    const visibleKey = firstVisibleParagraphKey(containerRef.current);
    let cursor = 0;
    if (visibleKey !== null) {
      const found = queue.findIndex((entry) => entry.key >= visibleKey);
      cursor = found === -1 ? queue.length - 1 : found;
    }
    playFrom(cursor, true);
  }, [supported, containerRef, playFrom]);

  const toggle = useCallback(() => {
    if (statusRef.current === "idle") {
      start();
      return;
    }
    stop();
  }, [start, stop]);

  const pause = useCallback(() => {
    if (statusRef.current !== "playing") {
      return;
    }
    if (audioRef.current !== null) {
      audioRef.current.pause();
    }
    if (supported) {
      window.speechSynthesis.pause();
    }
    applyStatus("paused");
  }, [supported, applyStatus]);

  const resume = useCallback(() => {
    if (statusRef.current !== "paused") {
      return;
    }
    applyStatus("playing");
    const audio = audioRef.current;
    if (audio !== null) {
      // http 音频已就位 (含合成等待中被暂停的段): 直接续播, 失败按首播同路回退
      void audio.play().catch(() => {
        if (statusRef.current === "idle") {
          return;
        }
        const item = queueRef.current[cursorRef.current];
        releaseAudio();
        if (item !== undefined) {
          fallbackToSystem(item, generationRef.current);
        }
      });
      return;
    }
    if (utteranceRef.current !== null) {
      window.speechSynthesis.resume();
      return;
    }
    // http 音频还在合成途中: 就绪回调看到 playing 会自行起播
  }, [applyStatus, releaseAudio, fallbackToSystem]);

  const step = useCallback(
    (delta: number) => {
      if (statusRef.current === "idle") {
        return;
      }
      const next = cursorRef.current + delta;
      if (next < 0) {
        toast.info("已经是本章第一段");
        return;
      }
      playFrom(next, true);
    },
    [playFrom],
  );

  // 换书 / 卸载 / 路由离开: 立刻掐掉朗读 (页面还活着的话 speechSynthesis/Audio 会一直响下去)
  useEffect(() => {
    stop();
    return () => {
      stop();
    };
  }, [resetKey, stop]);

  // 换章续读: 连读自动切章, 或朗读中手动切章 —— 新章正文到位后从章首接着念
  useEffect(() => {
    if (
      statusRef.current === "idle" ||
      chapterRef.current === chapterIndex ||
      items.length === 0
    ) {
      return;
    }
    chapterRef.current = chapterIndex;
    const queue = buildRoleQueue(items);
    if (queue.length === 0) {
      stop();
      toast.info("本章没有可朗读的正文");
      return;
    }
    queueRef.current = queue;
    playFrom(0, true);
  }, [chapterIndex, items, playFrom, stop]);

  // 音源/语音切换立刻生效: 用新配置重念当前段 (速度改动只对下一段生效, 免得连点步进器一直从头念)
  useEffect(() => {
    if (statusRef.current !== "playing" || queueRef.current.length === 0) {
      return;
    }
    playFrom(cursorRef.current, true);
  }, [provider, voiceURI, httpVoice, httpUrl, gatewayUrl, playFrom]);

  // 当前朗读段落: data-tts-active + accent/10 底色, 再滚进视口.
  // 直接改 DOM 而不透传 props —— 段落由 Virtuoso/多列渲染, 穿透一层会让整章跟着重渲染.
  useEffect(() => {
    const root = containerRef.current;
    if (root !== null) {
      for (const node of root.querySelectorAll("[data-tts-active]")) {
        node.removeAttribute("data-tts-active");
        node.classList.remove(HIGHLIGHT_CLASS);
      }
    }
    if (root === null || activeKey === null) {
      return;
    }
    const node = root.querySelector<HTMLElement>(`[data-para-index="${activeKey}"]`);
    if (node === null) {
      // 虚拟列表还没渲染出这一段 (超长章节), 只丢高亮, 朗读照常
      return;
    }
    node.setAttribute("data-tts-active", "");
    node.classList.add(HIGHLIGHT_CLASS);
    // smooth: 朗读推进时视口平滑跟随, 避免跨段时"闪现"到新位置
    node.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "smooth" });
  }, [activeKey, items, containerRef]);

  // 稳定引用: 阅读页随滚动逐帧重渲染, 朗读条不该跟着重渲染
  return useMemo(
    () => ({
      supported,
      status,
      activeKey,
      provider,
      setProvider,
      gatewayStatus,
      gatewayVoices,
      resolvedEngine,
      resolvedEngineName,
      voices,
      rate,
      setRate,
      voiceURI,
      setVoiceURI,
      httpVoice,
      setHttpVoice,
      autoNext,
      setAutoNext,
      multiRole,
      setMultiRole: setTtsMultiRole,
      start,
      toggle,
      pause,
      resume,
      stop,
      step,
    }),
    [
      supported,
      status,
      activeKey,
      provider,
      setProvider,
      gatewayStatus,
      gatewayVoices,
      resolvedEngine,
      resolvedEngineName,
      voices,
      rate,
      setRate,
      voiceURI,
      setVoiceURI,
      httpVoice,
      setHttpVoice,
      autoNext,
      setAutoNext,
      multiRole,
      setTtsMultiRole,
      start,
      toggle,
      pause,
      resume,
      stop,
      step,
    ],
  );
}
