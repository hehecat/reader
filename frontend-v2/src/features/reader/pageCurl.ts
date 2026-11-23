/**
 * 2D Physical Book Page-Curl Geometry & Retina Canvas Lighting Engine
 *
 * Implements Apple Books / iBooks two-circle spine-constrained fold geometry:
 * - Sutherland-Hodgman half-plane polygon clipping along the perpendicular bisector crease
 * - Exact rigid composition (R_crease ∘ R_spine, det = +1) for dual-page spread backside content
 * - Retina <canvas> cylindrical paper shading, specular roll ridge, and dual-sided cast shadows
 */

export interface Point {
  x: number;
  y: number;
}

export type CurlMode = "spread-next" | "spread-prev" | "single-next" | "single-prev";

export interface CurlPalette {
  paperBg: string;
  paperBack: string;
  inkRgb: string;
  isDark: boolean;
}

export interface CurlGeometry {
  mode: CurlMode;
  w: number;
  h: number;
  spineX: number;
  freeX: number;
  corner: Point;
  tip: Point;
  mid: Point;
  /** Unit normal pointing from lifted `tip` (P) toward original free `corner` (C) */
  normal: Point;
  /** Distance |C - P| */
  foldDist: number;
  /** Normalized gesture completion in [0, 1] (0 = start flat, 1 = turn complete) */
  progress: number;
  /** Screen-space polygon of the unpeeled flat portion of the full sheet [0, W] x [0, H] */
  unpeeledPoly: Point[];
  /** Screen-space polygon of the peeled portion of the active leaf */
  peeledLeafPoly: Point[];
  /** Screen-space polygon of the reflected lifted flap */
  flapPoly: Point[];
  /** CSS `polygon(...)` for clipping the top peeling sheet */
  topClipCss: string;
  /** CSS `polygon(...)` for clipping the reflected flap container */
  flapClipCss: string;
  /**
   * Rigid 2D CSS `matrix(a, b, c, d, tx, ty)` mapping the incoming opposite spread leaf
   * onto the reflected flap with right-side-up text (lands at identity when progress = 1).
   */
  flapContentMatrix: string;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

function projectToCircle(p: Point, center: Point, maxRadius: number): Point {
  const dx = p.x - center.x;
  const dy = p.y - center.y;
  const dist = Math.hypot(dx, dy);
  if (dist <= maxRadius || dist < 1e-5) return p;
  const scale = maxRadius / dist;
  return {
    x: center.x + dx * scale,
    y: center.y + dy * scale,
  };
}

/**
 * Sutherland-Hodgman polygon clip against half-plane:
 * signedDistance(Q) = (Q - M) · n
 * - keepPositive = true  => keeps sd >= 0 (peeled side closer to C)
 * - keepPositive = false => keeps sd <= 0 (unpeeled side closer to spine)
 */
function clipPolygonByCrease(
  poly: readonly Point[],
  mid: Point,
  normal: Point,
  keepPositive: boolean,
): Point[] {
  if (poly.length === 0) return [];
  const sign = keepPositive ? 1 : -1;
  const sd = (p: Point) => ((p.x - mid.x) * normal.x + (p.y - mid.y) * normal.y) * sign;

  const out: Point[] = [];
  for (let i = 0; i < poly.length; i += 1) {
    const cur = poly[i]!;
    const prev = poly[(i + poly.length - 1) % poly.length]!;
    const dCur = sd(cur);
    const dPrev = sd(prev);
    const curIn = dCur >= -1e-4;
    const prevIn = dPrev >= -1e-4;

    if (curIn !== prevIn) {
      const denom = dPrev - dCur;
      const t = Math.abs(denom) > 1e-7 ? dPrev / denom : 0.5;
      out.push({
        x: prev.x + (cur.x - prev.x) * t,
        y: prev.y + (cur.y - prev.y) * t,
      });
    }
    if (curIn) {
      out.push(cur);
    }
  }
  return out;
}

function reflectPoint(p: Point, mid: Point, normal: Point): Point {
  const d = (p.x - mid.x) * normal.x + (p.y - mid.y) * normal.y;
  return {
    x: p.x - 2 * d * normal.x,
    y: p.y - 2 * d * normal.y,
  };
}

export function pointsToCssPolygon(pts: readonly Point[]): string {
  if (pts.length < 3) return "polygon(0px 0px, 0px 0px, 0px 0px)";
  return `polygon(${pts.map((p) => `${p.x.toFixed(1)}px ${p.y.toFixed(1)}px`).join(", ")})`;
}

/**
 * Compute full physical curl geometry from a desired tip position `rawTip` in [0..W, 0..H] space.
 */
export function computeCurlGeometry(
  mode: CurlMode,
  w: number,
  h: number,
  cornerY: "top" | "bottom",
  rawTip: Point,
): CurlGeometry {
  const isSpread = mode === "spread-next" || mode === "spread-prev";
  const spineX = isSpread ? w * 0.5 : 0;
  // Note: single-prev unrolls page (p-1) from left spine (0) toward right free edge (W)
  const freeX = mode === "spread-prev" ? 0 : w;
  const leafWidth = Math.abs(freeX - spineX);
  const cy = cornerY === "top" ? 0 : h;
  const oppositeY = cornerY === "top" ? h : 0;
  const corner: Point = { x: freeX, y: cy };

  // Keep tip slightly off exact corner so normal vector never degenerates
  let tipX = rawTip.x;
  const tipY = rawTip.y;
  if (freeX > spineX) {
    tipX = Math.min(freeX - 1.5, tipX);
  } else {
    tipX = Math.max(freeX + 1.5, tipX);
  }

  // Two-circle physical spine constraint: prevents crease from crossing spine segment (spineX, 0..H)
  let tip: Point = { x: tipX, y: tipY };
  tip = projectToCircle(tip, { x: spineX, y: cy }, leafWidth);
  tip = projectToCircle(tip, { x: spineX, y: oppositeY }, Math.hypot(leafWidth, h));

  // Re-ensure minimum separation from corner after projection
  let dx = corner.x - tip.x;
  let dy = corner.y - tip.y;
  let foldDist = Math.hypot(dx, dy);
  if (foldDist < 1.5) {
    const dirSign = freeX > spineX ? -1 : 1;
    tip = { x: corner.x + dirSign * 1.5, y: corner.y };
    dx = corner.x - tip.x;
    dy = 0;
    foldDist = 1.5;
  }

  const normal: Point = { x: dx / foldDist, y: dy / foldDist };
  const mid: Point = { x: (corner.x + tip.x) * 0.5, y: (corner.y + tip.y) * 0.5 };

  // Full sheet rectangle [0, W] x [0, H]
  const fullRect: Point[] = [
    { x: 0, y: 0 },
    { x: w, y: 0 },
    { x: w, y: h },
    { x: 0, y: h },
  ];

  // Active leaf rectangle
  const leafMinX = Math.min(spineX, freeX);
  const leafMaxX = Math.max(spineX, freeX);
  const leafRect: Point[] = [
    { x: leafMinX, y: 0 },
    { x: leafMaxX, y: 0 },
    { x: leafMaxX, y: h },
    { x: leafMinX, y: h },
  ];

  const unpeeledPoly = clipPolygonByCrease(fullRect, mid, normal, false);
  const peeledLeafPoly = clipPolygonByCrease(leafRect, mid, normal, true);
  const flapPoly = peeledLeafPoly.map((pt) => reflectPoint(pt, mid, normal));

  // Normalized gesture completion [0..1]
  // Full span of tip.x is from `freeX` (flat) to `spineX - (freeX - spineX)` = `2 * spineX - freeX`
  const fullSpan = 2 * leafWidth;
  const rawTravel = Math.abs(freeX - tip.x) / Math.max(1, fullSpan);
  const progress =
    mode === "single-prev" ? clamp(1 - rawTravel, 0, 1) : clamp(rawTravel, 0, 1);

  // Rigid 2D matrix (R_crease ∘ R_spine) for dual-page spread backside content
  const nx = normal.x;
  const ny = normal.y;
  const a = 2 * nx * nx - 1;
  const b = 2 * nx * ny;
  const c = -2 * nx * ny;
  const d = 2 * nx * nx - 1;
  const du = 2 * spineX - mid.x;
  const dv = -mid.y;
  const dot = du * nx + dv * ny;
  const tx = 2 * spineX - 2 * dot * nx;
  const ty = -2 * dot * ny;
  const flapContentMatrix = `matrix(${a.toFixed(6)}, ${b.toFixed(6)}, ${c.toFixed(6)}, ${d.toFixed(6)}, ${tx.toFixed(2)}, ${ty.toFixed(2)})`;

  return {
    mode,
    w,
    h,
    spineX,
    freeX,
    corner,
    tip,
    mid,
    normal,
    foldDist,
    progress,
    unpeeledPoly,
    peeledLeafPoly,
    flapPoly,
    topClipCss: pointsToCssPolygon(unpeeledPoly),
    flapClipCss: pointsToCssPolygon(flapPoly),
    flapContentMatrix,
  };
}

/**
 * Map a live pointer drag `(curX, curY)` from `(startX, startY)` into a smooth tip position `P`.
 */
export function pointerToCurlTip(params: {
  mode: CurlMode;
  w: number;
  h: number;
  cornerY: "top" | "bottom";
  startX: number;
  startY: number;
  curX: number;
  curY: number;
  hardBoundary: boolean;
}): Point {
  const { mode, w, h, cornerY, startX, startY, curX, curY, hardBoundary } = params;
  const cy = cornerY === "top" ? 0 : h;
  const rawDx = curX - startX;
  const rawDy = curY - startY;

  // Rubber-band damping when at book's very first/last page with no adjacent chapter
  const damp = hardBoundary ? 0.22 : 1;
  const dx = rawDx * damp;
  const dy = rawDy * (hardBoundary ? 0.35 : 0.88);

  if (mode === "single-prev") {
    // Unrolling previous page from left spine (tip moves from -W toward +W as user drags right)
    const rightTravel = Math.max(4, dx * 2.05);
    const tipX = Math.min(w - 2, -w + rightTravel);
    const arcLift =
      (cornerY === "bottom" ? -1 : 1) *
      Math.sin(clamp(rightTravel / (2 * w), 0, 1) * Math.PI) *
      Math.min(42, h * 0.065);
    return { x: tipX, y: cy + dy + arcLift };
  }

  if (mode === "spread-prev") {
    // Peeling left leaf [0..W/2] rightward from x = 0 toward x = W
    const rightDrag = Math.max(0, dx);
    const decay = Math.max(0, 1 - rightDrag / 96);
    const baseTipX = hardBoundary ? Math.max(3, rightDrag) : curX - Math.max(0, startX) * decay;
    const tipX = clamp(Math.max(3, baseTipX), 3, hardBoundary ? w * 0.22 : w);
    const norm = clamp(tipX / Math.max(1, w), 0, 1);
    const arcLift =
      (cornerY === "bottom" ? -1 : 1) * Math.sin(norm * Math.PI) * Math.min(38, h * 0.06);
    return { x: tipX, y: cy + dy + arcLift };
  }

  // spread-next & single-next: peeling from right edge x = W leftward
  const leftDrag = Math.max(0, -dx);
  const fullSpan = mode === "spread-next" ? w : 2 * w;
  const leverage = mode === "single-next" ? 1.55 : 1.08;
  const decay = Math.max(0, 1 - leftDrag / 96);
  const inwardFromFinger =
    mode === "single-next"
      ? Math.max(3, leftDrag * leverage)
      : Math.max(3, w - curX - Math.max(0, w - startX) * decay);
  const maxInward = hardBoundary ? Math.min(110, fullSpan * 0.16) : fullSpan;
  const tipX = w - clamp(inwardFromFinger, 3, maxInward);
  const norm = clamp((w - tipX) / Math.max(1, fullSpan), 0, 1);
  const arcLift =
    (cornerY === "bottom" ? -1 : 1) * Math.sin(norm * Math.PI) * Math.min(42, h * 0.065);

  return { x: tipX, y: cy + dy + arcLift };
}

/**
 * Map normalized gesture `progress` in [0, 1] (0 = start/cancelled, 1 = turn completed)
 * onto a smooth 3D arched tip trajectory `P`.
 */
export function progressToArcTip(
  mode: CurlMode,
  w: number,
  h: number,
  cornerY: "top" | "bottom",
  progress: number,
  archHeight = 46,
): Point {
  const p = clamp(progress, 0.002, 0.9995);
  const travel = mode === "single-prev" ? 1 - p : p;
  const cy = cornerY === "top" ? 0 : h;
  const isSpread = mode === "spread-next" || mode === "spread-prev";
  const spineX = isSpread ? w * 0.5 : 0;
  const freeX = mode === "spread-prev" ? 0 : w;
  const targetX = 2 * spineX - freeX;

  const x = freeX + (targetX - freeX) * travel;
  const liftDir = cornerY === "bottom" ? -1 : 1;
  const y = cy + liftDir * Math.sin(travel * Math.PI) * Math.min(archHeight, h * 0.09);
  return { x, y };
}

function tracePoly(ctx: CanvasRenderingContext2D, pts: readonly Point[]) {
  if (pts.length < 3) return;
  ctx.beginPath();
  ctx.moveTo(pts[0]!.x, pts[0]!.y);
  for (let i = 1; i < pts.length; i += 1) {
    ctx.lineTo(pts[i]!.x, pts[i]!.y);
  }
  ctx.closePath();
}

/**
 * Render realistic 3D cylindrical curl shading, paper backside (for single-page mode),
 * and dual-sided cast shadows onto a Retina overlay `<canvas>`.
 */
export function drawCurlLighting(
  ctx: CanvasRenderingContext2D,
  dpr: number,
  geom: CurlGeometry,
  palette: CurlPalette,
) {
  ctx.save();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, geom.w, geom.h);

