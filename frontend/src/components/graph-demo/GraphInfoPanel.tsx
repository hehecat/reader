/**
 * 信息面板: 点节点看人物档案 + 全部关系, 点边看这条关系本身.
 * 两种情况共用一个面板(需求要求"塞进同一个面板").
 *
 * 不再占右侧固定栏 —— 那会永久吃掉 250px 画布宽度. 改成选中才滑入的悬浮卡:
 * 桌面贴右侧, 手机(窄屏)变成底部抽屉; 未选中时整块移出可视区(pointer-events: none).
 */
import { ChevronRight, Quote, X } from "lucide-react";

import { Badge, IconButton } from "@/components/ui";
import { cn } from "@/components/ui/cn";

import type { GraphDemoData, GraphRelation } from "@/types/graphDemo";

import { NEUTRAL_COLOR, RELATION_VISUAL } from "./graphTheme";
import type { Visibility } from "./graphVisibility";

export type GraphSelection = { kind: "node"; id: string } | { kind: "edge"; id: string };

export interface GraphInfoPanelProps {
  data: GraphDemoData;
  factionColors: Map<string, string>;
  visibility: Visibility;
  selection: GraphSelection | null;
  onClose: () => void;
  /** 点关系里的对方姓名 → 切到那个人 */
  onSelectCharacter: (id: string) => void;
}

const EMPTY_HINT = "点一个角色或一条关系线，看它的档案与原文根据。";

export function GraphInfoPanel({
  data,
  factionColors,
  visibility,
  selection,
  onClose,
  onSelectCharacter,
}: GraphInfoPanelProps) {
  const open = selection !== null;

  return (
    <aside
      aria-hidden={!open}
      data-testid="graph-info-panel"
      className={cn(
        "absolute z-30 flex flex-col overflow-hidden rounded-2xl border border-border bg-surface/95 shadow-lg backdrop-blur",
        "transition-[transform,opacity] duration-200 ease-out",
        // 窄屏: 底部抽屉(全宽卡片)
        "inset-x-2 bottom-2 top-auto max-h-[62%]",
        // 桌面: 右侧悬浮卡, 不占画布宽度
        "md:inset-x-auto md:bottom-3 md:right-3 md:top-3 md:max-h-none md:w-[19.5rem]",
        open
          ? "translate-x-0 translate-y-0 opacity-100"
          : "pointer-events-none translate-y-[calc(100%+1rem)] opacity-0 md:translate-y-0 md:translate-x-[calc(100%+1.5rem)]",
      )}
    >
      <div className="flex items-center justify-between gap-2 border-b border-border px-4 py-3">
        <p className="text-xs font-medium tracking-wide text-muted-foreground">
          {selection === null ? "信息面板" : selection.kind === "node" ? "人物档案" : "关系详情"}
        </p>
        {open ? (
          <IconButton aria-label="关闭面板" tooltip="关闭" size="sm" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        ) : null}
      </div>

      <div className="flex-1 overflow-y-auto px-4 py-4">
        {selection === null ? (
          <p className="text-xs leading-relaxed text-muted-foreground">{EMPTY_HINT}</p>
        ) : selection.kind === "node" ? (
          <CharacterDetail
            data={data}
            factionColors={factionColors}
            visibility={visibility}
            characterId={selection.id}
            onSelectCharacter={onSelectCharacter}
          />
        ) : (
          <RelationDetail
            data={data}
            visibility={visibility}
            relationIndex={Number(selection.id.slice("rel-".length))}
            onSelectCharacter={onSelectCharacter}
          />
        )}
      </div>
    </aside>
  );
}

interface CharacterDetailProps {
  data: GraphDemoData;
  factionColors: Map<string, string>;
  visibility: Visibility;
  characterId: string;
  onSelectCharacter: (id: string) => void;
}

