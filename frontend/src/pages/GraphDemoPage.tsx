/**
 * 关系图谱 demo —— /graph-demo
 *
 * 这是给产品/作者看效果的独立整页(壳外, 不需要登录), 数据来自静态文件
 * `/graph-demo-data.json`; 文件缺失或校验不过时用文件内自带的兜底数据, 保证一打开就有东西看.
 *
 * 三个视图:
 *   家族树  klay 分层 + 按 generation 归行, 看血缘世系
 *   势力图  compound 分组, 看门派归属(成员网格按标签高度定距)
 *   全景    fcose 力导向, 看关系极性与冲突
 *
 * 版式: 画布吃满全宽, 图例是右下角悬浮折叠卡, 信息面板选中才滑入 —— 固定左右栏会把画布挤成
 * 912px 宽, fit 之后整张图缩到 60%, 什么都看不清.
 *
 * 时间滑块是核心卖点: 拖到第 N 章, 只看那一刻已经出现且尚未结束的人与事.
 */
import cytoscape from "cytoscape";
import fcose from "cytoscape-fcose";
import klay from "cytoscape-klay";
import { CircleAlert, Download, Focus, Search } from "lucide-react";
import * as React from "react";

import { GraphInfoPanel, type GraphSelection } from "@/components/graph-demo/GraphInfoPanel";
import { GraphLegend } from "@/components/graph-demo/GraphLegend";
import {
  LAYOUT_OPTIONS,
  applyModeFilter,
  kinFamilies,
  applyVisibility,
  buildGraphElements,
  type LayoutMode,
} from "@/components/graph-demo/graphElements";
import { labelFontScale, syncEdgeLabels, syncLabels } from "@/components/graph-demo/graphLabels";
import { fitToContent, runLayout, visibleElements, type Occluder } from "@/components/graph-demo/graphLayouts";
import { buildGraphStylesheet, detectGraphTheme, makeFactionColorMap } from "@/components/graph-demo/graphTheme";
import { computeVisibility } from "@/components/graph-demo/graphVisibility";
import { Button, Input, Slider, toast } from "@/components/ui";
import { cn } from "@/components/ui/cn";
import { parseGraphDemoData, type GraphDemoData } from "@/types/graphDemo";

/** 全景的「最少关系数」档位(默认 4: 冻结数据里 51/138 人, 正好能看清结构) */
const DEGREE_OPTIONS: { value: number; label: string }[] = [
  { value: 3, label: "3" },
  { value: 4, label: "4" },
  { value: 6, label: "6" },
  { value: 0, label: "全部" },
];

// 布局扩展只需注册一次
cytoscape.use(klay);
cytoscape.use(fcose);

/** 小于这个缩放比例隐藏边标签(否则缩成一片糊) */
const EDGE_LABEL_ZOOM = 1.05;
/** 每种关系类型最多画几个边标签(重复的「亲缘」四个不如一个) */
const EDGE_LABELS_PER_TYPE = 2;
/** 聚焦时最多放大到多少, 免得两三个人的小圈子把镜头怼到脸上 */
const FOCUS_MAX_ZOOM = 1.9;
/** 聚焦取景的留白(px) */
const FOCUS_PADDING = 90;
/** 桌面端信息面板宽度: 聚焦时把镜头往左让一点, 否则选中的节点会被面板压住 */
const PANEL_OFFSET_X = 160;