  if (geom.peeledLeafPoly.length < 3 || geom.flapPoly.length < 3) {
    ctx.restore();
    return;
  }

  const { mid, normal, foldDist, w, h, isDark } = {
    ...geom,
    isDark: palette.isDark,
  };
  const maxSpan = geom.mode.startsWith("spread") ? w : 2 * w;
  const openRatio = clamp(foldDist / Math.max(1, maxSpan), 0, 1);
  // Shadow fades smoothly to 0 in the final 8% so full landing is 100% seamless
  const endFade = clamp((1 - openRatio) / 0.1, 0, 1);
  const startFade = clamp(foldDist / 24, 0, 1);
  const intensity = startFade * endFade;

  if (intensity <= 0.004) {
    ctx.restore();
    return;
  }

  const flapWidth = Math.max(12, foldDist * 0.5);

  /* 1. Cast Drop-Shadow onto Revealed Underneath Page (along +normal from crease) */
  ctx.save();
  tracePoly(ctx, geom.peeledLeafPoly);
  ctx.clip();
  const underShadowW = Math.min(88, Math.max(16, flapWidth * 0.55));
  const ug = ctx.createLinearGradient(
    mid.x,
    mid.y,
    mid.x + normal.x * underShadowW,
    mid.y + normal.y * underShadowW,
  );
  const underAlpha = (isDark ? 0.52 : 0.22) * intensity;
  ug.addColorStop(0, `rgba(12, 10, 8, ${underAlpha.toFixed(3)})`);
  ug.addColorStop(0.38, `rgba(12, 10, 8, ${(underAlpha * 0.42).toFixed(3)})`);
  ug.addColorStop(1, "rgba(12, 10, 8, 0)");
  ctx.fillStyle = ug;
  ctx.fillRect(0, 0, w, h);
  ctx.restore();

