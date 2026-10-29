/**
 * 关系图谱 demo 的数据契约 (graph-demo-v1).
 *
 * 这份数据由离线脚本生成, 前端只读: 公开静态文件 `/graph-demo-data.json`.
 * 字段名与后端/脚本约定一致, 不要改; 用 zod 宽容解析(缺字段给默认值),
 * 解析失败时页面回落到内联兜底数据(见 GraphDemoPage).
 */
import { z } from "zod";

/** 关系类型: 决定边的颜色/线型/箭头含义 */
export const RELATION_TYPES = [
  "parent",
  "spouse",
  "sibling",
  "master",
  "disciple",
  "ally",
  "enemy",
  "other",
] as const;

export type RelationType = (typeof RELATION_TYPES)[number];

/** 未知类型不报错, 归到 other(schema 演进时不至于整页回落兜底数据) */
const relationTypeSchema = z.enum(RELATION_TYPES).catch("other");

/** 章节号/代数: 缺字段或类型不对都给兜底值, 取整 */
const counter = (fallback: number) =>
  z
    .number()
    .catch(fallback)
    .transform((value) => Math.round(value));

const chapterOrNull = z
  .number()
  .nullish()
  .transform((value) => (value == null ? null : Math.round(value)));

const characterSchema = z.object({
  id: z.string(),
  name: z.string(),
  aliases: z.array(z.string()).catch([]),
  gender: z.string().catch("未知"),
  role: z.string().catch(""),
  faction: z.string().nullish().transform((value) => value ?? null),
  generation: counter(1),
  firstSeen: counter(1),
  lastSeen: chapterOrNull,
  conflict: z.boolean().catch(false),
});

const factionSchema = z.object({
  id: z.string(),
  name: z.string(),
  /** 门派/家族/势力/组织 —— 决定图例分组文案 */
  kind: z.string().catch("组织"),
  leader: z.string().nullish().transform((value) => value ?? null),
  members: z.array(z.string()).catch([]),
});

const membershipSchema = z.object({
  character: z.string(),
  faction: z.string(),
  from: counter(1),
  to: chapterOrNull,
});

const relationSchema = z.object({
  from: z.string(),
  to: z.string(),
  type: relationTypeSchema,
  /** 中文关系名(师徒/父子/宿敌…), 缺省时用类型名 */
  label: z.string().optional(),
  from_chapter: counter(1),
  to_chapter: chapterOrNull,
  quote: z.string().optional(),
});

/** 元信息: demo 静态文件与后端 /graph 共用(后者在此之上扩展 source/cost, 见 services/graph.ts) */
export const graphMetaSchema = z.object({
  book: z.string().catch("未知作品"),
  totalChapters: counter(1),
  sampledChapters: counter(0),
  generatedAt: z.string().catch(""),
  schema: z.string().catch("graph-demo-v1"),
});

const statsSchema = z
  .object({
    characters: counter(0),
    relations: counter(0),
    factions: counter(0),
  })
  .catch({ characters: 0, relations: 0, factions: 0 });

export const graphDemoSchema = z.object({
  meta: graphMetaSchema,
  characters: z.array(characterSchema).catch([]),
  factions: z.array(factionSchema).catch([]),
  memberships: z.array(membershipSchema).catch([]),
  relations: z.array(relationSchema).catch([]),
  stats: statsSchema.optional(),
});

export type GraphCharacter = z.infer<typeof characterSchema>;
export type GraphFaction = z.infer<typeof factionSchema>;
export type GraphMembership = z.infer<typeof membershipSchema>;
export type GraphRelation = z.infer<typeof relationSchema>;
export type GraphDemoMeta = z.infer<typeof graphMetaSchema>;
export type GraphDemoData = z.infer<typeof graphDemoSchema>;

export type ParseResult =
  | { ok: true; data: GraphDemoData }
  | { ok: false; message: string };

/** 解析并校验静态 JSON; 任何结构问题都转成一句可展示的中文原因. */
export function parseGraphDemoData(raw: unknown): ParseResult {
  const parsed = graphDemoSchema.safeParse(raw);
  if (!parsed.success) {
    const first = parsed.error.issues[0];
    const where = first?.path.join(".") || "(root)";
    return { ok: false, message: `数据校验失败: ${where} ${first?.message ?? ""}`.trim() };
  }
  if (parsed.data.characters.length === 0) {
    return { ok: false, message: "数据校验失败: characters 为空" };
  }
  return { ok: true, data: parsed.data };
}