/** 静态 JSON 还没生成时的兜底数据: 3 势力 / 8 人物 / 11 条关系 */
const FALLBACK_RAW: unknown = {
  "meta": {
    "book": "九霄剑录",
    "totalChapters": 998,
    "sampledChapters": 60,
    "generatedAt": "2026-01-01T00:00:00.000Z",
    "schema": "graph-demo-v1",
  },
  "factions": [
    {
      "id": "f1",
      "name": "云岚宗",
      "kind": "门派",
      "leader": "c2",
      "members": ["c1", "c2", "c3", "c4"],
    },
    {
      "id": "f2",
      "name": "南宫世家",
      "kind": "家族",
      "leader": "c5",
      "members": ["c5", "c6"],
    },
    {
      "id": "f3",
      "name": "天机阁",
      "kind": "组织",
      "leader": "c8",
      "members": ["c7", "c8"],
    },
  ],
  "characters": [
    {
      "id": "c1",
      "name": "沈青梧",
      "aliases": ["青梧剑主"],
      "gender": "男",
      "role": "主角",
      "faction": "f1",
      "generation": 2,
      "firstSeen": 1,
      "lastSeen": null,
      "conflict": false,
    },
    {
      "id": "c2",
      "name": "云岚真人",
      "aliases": ["云岚"],
      "gender": "男",
      "role": "宗主",
      "faction": "f1",
      "generation": 1,
      "firstSeen": 2,
      "lastSeen": null,
      "conflict": false,
    },
    {
      "id": "c3",
      "name": "苏挽月",
      "aliases": ["月姬"],
      "gender": "女",
      "role": "同门",
      "faction": "f1",
      "generation": 2,
      "firstSeen": 3,
      "lastSeen": null,
      "conflict": false,
    },
    {
      "id": "c4",
      "name": "陆知白",
      "aliases": [],
      "gender": "男",
      "role": "师兄",
      "faction": "f1",
      "generation": 2,
      "firstSeen": 6,
      "lastSeen": 480,
      "conflict": false,
    },
    {
      "id": "c5",
      "name": "南宫烈",
      "aliases": ["烈王"],
      "gender": "男",
      "role": "家主",
      "faction": "f2",
      "generation": 1,
      "firstSeen": 1,
      "lastSeen": null,
      "conflict": true,
    },
    {
      "id": "c6",
      "name": "南宫烬",
      "aliases": [],
      "gender": "男",
      "role": "少主",
      "faction": "f2",
      "generation": 2,
      "firstSeen": 12,
      "lastSeen": null,
      "conflict": true,
    },
    {
      "id": "c7",
      "name": "白无常",
      "aliases": ["摘星客"],
      "gender": "男",
      "role": "神秘客",
      "faction": "f3",
      "generation": 3,
      "firstSeen": 60,
      "lastSeen": null,
      "conflict": false,
    },
    {
      "id": "c8",
      "name": "无咎先生",
      "aliases": [],
      "gender": "男",
      "role": "阁主",
      "faction": "f3",
      "generation": 1,
      "firstSeen": 8,
      "lastSeen": null,
      "conflict": false,
    },
  ],
  "memberships": [
    {
      "character": "c1",
      "faction": "f1",
      "from": 1,
      "to": null,
    },
    {
      "character": "c2",
      "faction": "f1",
      "from": 2,
      "to": null,
    },
    {
      "character": "c3",
      "faction": "f1",
      "from": 3,
      "to": null,
    },
    {
      "character": "c4",
      "faction": "f1",
      "from": 6,
      "to": 480,
    },
    {
      "character": "c5",
      "faction": "f2",
      "from": 1,
      "to": null,
    },
    {
      "character": "c6",
      "faction": "f2",
      "from": 12,
      "to": null,
    },
    {
      "character": "c7",
      "faction": "f3",
      "from": 60,
      "to": null,
    },
    {
      "character": "c8",
      "faction": "f3",
      "from": 8,
      "to": null,
    },
  ],
  "relations": [
    {
      "from": "c2",
      "to": "c1",
      "type": "master",
      "label": "师徒",
      "from_chapter": 3,
      "to_chapter": null,
      "quote": "「此子心骨清奇，堪承我云岚剑意。」",
    },
    {
      "from": "c2",
      "to": "c4",
      "type": "master",
      "label": "师徒",
      "from_chapter": 6,
      "to_chapter": null,
      "quote": "「知白性稳，先随我守山三年。」",
    },
    {
      "from": "c1",
      "to": "c3",
      "type": "spouse",
      "label": "道侣",
      "from_chapter": 320,
      "to_chapter": null,
      "quote": "「霜雪三千，我只要你一人。」",
    },
    {
      "from": "c3",
      "to": "c4",
      "type": "sibling",
      "label": "师兄妹",
      "from_chapter": 10,
      "to_chapter": null,
      "quote": "「叫一声师兄，我替你挡三剑。」",
    },
    {
      "from": "c5",
      "to": "c6",
      "type": "parent",
      "label": "父子",
      "from_chapter": 12,
      "to_chapter": null,
      "quote": "「南宫家的刀，只传给姓南宫的。」",
    },
    {
      "from": "c5",
      "to": "c1",
      "type": "enemy",
      "label": "宿敌",
      "from_chapter": 30,
      "to_chapter": null,
      "quote": "「沈家的小子，你活不过今夜。」",
    },
    {
      "from": "c6",
      "to": "c1",
      "type": "enemy",
      "label": "争锋",
      "from_chapter": 45,
      "to_chapter": null,
      "quote": "「剑在你手里，可惜钝了。」",
    },
    {
      "from": "c1",
      "to": "c4",
      "type": "ally",
      "label": "同门",
      "from_chapter": 15,
      "to_chapter": null,
      "quote": "「你我同门，退路留给别人。」",
    },
    {
      "from": "c2",
      "to": "c5",
      "type": "ally",
      "label": "旧盟",
      "from_chapter": 1,
      "to_chapter": 40,
      "quote": "「云岚与南宫，二十年前便已有约。」",
    },
    {
      "from": "c8",
      "to": "c7",
      "type": "master",
      "label": "师徒",
      "from_chapter": 60,
      "to_chapter": null,
      "quote": "「摘星一脉，只收不认命的人。」",
    },
    {
      "from": "c1",
      "to": "c7",
      "type": "other",
      "label": "未明",
      "from_chapter": 60,
      "to_chapter": null,
      "quote": "「他提剑拦我，却一句话也没说。」",
    },
  ],
  "stats": {
    "characters": 8,
    "relations": 11,
    "factions": 3,
  },
};

/** 兜底数据也要过一遍 schema, 免得手写数组里漏字段时页面炸掉 */
function makeFallbackData(): GraphDemoData {
  const parsed = parseGraphDemoData(FALLBACK_RAW);
  if (parsed.ok) return parsed.data;
  return {
    meta: { book: "演示数据", totalChapters: 100, sampledChapters: 0, generatedAt: "", schema: "graph-demo-v1" },
    characters: [],
    factions: [],
    memberships: [],
    relations: [],
  };
}

const FALLBACK_DATA = makeFallbackData();

/** 家族树剩这么点人时, 名字全开(去重叠只处理真正压住的那几个) */
const TREE_ALL_LABEL_LIMIT = 40;
/** 势力图: 单个势力框内最多摆几个名字(宋廷 83 人, 再多整框都是字) */
const FACTION_LABELS_PER_BOX = 12;

/** 当前真正画在画布上的人物数(排除时间过滤与视图过滤) */
function visibleCharacterCount(cy: cytoscape.Core): number {
  return cy
    .nodes('[type="character"]')
    .filter((node) => !node.hasClass("gd-hidden") && !node.hasClass("gd-mode-hidden")).length;
}