  /* 2. Cast Shadow + Subtle Paper Bend onto Flat Unpeeled Page (along -normal from crease) */
  if (geom.unpeeledPoly.length >= 3) {
    ctx.save();
    tracePoly(ctx, geom.unpeeledPoly);
    ctx.clip();
    const overShadowW = Math.min(flapWidth + 28, 120);
    const og = ctx.createLinearGradient(
      mid.x,
      mid.y,
      mid.x - normal.x * overShadowW,
      mid.y - normal.y * overShadowW,
    );
    const overAlpha = (isDark ? 0.44 : 0.15) * intensity;
    og.addColorStop(0, `rgba(12, 10, 8, ${(overAlpha * 0.55).toFixed(3)})`);
    og.addColorStop(
      clamp(flapWidth / overShadowW, 0.15, 0.85),
      `rgba(12, 10, 8, ${(overAlpha * 0.85).toFixed(3)})`,
    );
    og.addColorStop(1, "rgba(12, 10, 8, 0)");
    ctx.fillStyle = og;
    ctx.fillRect(0, 0, w, h);
    ctx.restore();
  }

  /* 3. Lifted Flap Body & Cylindrical 3D Specular Roll Shading */
  ctx.save();
  tracePoly(ctx, geom.flapPoly);
  ctx.clip();