function CharacterDetail({
  data,
  factionColors,
  visibility,
  characterId,
  onSelectCharacter,
}: CharacterDetailProps) {
  const character = data.characters.find((item) => item.id === characterId);
  if (!character) {
    return <p className="text-xs text-muted-foreground">这个节点不在当前数据里。</p>;
  }

  const faction = data.factions.find((item) => item.id === character.faction);
  const factionColor = (character.faction && factionColors.get(character.faction)) || NEUTRAL_COLOR;

  // 这个人的全部关系(有向/无向都列, 但把"对方"归一成另一边)
  const relationRows = data.relations
    .map((relation, index) => ({ relation, index }))
    .filter(({ relation }) => relation.from === characterId || relation.to === characterId)
    .sort((a, b) => a.relation.from_chapter - b.relation.from_chapter);

  return (
    <div className="space-y-4">
      <header className="space-y-1.5">
        <div className="flex items-center gap-2">
          <span className="size-2.5 shrink-0 rounded-full" style={{ backgroundColor: factionColor }} />
          <h2 className="truncate text-lg font-semibold text-foreground">{character.name}</h2>
        </div>
        {character.aliases.length > 0 ? (
          <p className="pl-4.5 text-xs text-muted-foreground">别名：{character.aliases.join("、")}</p>
        ) : null}
        <div className="flex flex-wrap items-center gap-1.5 pl-4.5 pt-0.5">
          <Badge variant="outline" size="sm">
            {character.gender || "未知"}
          </Badge>
          {character.role ? (
            <Badge variant="accent" size="sm">
              {character.role}
            </Badge>
          ) : null}
          <Badge variant="muted" size="sm">
            第 {character.generation} 代
          </Badge>
          {character.conflict ? (
            <Badge variant="danger" size="sm">
              立场冲突
            </Badge>
          ) : null}
        </div>
      </header>

      <dl className="space-y-1.5 rounded-lg border border-border bg-surface-muted/60 px-3 py-2.5 text-xs">
        <Row label="所属势力" value={faction ? `${faction.name} · ${faction.kind}` : "无"} />
        <Row label="首次出现" value={`第 ${character.firstSeen} 章`} />
        <Row label="最近出现" value={character.lastSeen === null ? "至今" : `第 ${character.lastSeen} 章`} />
        <Row label="关系数" value={`${relationRows.length} 条`} />
      </dl>

      <section className="space-y-2">
        <p className="text-[11px] font-medium tracking-wide text-muted-foreground">
          关系 · {relationRows.length}
        </p>
        {relationRows.length === 0 ? (
          <p className="text-xs text-muted-foreground">还没有和别人的关系记录。</p>
        ) : null}
        {relationRows.map(({ relation, index }) => (
          <RelationRow
            key={`${relation.from}-${relation.to}-${index}`}
            data={data}
            relation={relation}
            relationIndex={index}
            locked={!visibility.relations.has(index)}
            selfId={character.id}
            onSelectCharacter={onSelectCharacter}
          />
        ))}
      </section>
    </div>
  );
}

interface RelationDetailProps {
  data: GraphDemoData;
  visibility: Visibility;
  relationIndex: number;
  onSelectCharacter: (id: string) => void;
}