/**
 * 每个视图一屏最多摆几个标签. 这是兜底上限, 真正的数量由「度数优先 + 去重叠」算出来:
 * 空间够就多放, 放不下就少放, 所以这里给得比过去宽松得多(过去 fit 视图一律 8~12 个).
 */
function labelBudgetFor(mode: LayoutMode, visibleCharacters: number): number {
  if (mode === "faction") return 60;
  // 家族树: 只看一家时人少, 名字全开; 「全部」83 人时给 40 个(去重叠后实际少一些)
  if (mode === "tree") return visibleCharacters <= TREE_ALL_LABEL_LIMIT ? 999 : 40;
  return 24;
}

/** 布局 fit 之后同步一次标签(不重建图时用: 拖时间滑块/适配视图) */
function syncLabelsNow(cy: cytoscape.Core, selection: GraphSelection | null, mode: LayoutMode): void {
  let focusNodes: cytoscape.NodeCollection | null = null;
  if (selection?.kind === "node") {
    const element = cy.getElementById(selection.id);
    if (!element.empty()) focusNodes = (element as cytoscape.NodeSingular).closedNeighborhood().nodes();
  }
  syncLabels(cy, {
    focusNodes,
    labelBudget: labelBudgetFor(mode, visibleCharacterCount(cy)),
    perGroupBudget: mode === "faction" ? FACTION_LABELS_PER_BOX : 999,
  });
  syncEdgeLabelNow(cy);
}

/** 边标签: 每种关系最多 2 个、压在人物节点上的不画(见 graphLabels.syncEdgeLabels) */
function syncEdgeLabelNow(cy: cytoscape.Core): void {
  syncEdgeLabels(cy, { perTypeBudget: EDGE_LABELS_PER_TYPE, minZoom: EDGE_LABEL_ZOOM });
}

/**
 * 排布 + 上标签, 两遍:
 *   第一遍 fit 只量到节点, 标签是之后才开的 —— 最外圈的名字(移动端「完颜吴乞买」)会被画布切掉;
 *   所以上完标签再 fit 一次, 这一遍的包围盒已经把字算进去了.
 */
function layoutAndLabel(cy: cytoscape.Core, mode: LayoutMode): number {
  let zoom = runLayout(cy, mode, visibleElements(cy));
  syncLabelsNow(cy, null, mode);
  zoom = fitToContent(cy, visibleElements(cy));
  syncLabelsNow(cy, null, mode);
  return zoom;
}

/** 建图 effect 对外暴露的操作(工具栏/搜索/面板都走同一套聚焦逻辑) */
interface GraphApi {
  /** 选中 + 聚焦一个元素(null = 取消高亮与面板) */
  select: (next: GraphSelection | null, options?: { animate?: boolean }) => void;
  /** 重放当前选中态(换布局/换数据后) */
  reapply: () => void;
}