  // In single-page modes, paint opaque paper backside + faint bleed-through lines;
  // in spread modes, the real incoming opposite leaf DOM sits right beneath this canvas!
  const isSingle = geom.mode === "single-next" || geom.mode === "single-prev";
  if (isSingle) {
    ctx.fillStyle = palette.paperBack;
    ctx.fillRect(0, 0, w, h);

    // Subtle reversed bleed-through ruled hint on paper backside
    ctx.save();
    // Reflect across crease so bleed-through lines rotate realistically with the flap!
    const nx = normal.x;
    const ny = normal.y;
    const ra = 1 - 2 * nx * nx;
    const rb = -2 * nx * ny;
    const rc = -2 * nx * ny;
    const rd = 1 - 2 * ny * ny;
    const rdot = mid.x * nx + mid.y * ny;
    ctx.transform(ra, rb, rc, rd, 2 * rdot * nx, 2 * rdot * ny);
    ctx.fillStyle = `rgba(${palette.inkRgb}, ${isDark ? "0.032" : "0.038"})`;
    const marginX = Math.min(44, w * 0.08);
    const lineStep = 30;
    for (let y = 56; y < h - 48; y += lineStep) {
      const wLine = (w - marginX * 2) * (y % 90 === 26 ? 0.72 : 0.94);
      ctx.fillRect(marginX, y, wLine, 2);
    }
    ctx.restore();
  }