function RelationDetail({ data, visibility, relationIndex, onSelectCharacter }: RelationDetailProps) {
  const relation = data.relations[relationIndex];
  if (!relation) {
    return <p className="text-xs text-muted-foreground">这条关系不在当前数据里。</p>;
  }
  const visual = RELATION_VISUAL[relation.type];
  const fromName = nameOf(data, relation.from);
  const toName = nameOf(data, relation.to);

  return (
    <div className="space-y-4">
      <header className="space-y-1.5">
        <div className="flex items-center gap-2">
          <span className="h-2.5 w-6 shrink-0 rounded-full" style={{ backgroundColor: visual.color }} />
          <h2 className="text-lg font-semibold text-foreground">{relation.label ?? visual.name}</h2>
        </div>
        <p className="text-xs text-muted-foreground">
          <button
            type="button"
            className="rounded underline-offset-2 hover:text-accent hover:underline"
            onClick={() => onSelectCharacter(relation.from)}
          >
            {fromName}
          </button>
          <ChevronRight className="mx-1 inline size-3 align-middle" />
          <button
            type="button"
            className="rounded underline-offset-2 hover:text-accent hover:underline"
            onClick={() => onSelectCharacter(relation.to)}
          >
            {toName}
          </button>
        </p>
      </header>

      <dl className="space-y-1.5 rounded-lg border border-border bg-surface-muted/60 px-3 py-2.5 text-xs">
        <Row label="关系类型" value={`${visual.name}（${relation.type}）`} />
        <Row label="起始章节" value={`第 ${relation.from_chapter} 章`} />
        <Row
          label="结束章节"
          value={relation.to_chapter === null ? "至今" : `第 ${relation.to_chapter} 章`}
        />
        <Row label="当前视图" value={visibility.relations.has(relationIndex) ? "已出现" : "尚未出现"} />
      </dl>

      <section className="space-y-1.5">
        <p className="text-[11px] font-medium tracking-wide text-muted-foreground">原文根据</p>
        {relation.quote ? (
          <blockquote className="rounded-lg border-l-2 border-accent/60 bg-surface-muted/50 px-3 py-2 text-xs leading-relaxed text-foreground/90">
            <Quote className="mb-1 size-3 text-muted-foreground" />
            {relation.quote}
          </blockquote>
        ) : (
          <p className="text-xs text-muted-foreground">数据里没有摘录原文。</p>
        )}
      </section>
    </div>
  );
}

interface RelationRowProps {
  data: GraphDemoData;
  relation: GraphRelation;
  relationIndex: number;
  locked: boolean;
  selfId: string;
  onSelectCharacter: (id: string) => void;
}

function RelationRow({ data, relation, relationIndex, locked, selfId, onSelectCharacter }: RelationRowProps) {
  const visual = RELATION_VISUAL[relation.type];
  const otherId = relation.from === selfId ? relation.to : relation.from;
  const otherName = nameOf(data, otherId);
  const isOutgoing = relation.from === selfId;
  const chapterText =
    relation.to_chapter === null
      ? `第 ${relation.from_chapter} 章 · 至今`
      : `第 ${relation.from_chapter}–${relation.to_chapter} 章`;

  return (
    <button
      type="button"
      onClick={() => onSelectCharacter(otherId)}
      className={cn(
        "w-full space-y-1 rounded-lg border border-border bg-surface px-2.5 py-2 text-left transition-colors hover:border-accent/50 hover:bg-surface-muted/70",
        locked && "opacity-55",
      )}
    >
      <div className="flex items-center gap-1.5 text-xs">
        <span
          className="h-2.5 w-5 shrink-0 rounded-full"
          style={{ backgroundColor: visual.color }}
          aria-hidden
        />
        <span className="truncate font-medium text-foreground">
          {isOutgoing ? "→ " : "← "}
          {otherName}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">{chapterText}</span>
      </div>
      <div className="flex items-center gap-1.5 pl-6.5">
        <Badge variant="outline" size="sm">
          {relation.label ?? visual.name}
        </Badge>
        {locked ? (
          <span className="text-[10px] text-muted-foreground">第 {relation.from_chapter} 章后解锁</span>
        ) : (
          <span className="truncate text-[10px] text-muted-foreground">#{relationIndex}</span>
        )}
      </div>
      {relation.quote ? (
        <p className="line-clamp-2 pl-6.5 text-[11px] leading-relaxed text-muted-foreground">
          「{relation.quote}」
        </p>
      ) : null}
    </button>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="truncate text-right text-foreground">{value}</dd>
    </div>
  );
}

function nameOf(data: GraphDemoData, id: string): string {
  return data.characters.find((character) => character.id === id)?.name ?? id;
}
