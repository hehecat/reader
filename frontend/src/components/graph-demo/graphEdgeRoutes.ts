/**
 * 边绕行: 家族树里「同层横边」从第三个节点的圆点背后穿过时(赵玖 → 吴夫人 被潘贤妃挡住),
 * 给这条边换成带控制点的曲线绕开, 而不是让它直挺挺地压在别人身上.
 *
 * 代价控制(138 个节点不能每帧 O(n²)):
 *   1. 只在家族树视图、且当前缩放 ≥ 1 时算 —— 缩到 0.5 时整张图本就糊成一片, 绕不绕都看不出来;
 *   2. 只处理「同层/相邻层」的横边: 两端竖直距离超过一层半(家族树一层 ≈ 132)的直接跳过;
 *   3. 每条边只查弦中点 ±40px(屏幕像素)内的节点, 命中不到就立刻返回, 不做全局两两比较.
 * 命中时再沿法线两侧各探一次, 取节点更少的那一侧挂 .gd-detour-up / .gd-detour-down
 * (样式在 graphTheme: unbundled-bezier + 控制点偏移, 见 DETOUR_DISTANCE).
 */
import type cytoscape from "cytoscape";

/** 缩放门槛: 低于它就不绕(见文件头) */
export const DETOUR_MIN_ZOOM = 1;
/** 「同层/相邻层」的竖直容差(px): 家族树一层 132, 给到一层半 */
const LAYER_TOLERANCE = 176;
/** 中点附近多大范围(屏幕 px)内的节点算挡路 */
const PROBE_RADIUS_PX = 40;
/** 选边时探测的偏移量(模型 px); 与样式表里的控制点距离一半大致对应 */
const PROBE_OFFSET = 32;

interface NodeBox {
  id: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface RouteEdgesOptions {
  /** 当前视图: 只有家族树才绕行(势力图/全景是网格与力导向, 绕行只会添乱) */
  mode: string;
}

/** 点是否落在「节点包围盒 + 半径」里(方块近似, 比圆判定便宜, 宁可多绕不可漏绕) */
function nearPoint(box: NodeBox, point: { x: number; y: number }, radius: number): boolean {
  return (
    point.x >= box.x1 - radius &&
    point.x <= box.x2 + radius &&
    point.y >= box.y1 - radius &&
    point.y <= box.y2 + radius
  );
}

/**
 * 重算所有边的绕行 class, 返回被绕开的边数. 幂等: 条件不满足时会清掉 class.
 */
export function routeEdges(cy: cytoscape.Core, options: RouteEdgesOptions): number {
  const clear = () => cy.edges().removeClass("gd-detour-up gd-detour-down");
  if (options.mode !== "tree" || cy.zoom() < DETOUR_MIN_ZOOM) {
    clear();
    return 0;
  }

  const zoom = Math.max(cy.zoom(), 0.05);
  const radius = PROBE_RADIUS_PX / zoom;
  const boxes: NodeBox[] = [];
  cy.nodes()
    .filter(
      (node) => node.data("type") === "character" && !node.hasClass("gd-hidden") && !node.hasClass("gd-mode-hidden"),
    )
    .forEach((element) => {
      const box = element.boundingBox({ includeLabels: false, includeOverlays: false });
      boxes.push({ id: element.id(), x1: box.x1, y1: box.y1, x2: box.x2, y2: box.y2 });
    });

  const countNear = (point: { x: number; y: number }, skip: string): number => {
    let hits = 0;
    for (const box of boxes) {
      if (box.id === skip) continue;
      if (nearPoint(box, point, radius * 0.8)) hits += 1;
    }
    return hits;
  };

  let routed = 0;
  cy.batch(() => {
    cy.edges().forEach((edge) => {
      const drop = () => edge.removeClass("gd-detour-up gd-detour-down");
      if (edge.hasClass("gd-hidden") || edge.hasClass("gd-mode-hidden")) {
        drop();
        return;
      }
      const from = edge.source();
      const to = edge.target();
      if (from.data("type") !== "character" || to.data("type") !== "character") {
        drop();
        return;
      }
      const a = from.position();
      const b = to.position();
      const dy = b.y - a.y;
      // 竖边(父子)本来就从两行之间穿过, 不在这里处理
      if (Math.abs(dy) > LAYER_TOLERANCE) {
        drop();
        return;
      }
      const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
      const blocked = boxes.some(
        (box) => box.id !== from.id() && box.id !== to.id() && nearPoint(box, mid, radius),
      );
      if (!blocked) {
        drop();
        return;
      }
      const length = Math.hypot(b.x - a.x, dy) || 1;
      const nx = -dy / length;
      const ny = (b.x - a.x) / length;
      const up = { x: mid.x + nx * PROBE_OFFSET, y: mid.y + ny * PROBE_OFFSET };
      const down = { x: mid.x - nx * PROBE_OFFSET, y: mid.y - ny * PROBE_OFFSET };
      // 平手时固定往上绕, 保证同样的数据每次画出来一样
      const goUp = countNear(up, from.id()) <= countNear(down, from.id());
      edge.toggleClass("gd-detour-up", goUp);
      edge.toggleClass("gd-detour-down", !goUp);
      routed += 1;
    });
  });
  return routed;
}