export default function GraphDemoPage() {
  const containerRef = React.useRef<HTMLDivElement | null>(null);
  // 左上角标题卡是浮层: 量给 fit 用, 免得内容铺到卡片底下
  const headerRef = React.useRef<HTMLDivElement | null>(null);
  // 顶部整条浮层(标题卡 + 视图按钮 + 搜索/导出)和左下角图例都会盖住画布
  const topBarRef = React.useRef<HTMLDivElement | null>(null);
  const toolbarRef = React.useRef<HTMLDivElement | null>(null);
  const legendAreaRef = React.useRef<HTMLDivElement | null>(null);
  const measureOccludersRef = React.useRef<(() => void) | null>(null);
  const cyRef = React.useRef<cytoscape.Core | null>(null);
  const apiRef = React.useRef<GraphApi | null>(null);

  const [data, setData] = React.useState<GraphDemoData>(FALLBACK_DATA);
  const [loadNote, setLoadNote] = React.useState<string | null>("正在读取 /graph-demo-data.json…");
  const [mode, setMode] = React.useState<LayoutMode>("tree");
  // 全景默认只铺「关系数 ≥4」的人(冻结数据里 51/138): 138 个点全上一次必然糊成毛球
  const [minDegree, setMinDegree] = React.useState(4);
  // 势力图默认只画跨势力关系(内部关系 200+ 条)
  const [showIntraFaction, setShowIntraFaction] = React.useState(false);
  // 家族树: 一次只看一家(null = 全部). 全画时 83 人散成 29 条小链, fit 后只剩 57% 缩放
  const [treeFamily, setTreeFamily] = React.useState<string | null>(null);
  const [chapter, setChapter] = React.useState<number>(FALLBACK_DATA.meta.totalChapters);
  const [selection, setSelection] = React.useState<GraphSelection | null>(null);
  // 图例默认收起: 常开会把画布挤窄(fit 按容器算缩放, 一挤整张图就缩到 60%)
  const [legendOpen, setLegendOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const [zoomPercent, setZoomPercent] = React.useState(100);

  // 事件回调里要读到最新值, 用 ref 兜住(避免重建整张图)
  const chapterRef = React.useRef(chapter);
  // 非 effect 里的回调(适配视图/导出/搜索)要用到当前视图, 用 ref 避免闭包拿到旧值
  const modeRef = React.useRef<LayoutMode>(mode);
  const selectionRef = React.useRef<GraphSelection | null>(selection);
  /** 最近一次布局 fit 得到的缩放: 标签分级的「一级」基准 */
  const fitZoomRef = React.useRef(1);
  const zoomPercentRef = React.useRef(100);
  /** 视图过滤条件的最新值(章节提交时要重算家族树的「还连着的人」) */
  const filterRef = React.useRef<{ minDegree: number; showIntraFaction: boolean; treeFamilyIds: Set<string> | null }>({
    minDegree: 4,
    showIntraFaction: false,
    treeFamilyIds: null,
  });

  React.useEffect(() => {
    selectionRef.current = selection;
  }, [selection]);


  const factionColors = React.useMemo(
    () => makeFactionColorMap(data.factions.map((faction) => faction.id)),
    [data],
  );
  const visibility = React.useMemo(() => computeVisibility(data, chapter), [data, chapter]);
  // 家族树视图只画亲缘子图, 顶部要说明「其余 N 人去哪了」
  const degreeMap = React.useMemo(() => {
    const map = new Map<string, number>();
    for (const relation of data.relations) {
      if (relation.from === relation.to) continue;
      map.set(relation.from, (map.get(relation.from) ?? 0) + 1);
      map.set(relation.to, (map.get(relation.to) ?? 0) + 1);
    }
    return map;
  }, [data]);

  // 亲缘连通分量清单(家族选择器用): 按人数排序, 名字取分量里关系数最多的人
  // 这一章里「身边还连着亲缘边」的人: 关系有章节区间, 吴近 → 吴夫人 这种过期后就不算一家人了
  const kinLinkedNow = React.useMemo(() => {
    const kinTypes = new Set(["parent", "spouse", "sibling"]);
    const linked = new Set<string>();
    for (const index of visibility.relations) {
      const relation = data.relations[index];
      if (!relation || !kinTypes.has(relation.type)) continue;
      linked.add(relation.from);
      linked.add(relation.to);
    }
    return linked;
  }, [data, visibility]);
  const families = React.useMemo(() => {
    return kinFamilies(data)
      .map((family) => ({
        ...family,
        size: family.memberIds.filter((id) => visibility.characters.has(id) && kinLinkedNow.has(id)).length,
      }))
      .filter((family) => family.size > 1)
      .sort((a, b) => b.size - a.size || a.name.localeCompare(b.name));
  }, [data, visibility, kinLinkedNow]);
  const treeFamilyIds = React.useMemo(() => {
    if (!treeFamily) return null;
    const hit = families.find((family) => family.id === treeFamily);
    return hit ? new Set(hit.memberIds) : null;
  }, [families, treeFamily]);
  // 章节提交时要重放视图过滤, 用 ref 兜住最新条件
  React.useEffect(() => {
    filterRef.current = { minDegree, showIntraFaction, treeFamilyIds };
  }, [minDegree, showIntraFaction, treeFamilyIds]);

  const viewStats = React.useMemo(() => {
    const chars = [...visibility.characters];
    const shown =
      mode === "panorama"
        ? chars.filter((id) => (degreeMap.get(id) ?? 0) >= minDegree).length
        : mode === "tree" && treeFamilyIds
          ? chars.filter((id) => treeFamilyIds.has(id) && kinLinkedNow.has(id)).length
          : chars.length;
    const factionOf = new Map(data.characters.map((character) => [character.id, character.faction ?? ""]));
    let crossRelations = 0;
    for (const index of visibility.relations) {
      const relation = data.relations[index];
      if (!relation) continue;
      const from = factionOf.get(relation.from) ?? "";
      const to = factionOf.get(relation.to) ?? "";
      if (from === to) continue;
      crossRelations += 1;
    }
    return { shown, total: chars.length, crossRelations };
  }, [data, degreeMap, kinLinkedNow, mode, minDegree, visibility, treeFamilyIds]);

  const kindredStats = React.useMemo(() => {
    // 只数「这一章还连着亲缘边」的人: 与树里真正画出来的点一一对应
    const visibleKin = [...kinLinkedNow].filter((id) => visibility.characters.has(id)).length;
    return { kin: visibleKin, others: visibility.characters.size - visibleKin };
  }, [kinLinkedNow, visibility]);
  const totalChapters = Math.max(1, data.meta.totalChapters);

  // ---- 读静态 JSON, 失败就留在兜底数据上 ----
  React.useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const response = await fetch("/graph-demo-data.json", { cache: "no-cache" });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const raw: unknown = await response.json();
        const parsed = parseGraphDemoData(raw);
        if (!parsed.ok) throw new Error(parsed.message);
        if (cancelled) return;
        setData(parsed.data);
        const latest = Math.max(1, parsed.data.meta.totalChapters);
        chapterRef.current = latest;
        setChapter(latest);
        setLoadNote(null);
      } catch (error) {
        if (cancelled) return;
        const reason = error instanceof Error ? error.message : String(error);
        setLoadNote(`未能加载 /graph-demo-data.json（${reason}），当前展示内置演示数据`);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, []);

  // ---- 建图: 换数据或换布局都重建一次(compound 结构不能原地改) ----
  React.useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let fontScale = 1;
    const cy = cytoscape({
      container,
      elements: buildGraphElements(data, mode),
      style: buildGraphStylesheet(detectGraphTheme(), { labelScale: fontScale }),
      layout: { name: "preset" },
      textureOnViewport: true,
      wheelSensitivity: 0.2,
      minZoom: 0.15,
      maxZoom: 3,
      boxSelectionEnabled: false,
      autounselectify: true,
      pixelRatio: "auto",
    });
    cyRef.current = cy;
    modeRef.current = mode;

    /** 当前需要强制显示标签的节点(hover 优先于选中) */
    let hoveredNode: cytoscape.NodeSingular | null = null;

    const refreshLabels = () => {
      let focusNodes: cytoscape.NodeCollection | null = null;
      if (hoveredNode && !hoveredNode.empty()) {
        focusNodes = hoveredNode.closedNeighborhood().nodes();
      } else if (selectionRef.current?.kind === "node") {
        const element = cy.getElementById(selectionRef.current.id);
        if (!element.empty()) focusNodes = (element as cytoscape.NodeSingular).closedNeighborhood().nodes();
      }
      const currentMode = modeRef.current;
      syncLabels(cy, {
        focusNodes,
        labelBudget: labelBudgetFor(currentMode, visibleCharacterCount(cy)),
        perGroupBudget: currentMode === "faction" ? FACTION_LABELS_PER_BOX : 999,
      });
    };

    const syncEdgeLabelsHere = () => {
      syncEdgeLabelNow(cy);
    };

    /** 缩小后模型字号放大, 屏幕上字高一直接近 11px; 只在换算结果变化时重建样式表 */
    const applyFontScale = () => {
      const next = labelFontScale(cy);
      if (Math.abs(next - fontScale) < 0.05) return;
      fontScale = next;
      cy.style(buildGraphStylesheet(detectGraphTheme(), { labelScale: fontScale }));
    };

    /** 推镜头: 桌面端信息面板会盖住右侧 300px, 取景中心往左让一点 */
    const animateCamera = (zoom: number, point: { x: number; y: number }, duration: number) => {
      const panelOpen = window.innerWidth >= 768;
      const offsetX = panelOpen ? PANEL_OFFSET_X : 0;
      const nextZoom = Math.min(Math.max(zoom, minZoomSafe()), cy.maxZoom());
      const screenX = cy.width() / 2 - offsetX / 2;
      const screenY = cy.height() / 2;
      cy.animate({
        zoom: nextZoom,
        pan: { x: screenX - point.x * nextZoom, y: screenY - point.y * nextZoom },
        duration,
      });
    };

    const minZoomSafe = () => Math.max(cy.minZoom(), 0.2);

    const dimOthers = (keep: cytoscape.CollectionReturnValue) => {
      cy.batch(() => {
        cy.elements().addClass("gd-dim");
        keep.removeClass("gd-dim");
        // 势力框只淡不隐: 框线全没了就看不出归属
        cy.nodes()
          .filter((node) => node.data("type") === "faction")
          .removeClass("gd-dim");
      });
    };

    const resetHighlight = () => {
      cy.batch(() => {
        cy.elements().removeClass("gd-dim gd-focus gd-selected");
      });
    };

    /** 点节点: 邻域高亮 + 面板滑入 + 镜头推近 */
    const focusNode = (node: cytoscape.NodeSingular, animate = true) => {
      const hood = node.closedNeighborhood();
      dimOthers(hood);
      node.addClass("gd-focus");
      cy.batch(() => {
        node.addClass("gd-selected");
      });
      refreshLabels();
      if (!animate) return;
      const box = hood.boundingBox();
      const fitZoom = Math.min(
        (cy.width() - FOCUS_PADDING * 2) / Math.max(box.w, 1),
        (cy.height() - FOCUS_PADDING * 2) / Math.max(box.h, 1),
      );
      const zoom = Math.min(fitZoom * 1.06, FOCUS_MAX_ZOOM);
      // 圈子小就框住整个圈子, 圈子大(赵玖那种 40 个邻居)就直接把本人放中间
      const point =
        hood.nodes().length <= 10
          ? { x: (box.x1 + box.x2) / 2, y: (box.y1 + box.y2) / 2 }
          : node.position();
      animateCamera(zoom, point, 320);
    };

    const focusEdge = (edge: cytoscape.EdgeSingular) => {
      const keep = edge.closedNeighborhood();
      dimOthers(keep);
      cy.batch(() => {
        edge.addClass("gd-selected");
      });
      refreshLabels();
      const box = keep.boundingBox();
      const fitZoom = Math.min(
        (cy.width() - FOCUS_PADDING * 2) / Math.max(box.w, 1),
        (cy.height() - FOCUS_PADDING * 2) / Math.max(box.h, 1),
      );
      animateCamera(Math.min(fitZoom * 0.95, FOCUS_MAX_ZOOM), { x: (box.x1 + box.x2) / 2, y: (box.y1 + box.y2) / 2 }, 300);
    };

    const select = (next: GraphSelection | null, options: { animate?: boolean } = {}) => {
      const animate = options.animate ?? true;
      if (!next) {
        resetHighlight();
      } else {
        const element = cy.getElementById(next.id);
        if (element.empty()) {
          resetHighlight();
        } else if (next.kind === "node") {
          focusNode(element as cytoscape.NodeSingular, animate);
        } else {
          focusEdge(element as cytoscape.EdgeSingular);
        }
      }
      setSelection(next);
      refreshLabels();
    };

    apiRef.current = {
      select,
      reapply: () => {
        const current = selectionRef.current;
        if (!current) return;
        select(current, { animate: false });
      },
    };

    cy.on("tap", "node", (event) => {
      const node = event.target as cytoscape.NodeSingular;
      // 势力框不是人物, 点框不改选中(免得面板显示一个不存在的档案)
      if (node.data("type") === "faction") return;
      select({ kind: "node", id: node.id() });
    });

    cy.on("tap", "edge", (event) => {
      select({ kind: "edge", id: event.target.id() });
    });

    cy.on("tap", (event) => {
      if (event.target !== cy) return;
      select(null);
    });

    // hover: 高亮邻边(不改选中, 不动面板)
    cy.on("mouseover", "node", (event) => {
      const node = event.target as cytoscape.NodeSingular;
      if (node.data("type") === "faction") return;
      hoveredNode = node;
      node.addClass("gd-hover");
      node.connectedEdges().addClass("gd-hover");
      refreshLabels();
    });

    cy.on("mouseout", "node", (event) => {
      const node = event.target as cytoscape.NodeSingular;
      if (hoveredNode?.id() !== node.id()) return;
      hoveredNode = null;
      node.removeClass("gd-hover");
      node.connectedEdges().removeClass("gd-hover");
      refreshLabels();
    });

    cy.on("zoom", () => {
      applyFontScale();
      syncEdgeLabelsHere();
      refreshLabels();
      const percent = Math.round(cy.zoom() * 100);
      if (Math.abs(percent - zoomPercentRef.current) >= 3) {
        zoomPercentRef.current = percent;
        setZoomPercent(percent);
      }
    });

    // 主题切换(明/暗/护眼)时重建样式表: canvas 读不到 CSS 变量
    const themeObserver = new MutationObserver(() => {
      cy.style(buildGraphStylesheet(detectGraphTheme(), { labelScale: fontScale }));
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme"] });

    // 首帧容器可能还没有尺寸, 有尺寸后再排一次版
    let laidOut = false;
    // 浮层遮挡区(画布坐标)交给 fit: 只平移不缩小
    const measureOccluders = () => {
      const canvasRect = container.getBoundingClientRect();
      const canvasW = canvasRect.width;
      const headerRect = headerRef.current?.getBoundingClientRect();
      const toolbarRect = toolbarRef.current?.getBoundingClientRect();
      // 顶部要让出多高: 左侧标题卡总是要让; 右侧工具栏只有「窄屏铺满一行」时才整条让开,
      // 桌面端它只占右上角, 中间那道空白没必要一起让(否则缩放白白掉一档)
      let topInset = 0;
      if (headerRect && headerRect.height > 0) topInset = Math.max(topInset, headerRect.bottom - canvasRect.top);
      if (toolbarRect && toolbarRect.width >= canvasW * 0.6) {
        topInset = Math.max(topInset, toolbarRect.bottom - canvasRect.top);
      }
      const rects: Occluder[] =
        topInset > 0 ? [{ x1: 0, y1: 0, x2: canvasW, y2: topInset + 10 }] : [];
      cy.scratch("gd-occluders", rects);

      // 左下角图例是折叠状态的小卡片(约 36px), 画布底部本来就留了 fitPadding, 不再额外让位;
      // 整条让开会让打包区域从 1.9 变成 1.6 的长宽比, 缩放白掉一档(实测 0.55 → 0.43)
      cy.scratch("gd-safe-bottom", 0);
    };
    measureOccludersRef.current = measureOccluders;

    const applyInitialState = () => {
      laidOut = true;
      measureOccluders();
      // 顺序有讲究: 先按章节切显隐, 再按视图过滤 —— 家族树要知道哪些亲缘边「此刻还活着」
      applyVisibility(cy, computeVisibility(data, chapterRef.current));
      applyModeFilter(cy, { mode, minDegree, showIntraFaction, treeFamilyIds });
      // 家族树: 非血缘关系压暗, 让世系线自己浮出来
      cy.batch(() => {
        cy.edges().forEach((element) => {
          const type = String(element.data("type"));
          const isKin = type === "parent" || type === "spouse" || type === "sibling";
          element.toggleClass("gd-muted", mode === "tree" && !isKin);
        });
      });
      fitZoomRef.current = layoutAndLabel(cy, mode);
      applyFontScale();
      syncEdgeLabelsHere();
      apiRef.current?.reapply();
      refreshLabels();
    };

    const observer = new ResizeObserver(() => {
      cy.resize();
      if (!laidOut) {
        const rect = container.getBoundingClientRect();
        if (rect.width > 0 && rect.height > 0) applyInitialState();
      }
    });
    observer.observe(container);

    const rect = container.getBoundingClientRect();
    if (rect.width > 0 && rect.height > 0) applyInitialState();

    return () => {
      observer.disconnect();
      themeObserver.disconnect();
      apiRef.current = null;
      measureOccludersRef.current = null;
      cy.destroy();
      cyRef.current = null;
    };
  }, [data, mode, minDegree, showIntraFaction, treeFamilyIds]);

  // ---- 时间维度: 只切 class, 不重建图 ----
  React.useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    applyVisibility(cy, visibility);
    applyModeFilter(cy, { mode, minDegree, showIntraFaction, treeFamilyIds });
  }, [visibility, mode, minDegree, showIntraFaction, treeFamilyIds]);

  // Esc: 取消高亮 + 收起面板
  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      apiRef.current?.select(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleChapterPreview = (next: number) => {
    const clamped = Math.min(totalChapters, Math.max(1, next));
    chapterRef.current = clamped;
    setChapter(clamped);
  };

  /** 松手才重排: 拖动过程中只切显隐, 保证滑块跟手 */
  const handleChapterCommit = (next: number) => {
    const cy = cyRef.current;
    if (!cy) return;
    const clamped = Math.min(totalChapters, Math.max(1, next));
    chapterRef.current = clamped;
    applyVisibility(cy, computeVisibility(data, clamped));
    applyModeFilter(cy, { mode: modeRef.current, ...filterRef.current });
    setSelection(null);
    fitZoomRef.current = layoutAndLabel(cy, modeRef.current);
  };

  const handleSearch = () => {
    const term = query.trim();
    if (!term) {
      apiRef.current?.select(null);
      return;
    }
    const hit = data.characters.find(
      (character) =>
        character.name.includes(term) || character.aliases.some((alias) => alias.includes(term)),
    );
    if (!hit) {
      toast.error(`没找到「${term}」`);
      return;
    }
    if (!visibility.characters.has(hit.id)) {
      toast.info(`「${hit.name}」到第 ${hit.firstSeen} 章才出场，当前视图停在第 ${chapter} 章`);
      return;
    }
    // 搜索命中和点节点走同一套: 高亮邻域 + 面板 + 居中
    apiRef.current?.select({ kind: "node", id: hit.id });
  };

  const handleResetView = () => {
    const cy = cyRef.current;
    if (!cy) return;
    apiRef.current?.select(null);
    measureOccludersRef.current?.();
    fitZoomRef.current = layoutAndLabel(cy, modeRef.current);
  };

  const handleExport = () => {
    const cy = cyRef.current;
    if (!cy) return;
    const uri = cy.png({ full: true, scale: 2, bg: detectGraphTheme().canvasBg });
    const link = document.createElement("a");
    link.href = uri;
    link.download = `关系图谱-${data.meta.book}-第${chapter}章.png`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    toast.success("已导出 PNG");
  };

  /** 面板里点对方姓名: 选中 + 居中(和点节点同一套) */
  const handleSelectCharacter = (id: string) => {
    const cy = cyRef.current;
    if (!cy) return;
    const node = cy.getElementById(id);
    if (node.empty()) return;
    if (node.hasClass("gd-hidden")) {
      toast.info("这个角色在当前章节视图里还看不到，拖动时间滑块试试");
      return;
    }
    apiRef.current?.select({ kind: "node", id });
  };

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-background text-foreground">
      {/* 画布: 桌面端不再有左右固定栏, 图和信息面板都是浮层, 画布吃满全宽 */}
      <div className="relative min-h-0 flex-1">
        {/* 注意: 这里不能用 absolute inset-0 —— cytoscape 初始化时会把容器改成 position: relative,
            绝对定位一旦失效高度就塌成 0, 画布什么都不画. 用 h-full w-full 让高度由父容器撑开. */}
        <div ref={containerRef} className="h-full w-full" />

        {/* 顶部: 标题/状态 + 布局/搜索/导出 */}
        <div
          ref={topBarRef}
          className="pointer-events-none absolute inset-x-0 top-0 z-20 flex flex-wrap items-start justify-between gap-2 p-2 sm:p-3"
        >
          <div
            ref={headerRef}
            className="pointer-events-auto flex min-w-0 max-w-full flex-col gap-2 sm:max-w-sm"
          >
            <div className="rounded-2xl border border-border bg-surface/92 px-3 py-1.5 shadow-sm backdrop-blur sm:py-2">
              <p className="text-[10px] font-medium tracking-[0.18em] text-accent uppercase">
                关系图谱 · DEMO
              </p>
              <h1 className="mt-0.5 truncate text-sm font-semibold text-foreground">
                {data.meta.book}
              </h1>
              <p className="mt-0.5 text-[11px] text-muted-foreground sm:mt-1">
                当前视图：第 {chapter} 章 · 可见 {visibility.characters.size} 人 /{" "}
                {visibility.relations.size} 条关系
              </p>
              {mode === "tree" ? (
                <p className="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">
                  {treeFamily ? (
                    <>
                      只看「{families.find((family) => family.id === treeFamily)?.name ?? "这一家"}」
                      {" "}
                      {viewStats.shown} 人；点「全部」回到整张家谱
                    </>
                  ) : (
                    <>
                      家族树只画有血缘/婚姻关系的人（{kindredStats.kin} 人，分成 {families.length} 家）；
                      其余 {Math.max(kindredStats.others, 0)} 人请切到「全景」或「势力图」
                    </>
                  )}
                </p>
              ) : null}
              {mode === "panorama" ? (
                <p className="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">
                  当前显示 {viewStats.shown} / {viewStats.total} 人（已按关系数 ≥{minDegree} 过滤）
                </p>
              ) : null}
              {mode === "faction" ? (
                <p className="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">
                  {showIntraFaction
                    ? "已显示势力内部关系（淡线）；关掉看得更清"
                    : `只画跨势力关系 ${viewStats.crossRelations} 条，内部关系已隐藏`}
                </p>
              ) : null}
            </div>

            {mode === "panorama" ? (
              <div className="pointer-events-auto flex flex-wrap items-center gap-1 rounded-xl border border-border bg-surface/92 p-1 text-[11px] shadow-sm backdrop-blur">
                <span className="px-1 text-muted-foreground">最少关系数</span>
                {DEGREE_OPTIONS.map((option) => (
                  <button
                    key={option.value}
                    type="button"
                    onClick={() => setMinDegree(option.value)}
                    className={cn(
                      "rounded-lg px-2 py-1 font-medium tabular-nums transition-colors",
                      minDegree === option.value
                        ? "bg-accent text-accent-foreground shadow-sm"
                        : "text-muted-foreground hover:bg-surface-muted hover:text-foreground",
                    )}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            ) : null}

            {mode === "tree" && families.length > 1 ? (
              <div className="pointer-events-auto flex max-w-[24rem] flex-wrap items-center gap-1 rounded-xl border border-border bg-surface/92 p-1 text-[11px] shadow-sm backdrop-blur">
                <span className="shrink-0 px-1 text-muted-foreground">家族</span>
                <button
                  type="button"
                  onClick={() => {
                    setSelection(null);
                    setTreeFamily(null);
                  }}
                  aria-pressed={treeFamily === null}
                  className={cn(
                    "rounded-lg px-2 py-1 font-medium tabular-nums transition-colors",
                    treeFamily === null
                      ? "bg-accent text-accent-foreground shadow-sm"
                      : "text-muted-foreground hover:bg-surface-muted hover:text-foreground",
                  )}
                >
                  全部 · {kindredStats.kin} 人
                </button>
                {families.slice(0, 8).map((family) => (
                  <button
                    key={family.id}
                    type="button"
                    onClick={() => {
                      setSelection(null);
                      setTreeFamily(family.id);
                    }}
                    aria-pressed={treeFamily === family.id}
                    className={cn(
                      "rounded-lg px-2 py-1 font-medium tabular-nums transition-colors",
                      treeFamily === family.id
                        ? "bg-accent text-accent-foreground shadow-sm"
                        : "text-muted-foreground hover:bg-surface-muted hover:text-foreground",
                    )}
                  >
                    {family.name} · {family.size} 人
                  </button>
                ))}
              </div>
            ) : null}

            {mode === "faction" ? (
              <button
                type="button"
                onClick={() => setShowIntraFaction((value) => !value)}
                aria-pressed={showIntraFaction}
                className={cn(
                  "pointer-events-auto self-start rounded-xl border border-border px-2.5 py-1.5 text-[11px] font-medium shadow-sm backdrop-blur transition-colors",
                  showIntraFaction
                    ? "bg-accent text-accent-foreground"
                    : "bg-surface/92 text-muted-foreground hover:bg-surface-muted hover:text-foreground",
                )}
              >
                {showIntraFaction ? "隐藏势力内部关系" : "显示势力内部关系"}
              </button>
            ) : null}

            {loadNote ? (
              <div className="flex items-start gap-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-[11px] leading-relaxed text-amber-700 dark:text-amber-300">
                <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
                <span>{loadNote}</span>
              </div>
            ) : null}
          </div>

          <div
            ref={toolbarRef}
            className="pointer-events-auto flex w-full flex-wrap items-center justify-end gap-1.5 sm:w-auto"
          >
            <div className="flex items-center gap-1 rounded-xl border border-border bg-surface/92 p-1 shadow-sm backdrop-blur">
              {LAYOUT_OPTIONS.map((option) => (
                <button
                  key={option.id}
                  type="button"
                  title={option.hint}
                  onClick={() => setMode(option.id)}
                  className={cn(
                    "rounded-lg px-2.5 py-1.5 text-xs font-medium transition-colors",
                    mode === option.id
                      ? "bg-accent text-accent-foreground shadow-sm"
                      : "text-muted-foreground hover:bg-surface-muted hover:text-foreground",
                  )}
                >
                  {option.name}
                </button>
              ))}
            </div>

            <Input
              size="sm"
              className="w-36 bg-surface/92 backdrop-blur sm:w-48"
              prefixIcon={<Search className="size-3.5 text-muted-foreground" />}
              placeholder="搜姓名 / 别名"
              value={query}
              clearable
              onClear={() => {
                setQuery("");
                apiRef.current?.select(null);
              }}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") handleSearch();
              }}
            />

            <Button
              size="sm"
              variant="secondary"
              onClick={handleResetView}
              aria-label="适配视图"
              title="适配视图"
              className="bg-surface/92 backdrop-blur"
            >
              <Focus />
              <span className="hidden sm:inline">适配视图</span>
            </Button>
            <Button
              size="sm"
              variant="primary"
              onClick={handleExport}
              aria-label="导出 PNG"
              title="导出 PNG"
            >
              <Download />
              <span className="hidden sm:inline">导出 PNG</span>
            </Button>
          </div>
        </div>

        {/* 图例: 右下角改成左下角悬浮折叠卡, 默认收起 */}
        <div
          ref={legendAreaRef}
          className="pointer-events-none absolute bottom-2 left-2 z-20 flex flex-col items-start sm:bottom-3 sm:left-3"
        >
          <GraphLegend
            factions={data.factions}
            factionColors={factionColors}
            open={legendOpen}
            onToggle={() => setLegendOpen((open) => !open)}
            activeFactions={visibility.factions}
          />
        </div>

        <div className="pointer-events-none absolute bottom-3 right-3 z-10 rounded-md border border-border/60 bg-surface/80 px-2 py-0.5 text-[10px] tabular-nums text-muted-foreground backdrop-blur">
          {zoomPercent}%
        </div>

        <GraphInfoPanel
          data={data}
          factionColors={factionColors}
          visibility={visibility}
          selection={selection}
          onClose={() => apiRef.current?.select(null)}
          onSelectCharacter={handleSelectCharacter}
        />
      </div>

      {/* 底部时间轴: 放在画布外面(不是浮层), 免得盖住最下面一行节点 */}
      <div className="border-t border-border bg-surface/92 backdrop-blur">
        <div className="mx-auto flex max-w-4xl flex-col gap-1 px-4 py-2 sm:py-2.5">
          <Slider
            label="读到第几章"
            value={[chapter]}
            min={1}
            max={totalChapters}
            step={1}
            formatValue={(value) => `第 ${value} 章`}
            onValueChange={(next) => handleChapterPreview(next[0] ?? chapter)}
            onValueCommit={(next) => handleChapterCommit(next[0] ?? chapter)}
          />
          <div className="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
            <span>第 1 章</span>
            <span className="hidden sm:inline">拖动看人物与关系随剧情生长/消散，松手后自动重排</span>
            <span>第 {totalChapters} 章</span>
          </div>
        </div>
      </div>
    </div>
  );
}