  // Cylindrical 3D roll lighting perpendicular to crease (from M toward tip P along -normal)
  const fg = ctx.createLinearGradient(
    mid.x,
    mid.y,
    mid.x - normal.x * flapWidth,
    mid.y - normal.y * flapWidth,
  );
  if (isDark) {
    fg.addColorStop(0, `rgba(0, 0, 0, ${(0.34 * intensity).toFixed(3)})`);
    fg.addColorStop(0.14, `rgba(255, 250, 240, ${(0.09 * intensity).toFixed(3)})`);
    fg.addColorStop(0.45, "rgba(255, 255, 255, 0.02)");
    fg.addColorStop(1, `rgba(0, 0, 0, ${(0.22 * intensity).toFixed(3)})`);
  } else {
    fg.addColorStop(0, `rgba(32, 24, 16, ${(0.13 * intensity).toFixed(3)})`);
    fg.addColorStop(0.13, `rgba(255, 255, 255, ${(0.52 * intensity).toFixed(3)})`);
    fg.addColorStop(0.36, `rgba(255, 255, 255, ${(0.14 * intensity).toFixed(3)})`);
    fg.addColorStop(0.88, `rgba(38, 30, 20, ${(0.045 * intensity).toFixed(3)})`);
    fg.addColorStop(1, `rgba(38, 30, 20, ${(0.09 * intensity).toFixed(3)})`);
  }
  ctx.fillStyle = fg;
  ctx.fillRect(0, 0, w, h);
  ctx.restore();

  /* 4. Crisp Hairline Paper Edge Stroke around Lifted Flap */
  ctx.save();
  tracePoly(ctx, geom.flapPoly);
  ctx.strokeStyle = isDark
    ? `rgba(255, 248, 236, ${(0.14 * intensity).toFixed(3)})`
    : `rgba(${palette.inkRgb}, ${(0.13 * intensity).toFixed(3)})`;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.restore();

  ctx.restore();
}

export function readThemeCurlPalette(el: HTMLElement | null): CurlPalette {
  const isDark =
    typeof document !== "undefined" && document.documentElement.classList.contains("dark");
  const paper = typeof document !== "undefined" ? document.documentElement.dataset.paper : "paper";
  const inkRgb = isDark
    ? "232, 228, 220"
    : paper === "parchment"
      ? "67, 52, 34"
      : paper === "celadon"
        ? "34, 53, 41"
        : "38, 34, 30";
  if (!el || typeof window === "undefined") {
    return {
      paperBg: isDark ? "#171513" : "#fcfaf6",
      paperBack: isDark ? "#211e1b" : "#f7f3eb",
      inkRgb,
      isDark,
    };
  }
  const cs = window.getComputedStyle(document.body);
  const bg = cs.backgroundColor || (isDark ? "#171513" : "#fcfaf6");
  const paperBack =
    paper === "parchment"
      ? "#efe4ca"
      : paper === "celadon"
        ? "#e4f0e6"
        : isDark
          ? "#221f1c"
          : "#f6f2ea";
  return {
    paperBg: bg,
    paperBack,
    inkRgb,
    isDark,
  };
}
