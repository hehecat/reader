import type { ReaderParagraph } from "@/hooks/useChapterContent";

/**
 * 多角色朗读: 把正文段落切成「句级片段」并标注说话人。
 *
 * 设计取舍(与阅读 App 的 MultiTTS 同思路, 但纯前端、纯规则):
 * - 只认高置信度模式(`X说道:"…"` / `"…"X道` / `X:…`), 其余一律回退旁白 ——
 *   识别错误导致的"音色乱串"比"少识别几句"更伤体验, 所以宁缺勿滥。
 * - 无外部依赖, 不调用 LLM; 后续若要处理"连续无标签对白", 在 P3 用按钮手动触发。
 */

/** 说话人识别结果; speaker 为空串 = 旁白 */
export interface TtsSegment {
  /** 全局唯一 id(合并后仍稳定, 用于音频缓存键) */
  id: string;
  /** 所属段落 key(与正文 DOM 的 data-para-index 同源, 用于高亮) */
  paragraphKey: number;
  text: string;
  /** 说话人; 空串 = 旁白 */
  speaker: string;
  /** 置信度 0-1; < LOW_CONFIDENCE 时按策略回退旁白 */
  confidence: number;
}

/** 低于此置信度的识别结果不切音色(回退旁白) */
export const LOW_CONFIDENCE = 0.7;

/**
 * 同段落内相邻同角色片段的合并上限(字): 段落普遍短于它 → 一段合成一次,
 * 高亮随段落自然推进; 切得太碎会让合成排队、播放断续(表现为高亮卡住后突跳)。
 */
const MERGE_LIMIT = 400;

/** 常见"看着像人名却不是人名"的词: 命中即不作为说话人(静态表, 用 Record) */
const NOT_A_NAME: Record<string, true> = {
  他们: true, 她们: true, 我们: true, 你们: true, 众人: true, 所有人: true,
  大家: true, 两人: true, 三人: true, 几人: true, 这人: true, 那人: true,
  此人: true, 有人: true, 没人: true, 后者: true, 前者: true, 对方: true,
  自己: true, 旁人: true, 这时: true, 此时: true, 顿时: true, 忽然: true,
  突然: true, 于是: true, 但是: true, 可是: true, 然而: true, 如果: true,
  因为: true, 所以: true, 只是: true, 不过: true, 然后: true, 接着: true,
  随即: true, 毕竟: true, 似乎: true, 仿佛: true, 最终: true, 最后: true,
  同时: true, 其中: true, 另外: true, 而且: true, 一边: true, 说着: true,
  闻言: true, 见状: true,
};

/** 说话动词: 识别 `X<动词>` 与 `"…"X<动词>` 两类模式 */
const SPEAK_VERBS = [
  "说道", "问道", "喊道", "叫道", "笑道", "答道", "回道", "应道", "喝道", "叹道",
  "低声道", "轻声道", "沉声道", "冷声道", "开口道", "继续道", "接着道", "解释道",
  "回答", "开口", "反问", "追问", "沉吟", "叹气", "嘀咕", "嘟囔", "呢喃",
  "说", "问", "喊", "叫", "答", "道",
];

/** 人名: 2-4 个汉字(允许中间的点, 如"艾米·王"不合规则故不处理) */
const NAME = "[\\u4e00-\\u9fa5]{2,3}?";

/** 引号对: 中文双引号 / 直角引号 / 英文双引号 */
const QUOTE_OPEN = "[“「\"]";
const QUOTE_CLOSE = "[”」\"]";

/** 句末切分: 保留标点; 引号闭合后也算边界 */
const SENTENCE_SPLIT = /(?<=[。！？!?…])(?=[^”」"』】）)])|(?<=[。！？!?…][”」"])/;

interface SpeakerHit {
  speaker: string;
  confidence: number;
}

