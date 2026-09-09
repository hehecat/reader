import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

export type ThemeMode = "light" | "dark" | "sepia" | "green" | "system";
export type FontFamilyMode = "serif" | "sans" | "kai" | "custom";
export type ReadMode = "scroll" | "page";
/**
 * TTS 音源: auto = 网关自动选择 (自建实测够快 → 免费云 → 密钥云), system = 浏览器 speechSynthesis,
 * template = 自定义 HTTP 模板 (ttsHttpUrl), 其余取值 = 网关引擎 id (kokoro/cosyvoice/gptsovits/edge/tencent/generic).
 */
export type TtsProvider = "auto" | "system" | "template" | (string & {});

export const SETTINGS_STORAGE_KEY = "reader.settings";

/** 字号范围 (px) */
export const FONT_SIZE_RANGE = { min: 14, max: 28, step: 1 } as const;
/** 行距范围 (无单位倍数) */
export const LINE_HEIGHT_RANGE = { min: 1.4, max: 2.2, step: 0.05 } as const;
/** 自动滚动速度范围 (px/s) */
export const AUTO_SCROLL_SPEED_RANGE = { min: 10, max: 120, step: 10 } as const;
/** 朗读速度范围 (倍率) */
export const TTS_RATE_RANGE = { min: 0.6, max: 2, step: 0.2 } as const;
/** 默认神经音源模板: 本地 edge-tts 服务 (9912), {text}/{voice} 占位由 buildTtsUrl 替换 */
export const EDGE_TTS_LOCAL_TEMPLATE = "http://127.0.0.1:9912/tts?text={text}&voice={voice}";
/** 默认 TTS 网关地址: 空串 = 同源代理 /tts-gateway (跨设备/https 安全), 可填绝对地址指向其他网关 */
export const TTS_GATEWAY_DEFAULT_URL = "";

export interface SettingsState {
  /** light/dark/sepia/green, system 表示跟随系统 */
  theme: ThemeMode;
  /** 正文字号 px, 14-28 */
  fontSize: number;
  /** 行距倍数, 1.4-2.2 */
  lineHeight: number;
  /** 段距 em */
  paragraphGap: number;
  /** 字体: 宋体(serif) / 黑体(sans) / 楷体(kai) / 自定义(custom, 用 customFontFamily 串) */
  fontFamily: FontFamilyMode;
  /** 滚动 / 翻页 */
  readMode: ReadMode;
  /** 首行缩进 */
  indentParagraph: boolean;
  /** 正文最大宽度 em */
  contentWidth: number;
  /** 自定义 font-family 串 (fontFamily=custom 时生效, 空串回退衬线) */
  customFontFamily: string;
  /** 自动滚动速度 px/s, 10-120 */
  autoScrollSpeed: number;
  /** 朗读速度倍率, 0.6-2.0 */
  ttsRate: number;
  /** 朗读语音 (SpeechSynthesisVoice.voiceURI), 空串 = 默认语音 (优先中文) */
  ttsVoiceURI: string;
  /** TTS 音源 (默认 auto: 网关按资源可用性自动选择, 单段失败自动回退系统音) */
  ttsProvider: TtsProvider;
  /** TTS 网关地址: 引擎能力清单与合成都走它 (部署方经网关环境变量配置引擎端点/密钥) */
  ttsGatewayUrl: string;
  /** 自定义模板 URL (ttsProvider=template 时生效): {text}/{voice} 占位, 缺占位时降级为 query 参数 */
  ttsHttpUrl: string;
  /** 音色名: 网关引擎音色 id 或模板音色名, 空串 = 用当前引擎的默认音色 */
  ttsHttpVoice: string;
  /** 读完本章自动接着读下一章 */
  ttsAutoNext: boolean;
  /** 读到章末自动后台预热缓存后续章节 (源站失效后仍可读) */
  preheatOnChapterEnd: boolean;
  /** 单源搜索超时秒(3-60): 运行时下发后端, 即时保存生效, 调参无需重新构建 */
  searchTimeout: number;
  /** 隐藏目录解析 0 章的搜索结果(后台探针校验, 缓存 7 天) */
  /** 加入书架/导入后自动后台预热整书, 把首开抓取前移 (默认开) */
  preheatOnAdd: boolean;
  setTheme: (theme: ThemeMode) => void;
  setFontSize: (fontSize: number) => void;
  setLineHeight: (lineHeight: number) => void;
  setParagraphGap: (paragraphGap: number) => void;
  setFontFamily: (fontFamily: FontFamilyMode) => void;
  setReadMode: (readMode: ReadMode) => void;
  setIndentParagraph: (indentParagraph: boolean) => void;
  setContentWidth: (contentWidth: number) => void;
  setCustomFontFamily: (customFontFamily: string) => void;
  setAutoScrollSpeed: (autoScrollSpeed: number) => void;
  setTtsRate: (ttsRate: number) => void;
  setTtsVoiceURI: (ttsVoiceURI: string) => void;
  setTtsProvider: (ttsProvider: TtsProvider) => void;
  setTtsGatewayUrl: (ttsGatewayUrl: string) => void;
  setTtsHttpUrl: (ttsHttpUrl: string) => void;
  setTtsHttpVoice: (ttsHttpVoice: string) => void;
  setTtsAutoNext: (ttsAutoNext: boolean) => void;
  setPreheatOnChapterEnd: (preheatOnChapterEnd: boolean) => void;
  setPreheatOnAdd: (preheatOnAdd: boolean) => void;
  setSearchTimeout: (searchTimeout: number) => void;
}

