// 砚台 PWA Service Worker
//
// 策略要点(与站点既有缓存方针保持一致):
// - HTML(导航)永远 network-first: 入口 index.html 不发版缓存, 保证新版前端即时生效;
//   断网时回退缓存外壳而不是浏览器错误页。
// - /static/* 与 /fonts/* 文件名带 hash 或内容固定 → cache-first(与 Caddy immutable 一致)。
// - /reader3/*(全部 API, 含 SSE)、/assets/proxy、/tts-gateway/* 一律直连不拦截:
//   接口数据与流式响应绝不进缓存。
// - 封面 /assets/*/covers/* 用 stale-while-revalidate: 秒开且后台更新。
const VERSION = "v1";
const STATIC_CACHE = `yantai-static-${VERSION}`;
const PAGE_CACHE = `yantai-page-${VERSION}`;
const COVER_CACHE = `yantai-cover-${VERSION}`;
const KEEP = new Set([STATIC_CACHE, PAGE_CACHE, COVER_CACHE]);
const PRECACHE = ["/", "/favicon.svg", "/icon-192.png", "/icon-512.png", "/apple-touch-icon.png"];

/** 不拦截的路径前缀: API 与流式接口 */
const BYPASS = [/^\/reader3\//, /^\/assets\/proxy/, /^\/tts-gateway\//];

const OFFLINE_HTML = `<!doctype html>
<html lang="zh-CN"><head><meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>砚台 · 离线</title>
<style>
  html,body{height:100%;margin:0;background:#fcfaf6;color:#1c1917;
    font-family:system-ui,-apple-system,"Noto Sans SC",sans-serif}
  @media (prefers-color-scheme:dark){html,body{background:#171310;color:#f2ede6}}
  main{height:100%;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;padding:24px;text-align:center}
  h1{font-size:17px;font-weight:600;margin:0}
  p{margin:0;font-size:13px;opacity:.65;line-height:1.7}
  button{margin-top:8px;padding:8px 16px;border-radius:10px;border:1px solid currentColor;
    background:transparent;color:inherit;font-size:13px;opacity:.8}
</style></head>
<body><main>
  <h1>当前处于离线状态</h1>
  <p>书架与已缓存的页面仍可打开;<br>联网后即可继续搜索与追更。</p>
  <button onclick="location.reload()">重试</button>
</main></body></html>`;

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(PAGE_CACHE);
      // 单个资源失败不阻塞安装(precache 仅为乐观优化)
      await Promise.allSettled(
        PRECACHE.map((url) => cache.add(new Request(url, { cache: "reload" }))),
      );
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(names.filter((n) => !KEEP.has(n)).map((n) => caches.delete(n)));
      await self.clients.claim();
    })(),
  );
});

/** 导航: 网络优先, 失败回退缓存外壳, 再失败给离线页 */
async function handleNavigation(request) {
  try {
    const response = await fetch(request);
    if (response && response.ok) {
      const cache = await caches.open(PAGE_CACHE);
      cache.put("/", response.clone());
    }
    return response;
  } catch {
    const cached = (await caches.match(request)) ?? (await caches.match("/"));
    if (cached) {
      return cached;
    }
    return new Response(OFFLINE_HTML, {
      status: 200,
      headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" },
    });
  }
}

/** 静态资源: 缓存优先(文件名带 hash) */
async function handleCacheFirst(request, cacheName) {
  const cached = await caches.match(request);
  if (cached) {
    return cached;
  }
  const response = await fetch(request);
  if (response && response.ok && response.type === "basic") {
    const cache = await caches.open(cacheName);
    cache.put(request, response.clone());
  }
  return response;
}

/** 封面等: 先给缓存, 后台顺手更新 */
async function handleStaleWhileRevalidate(request, cacheName) {
  const cache = await caches.open(cacheName);
  const cached = await cache.match(request);
  const network = fetch(request)
    .then((response) => {
      if (response && response.ok && response.type === "basic") {
        cache.put(request, response.clone());
      }
      return response;
    })
    .catch(() => undefined);
  return cached ?? (await network) ?? Response.error();
}

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") {
    return;
  }
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) {
    return;
  }
  if (BYPASS.some((re) => re.test(url.pathname))) {
    return;
  }
  if (request.mode === "navigate") {
    event.respondWith(handleNavigation(request));
    return;
  }
  if (url.pathname.startsWith("/static/") || url.pathname.startsWith("/fonts/")) {
    event.respondWith(handleCacheFirst(request, STATIC_CACHE));
    return;
  }
  if (/^\/assets\/.*\/covers\//.test(url.pathname) || /^\/(icon|apple-touch-icon)/.test(url.pathname)) {
    event.respondWith(handleStaleWhileRevalidate(request, COVER_CACHE));
    return;
  }
  if (url.pathname === "/manifest.webmanifest") {
    event.respondWith(handleStaleWhileRevalidate(request, STATIC_CACHE));
  }
});