/** 从一句话里识别说话人; 识别不出返回 null */
function detectSpeaker(sentence: string): SpeakerHit | null {
  const text = sentence.trim();
  if (text === "") {
    return null;
  }
  // ① `X说道:"…"` / `X道:「…」`  —— 人名在引号之前
  const beforeQuote = new RegExp(
    `^(${NAME})(?:${SPEAK_VERBS.join("|")})[,:：]?\\s*${QUOTE_OPEN}`,
  ).exec(text);
  if (beforeQuote?.[1] !== undefined) {
    // 已是「X+说话动词」结构: 名字是排除词(他们/我们…)即判定旁白, 不再退化匹配 `X:` 模式
    return NOT_A_NAME[beforeQuote[1]] === true
      ? null
      : { speaker: beforeQuote[1], confidence: 0.95 };
  }
  // ② `"…"X说道` —— 人名在引号之后(最常见)
  const afterQuote = new RegExp(
    `${QUOTE_CLOSE}\\s*(${NAME})(?:${SPEAK_VERBS.join("|")})`,
  ).exec(text);
  if (afterQuote?.[1] !== undefined) {
    return NOT_A_NAME[afterQuote[1]] === true
      ? null
      : { speaker: afterQuote[1], confidence: 0.9 };
  }
  // ③ `X:"…"` 直接给话(聊天/系统提示风格) —— **必须后接引号**:
  // 否则 `力量：150`、`敏捷：12` 这类属性面板/清单文本会被误判成说话人
  const colon = new RegExp(`^(${NAME})[：:]\\s*${QUOTE_OPEN}`).exec(text);
  if (colon?.[1] !== undefined) {
    return NOT_A_NAME[colon[1]] === true ? null : { speaker: colon[1], confidence: 0.85 };
  }
  return null;
}

/** 按句读切分(不做复杂 NLP, 中文标点足够) */
function splitSentences(text: string): string[] {
  return text
    .split(SENTENCE_SPLIT)
    .map((part) => part.trim())
    .filter((part) => part !== "");
}

/**
 * 整章分析: 段落 → 句级片段 + 说话人。
 * 段落内先按句读切分(对白与旁白常同段), 再逐句识别; 无标签对白回退旁白。
 */
export function analyzeChapter(paragraphs: readonly ReaderParagraph[]): {
  segments: TtsSegment[];
  characters: string[];
} {
  const segments: TtsSegment[] = [];
  const characters: string[] = [];
  let seq = 0;
  for (const paragraph of paragraphs) {
    if (paragraph.type !== "paragraph") {
      continue;
    }
    for (const sentence of splitSentences(plainText(paragraph.text))) {
      const hit = detectSpeaker(sentence);
      const speaker = hit !== null && hit.confidence >= LOW_CONFIDENCE ? hit.speaker : "";
      if (speaker !== "" && !characters.includes(speaker)) {
        characters.push(speaker);
      }
      segments.push({
        id: `s${String(seq)}`,
        paragraphKey: paragraph.key,
        text: sentence,
        speaker,
        // 无标签但明显是对白(带引号) → 低置信; 纯叙述给满分
        confidence:
          hit?.confidence ??
          (new RegExp(`${QUOTE_OPEN}[^${QUOTE_CLOSE}]{1,200}${QUOTE_CLOSE}`).test(sentence)
            ? 0.4
            : 1),
      });
      seq += 1;
    }
  }
  // 跨句归并: `“你来了。”老赵道。` 被句读切成两句, 说话人掉到后一句 ——
  // 后一句若只是「人名+说话动词」, 把角色并回前一句并去掉这半句(免得朗读念出来)
  const tailSpeaker = new RegExp(`^(${NAME})(?:${SPEAK_VERBS.join("|")})[。！？!?…]?$`);
  for (let i = 1; i < segments.length; i += 1) {
    const prev = segments[i - 1];
    const current = segments[i];
    if (prev === undefined || current === undefined || prev.speaker !== "") {
      continue;
    }
    if (!new RegExp(`${QUOTE_CLOSE}$`).test(prev.text)) {
      continue;
    }
    const name = tailSpeaker.exec(current.text)?.[1];
    if (name === undefined || NOT_A_NAME[name] === true) {
      continue;
    }
    prev.speaker = name;
    prev.confidence = 0.9;
    prev.text = `${prev.text}${current.text}`;
    if (!characters.includes(name)) {
      characters.push(name);
    }
    segments.splice(i, 1);
    i -= 1;
  }

  return { segments, characters };
}

/** 段落 HTML → 纯文本(与 useTts 同口径: DOMParser 不加载资源) */
function plainText(html: string): string {
  const body = new DOMParser().parseFromString(html, "text/html").body;
  return (body.textContent ?? "").replace(/\s+/g, " ").trim();
}