/** 可持久化的设置数据(不含 setter) */
export type SettingsData = Omit<SettingsState, `set${string}`>;

/** 默认阅读设置, 也可用于「恢复默认」: useSettingsStore.setState(defaultSettings) */
export const defaultSettings: SettingsData = {
  theme: "system",
  fontSize: 18,
  lineHeight: 1.8,
  paragraphGap: 0.75,
  fontFamily: "sans",
  readMode: "scroll",
  indentParagraph: true,
  contentWidth: 42,
  customFontFamily: "",
  autoScrollSpeed: 40,
  ttsRate: 1,
  ttsVoiceURI: "",
  ttsProvider: "auto",
  ttsGatewayUrl: TTS_GATEWAY_DEFAULT_URL,
  ttsHttpUrl: EDGE_TTS_LOCAL_TEMPLATE,
  ttsHttpVoice: "zh-CN-YunxiNeural",
  ttsAutoNext: true,
  preheatOnChapterEnd: false,
  preheatOnAdd: true,
  searchTimeout: 15,
};

export const useSettingsStore = create<SettingsState>()(
  persist(
    (set) => ({
      ...defaultSettings,

      setTheme: (theme) => {
        set({ theme });
      },

      setFontSize: (fontSize) => {
        set({
          fontSize: Math.min(Math.max(fontSize, FONT_SIZE_RANGE.min), FONT_SIZE_RANGE.max),
        });
      },

      setLineHeight: (lineHeight) => {
        set({
          lineHeight: Math.min(
            Math.max(lineHeight, LINE_HEIGHT_RANGE.min),
            LINE_HEIGHT_RANGE.max,
          ),
        });
      },

      setParagraphGap: (paragraphGap) => {
        set({ paragraphGap });
      },

      setFontFamily: (fontFamily) => {
        set({ fontFamily });
      },

      setReadMode: (readMode) => {
        set({ readMode });
      },

      setIndentParagraph: (indentParagraph) => {
        set({ indentParagraph });
      },

      setContentWidth: (contentWidth) => {
        set({ contentWidth });
      },

      setCustomFontFamily: (customFontFamily) => {
        set({ customFontFamily });
      },

      setAutoScrollSpeed: (autoScrollSpeed) => {
        set({
          autoScrollSpeed: Math.min(
            Math.max(autoScrollSpeed, AUTO_SCROLL_SPEED_RANGE.min),
            AUTO_SCROLL_SPEED_RANGE.max,
          ),
        });
      },

      setTtsRate: (ttsRate) => {
        const { min, max, step } = TTS_RATE_RANGE;
        const clamped = Math.min(Math.max(ttsRate, min), max);
        // 收敛到步进格点再抹掉浮点尾差 (0.6 + 0.2 * 2 = 1.0000000000000002)
        set({ ttsRate: Math.round(Math.round(clamped / step) * step * 100) / 100 });
      },

      setTtsVoiceURI: (ttsVoiceURI) => {
        set({ ttsVoiceURI });
      },

      setTtsProvider: (ttsProvider) => {
        set({ ttsProvider });
      },

      setTtsGatewayUrl: (ttsGatewayUrl) => {
        set({ ttsGatewayUrl });
      },

      setTtsHttpUrl: (ttsHttpUrl) => {
        set({ ttsHttpUrl });
      },

      setTtsHttpVoice: (ttsHttpVoice) => {
        set({ ttsHttpVoice });
      },

      setTtsAutoNext: (ttsAutoNext) => {
        set({ ttsAutoNext });
      },

      setPreheatOnChapterEnd: (preheatOnChapterEnd) => {
        set({ preheatOnChapterEnd });
      },
      setPreheatOnAdd: (preheatOnAdd) => {
        set({ preheatOnAdd });
      },

      setSearchTimeout: (searchTimeout) => {
        set({ searchTimeout: Math.min(Math.max(Math.round(searchTimeout), 3), 60) });
      },


    }),
    {
      name: SETTINGS_STORAGE_KEY,
      version: 3,
      storage: createJSONStorage(() => localStorage),
      // v1 → v2: ttsEngine (http/system) 换成 ttsProvider —— http 迁 auto, system 保留;
      // v2 → v3: 本机回环网关地址 (浏览器在他机即失效 + https 混合内容) 迁同源代理空串
      migrate: (persisted, version) => {
        let state = (persisted ?? {}) as Partial<SettingsData> & {
          ttsEngine?: "http" | "system";
        };
        if (version < 2) {
          const { ttsEngine, ...rest } = state;
          state = {
            ...rest,
            ttsProvider: ttsEngine === "system" ? "system" : "auto",
          } as Partial<SettingsData>;
        }
        if (version < 3 && state.ttsGatewayUrl === "http://127.0.0.1:9912") {
          state = { ...state, ttsGatewayUrl: "" };
        }
        return state as SettingsState;
      },
    },
  ),
);
