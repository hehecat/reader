import * as React from "react";

import { Badge, Button } from "@/components/ui";

export interface GraphCharacter {
  name: string;
  alias?: string;
  firstChapter?: string;
  role?: string;
}

export interface GraphRelation {
  from: string;
  to: string;
  label?: string;
  chapter?: string;
}

export interface GraphData {
  characters: GraphCharacter[];
  relations: GraphRelation[];
}

interface Point {
  x: number;
  y: number;
}

const W = 440;
const H = 340;
const CX = W / 2;
const CY = H / 2;
const RADIUS = 118;
const NODE_R = 25;

function layout(count: number): Point[] {
  return Array.from({ length: count }, (_, i) => {
    const angle = (i / Math.max(1, count)) * Math.PI * 2 - Math.PI / 2;
    return { x: CX + Math.cos(angle) * RADIUS, y: CY + Math.sin(angle) * RADIUS };
  });
}

function truncate(text: string, max = 4): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/**
 * 人物关系图（无第三方图库）：
 * - 圆周布局，边为向圆心内凹的曲线，标签贴在曲线上；
 * - 点人物高亮相连关系，并在图下方展开该人物的身份/首次出场/关系清单（可跳章）。
 */
export function CharacterGraph({
  data,
  onJump,
}: {
  data: GraphData;
  onJump: (title: string) => void;
}) {
  const characters = React.useMemo(
    () => data.characters.filter((c) => (c.name ?? "").trim() !== "").slice(0, 15),
    [data.characters],
  );
  const positions = React.useMemo(() => layout(characters.length), [characters.length]);
  // 名字 → 下标（数据驱动的小查找表，用 Record）
  const indexOf = React.useMemo(() => {
    const map: Record<string, number> = {};
    characters.forEach((c, i) => {
      map[c.name] = i;
    });
    return map;
  }, [characters]);

  const [selected, setSelected] = React.useState<string | null>(null);
  const relations = React.useMemo(
    () =>
      (data.relations ?? []).filter(
        (r) => r.from in indexOf && r.to in indexOf && r.from !== r.to,
      ),
    [data.relations, indexOf],
  );

  const related = relations.filter((r) => selected !== null && (r.from === selected || r.to === selected));
  const active = characters.find((c) => c.name === selected) ?? null;

  return (
    <div className="flex flex-col gap-3">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        role="img"
        aria-label="人物关系图"
        className="w-full select-none"
      >
        {relations.map((rel, i) => {
          const a = positions[indexOf[rel.from] ?? 0] ?? { x: CX, y: CY };
          const b = positions[indexOf[rel.to] ?? 0] ?? { x: CX, y: CY };
          const mx = (a.x + b.x) / 2;
          const my = (a.y + b.y) / 2;
          // 控制点向圆心收 18%，曲线从人物旁绕过
          const cx = mx + (CX - mx) * 0.18;
          const cy = my + (CY - my) * 0.18;
          const lx = 0.25 * a.x + 0.5 * cx + 0.25 * b.x;
          const ly = 0.25 * a.y + 0.5 * cy + 0.25 * b.y;
          const highlighted =
            selected !== null && (rel.from === selected || rel.to === selected);
          const dimmed = selected !== null && !highlighted;
          return (
            <g key={`r-${i}`} className={dimmed ? "opacity-25" : undefined}>
              <path
                d={`M${a.x},${a.y} Q${cx},${cy} ${b.x},${b.y}`}
                fill="none"
                strokeWidth={highlighted ? 2 : 1.2}
                className={highlighted ? "stroke-accent" : "stroke-border"}
              />
              {rel.label !== undefined && rel.label !== "" ? (
                <text
                  x={lx}
                  y={ly}
                  textAnchor="middle"
                  dominantBaseline="middle"
                  className="fill-muted-foreground text-[9px]"
                  strokeWidth={3}
                  paintOrder="stroke"
                  stroke="currentColor"
                  style={{ stroke: "var(--color-surface, white)" }}
                >
                  {rel.label}
                </text>
              ) : null}
            </g>
          );
        })}

        {characters.map((c, i) => {
          const p = positions[i] ?? { x: CX, y: CY };
          const isSelected = selected === c.name;
          const isNeighbor =
            selected !== null &&
            relations.some(
              (r) =>
                (r.from === selected && r.to === c.name) ||
                (r.to === selected && r.from === c.name),
            );
          const dimmed = selected !== null && !isSelected && !isNeighbor;
          return (
            <g
              key={`c-${i}`}
              transform={`translate(${p.x},${p.y})`}
              className={`cursor-pointer outline-none ${dimmed ? "opacity-30" : ""}`}
              role="button"
              tabIndex={0}
              aria-label={`${c.name}${c.role ? `, ${c.role}` : ""}`}
              onClick={() => setSelected(isSelected ? null : c.name)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  setSelected(isSelected ? null : c.name);
                }
              }}
            >
              <circle
                r={NODE_R}
                strokeWidth={isSelected ? 2.2 : 1.2}
                className={
                  isSelected
                    ? "fill-surface stroke-accent"
                    : "fill-surface-muted stroke-border hover:stroke-accent/70"
                }
              />
              <text
                textAnchor="middle"
                dominantBaseline="central"
                className="pointer-events-none fill-foreground text-[11px] font-medium"
              >
                {truncate(c.name)}
              </text>
            </g>
          );
        })}
      </svg>

      {active === null ? (
        <p className="text-xs text-muted-foreground">
          点人物查看身份与关系；再点一次取消选中。
        </p>
      ) : (
        <div className="rounded-lg border border-border bg-surface-muted/50 p-2.5">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold">{active.name}</span>
            {active.alias !== undefined && active.alias !== "" ? (
              <span className="text-xs text-muted-foreground">别名: {active.alias}</span>
            ) : null}
            {active.firstChapter !== undefined && active.firstChapter !== "" ? (
              <button
                type="button"
                className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={() => onJump(active.firstChapter ?? "")}
                title="跳到首次出场章节"
              >
                <Badge size="sm" variant="muted">
                  首次出场: {active.firstChapter}
                </Badge>
              </button>
            ) : null}
          </div>
          {active.role !== undefined && active.role !== "" ? (
            <p className="mt-1 text-xs text-muted-foreground">{active.role}</p>
          ) : null}
          {related.length === 0 ? (
            <p className="mt-2 text-xs text-muted-foreground">片段里没有提到与其他人的关系。</p>
          ) : (
            <ul className="mt-2 flex flex-col gap-1">
              {related.map((r, i) => {
                const other = r.from === active.name ? r.to : r.from;
                const direction = r.from === active.name ? "→" : "←";
                return (
                  <li key={i} className="flex flex-wrap items-center gap-1.5 text-xs">
                    <span className="text-foreground">
                      {direction} {other}
                    </span>
                    {r.label !== undefined && r.label !== "" ? (
                      <span className="text-muted-foreground">({r.label})</span>
                    ) : null}
                    {r.chapter !== undefined && r.chapter !== "" ? (
                      <button
                        type="button"
                        onClick={() => onJump(r.chapter ?? "")}
                        className="text-accent underline-offset-2 hover:underline"
                      >
                        {r.chapter}
                      </button>
                    ) : null}
                  </li>
                );
              })}
            </ul>
          )}
          <div className="mt-2">
            <Button size="sm" variant="ghost" onClick={() => setSelected(null)}>
              收起
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