/**
 * 合并相邻同角色片段: 多角色会把一段拆成多句, 逐句请求会让合成次数暴涨。
 * 同角色且未超上限的相邻片段合并成一次合成(高亮仍按合并后的片段走)。
 */
export function mergeAdjacent(segments: readonly TtsSegment[]): TtsSegment[] {
  const out: TtsSegment[] = [];
  for (const segment of segments) {
    const last = out[out.length - 1];
    if (
      last !== undefined &&
      // 同段落才合并: 跨段落合并会让合并片段的 paragraphKey 停在首段,
      // 朗读时高亮(按 paragraphKey 定位)就再也不跟随了
      last.paragraphKey === segment.paragraphKey &&
      last.speaker === segment.speaker &&
      last.text.length + segment.text.length <= MERGE_LIMIT
    ) {
      last.text = `${last.text}${segment.text}`;
      continue;
    }
    out.push({ ...segment });
  }
  return out;
}

/**
 * 角色类型: 用户只需把这些类型的音色配一次(不必等识别出角色), 识别到的角色再归入某个类型。
 * narration 不参与自动分配 —— 旁白用全局音色。
 */
export const ROLE_TYPES = [
  { id: "youngMale", label: "年轻男性" },
  { id: "youngFemale", label: "年轻女性" },
  { id: "middleMale", label: "中年男性" },
  { id: "middleFemale", label: "中年女性" },
  { id: "oldMale", label: "年老男性" },
  { id: "oldFemale", label: "年老女性" },
  { id: "child", label: "儿童" },
] as const;

export type RoleTypeId = (typeof ROLE_TYPES)[number]["id"];

/**
 * 类型 → 默认音色: 开箱即用(不必让用户先配), 基于 edge 的中文音色 ——
 * edge 只有 4 男 4 女、没有儿童音色, 故部分类型就近复用(儿童用最年轻的女声)。
 * 用户可在设置里逐项改; 换到火山/阿里等引擎时按该引擎清单重选。
 */
export const DEFAULT_TYPE_VOICES: Record<string, string> = {
  youngMale: "zh-CN-YunxiNeural",
  youngFemale: "zh-CN-XiaoyiNeural",
  middleMale: "zh-CN-YunjianNeural",
  middleFemale: "zh-CN-XiaoxiaoNeural",
  oldMale: "zh-CN-YunyangNeural",
  oldFemale: "zh-CN-liaoning-XiaobeiNeural",
  child: "zh-CN-XiaoyiNeural",
};

/** 类型 id → 中文名(用于面板与徽标) */
export const ROLE_TYPE_LABEL: Record<string, string> = Object.fromEntries(
  ROLE_TYPES.map((entry) => [entry.id, entry.label]),
);

/**
 * 角色 → 类型自动归类: 新出现的角色按顺序轮换类型(不覆盖已有的),
 * 用户可在设置里改成更贴切的类型。
 */
export function assignRoleTypes(
  characters: readonly string[],
  existing: Readonly<Record<string, string>>,
): Record<string, string> {
  const out: Record<string, string> = { ...existing };
  let cursor = Object.keys(existing).length;
  for (const name of characters) {
    if (out[name] !== undefined) {
      continue;
    }
    out[name] = ROLE_TYPES[cursor % ROLE_TYPES.length]?.id ?? "youngMale";
    cursor += 1;
  }
  return out;
}

/** 片段 → 实际音色: 角色 → 类型 → 类型音色; 旁白或未配置时回落全局音色 */
export function voiceForSegment(
  segment: TtsSegment,
  roleTypes: Readonly<Record<string, string>>,
  typeVoices: Readonly<Record<string, string>>,
  narrationVoice: string,
): string {
  if (segment.speaker === "") {
    return narrationVoice;
  }
  const type = roleTypes[segment.speaker];
  if (type === undefined) {
    return narrationVoice;
  }
  // 类型音色优先取用户配置, 未配置则用内置默认(开箱即用), 再不行才回落旁白音色
  const voice = typeVoices[type] ?? DEFAULT_TYPE_VOICES[type];
  return voice === undefined || voice === "" ? narrationVoice : voice;
}
