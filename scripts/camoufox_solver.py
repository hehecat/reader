#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""camoufox 验证码/登录 HTTP 服务（默认端口 8196）

camoufox（Playwright 封装，Firefox 内核 + 真实指纹预设：navigator/screen/WebGL/
字体/canvas 噪声等）替代手搓 stealth：求解 Cloudflare 质询/Turnstile managed
challenge、登录表单填写 + 滑块拖拽 + 图片验证码两步流。

协议（reader-dev 后端调用）：
- GET  /health            → {"ok": true, "camoufoxVersion": "...", "browserReady": bool}
- POST /solve             → 请求 {"url", "cookies"?[{name,value}], "maxWaitMs"?,
                             "userAgent"?（Chrome Windows UA 覆盖，自动补 sec-ch-ua 头）,
                             "proxy"?（socks5://host:port 等住宅代理——机房 IP 解 Turnstile 必需）,
                             "post"?{"action","body","contentType"?,"charset"?,"mode"?("fetch"|"navigate")}}
                            成功 {"html","cookies","userAgent","turnstileToken","diagnostics"}
                            + 可选 postResult；失败 HTTP 200 + {"error","diagnostics"}
- POST /login            → 请求 {"url","username","password","cookies"?,"userAgent"?,"proxy"?,
                             "maxWaitMs"?} 表单填写+提交+自动处理滑块/质询
                            返回 {"status":"ok","html","cookies","userAgent","turnstileToken","sessionId"}
                            或 {"status":"need_captcha","sessionId","captcha":{base64,x,y,w,h}}
                            或 {"status":"timeout"/"error","error","diagnostics"}
                            会话（页面）保留在服务端，供 /login/captcha 两步回填
- POST /login/captcha    → 请求 {"sessionId","captcha"} 回填验证码→重新提交→等待
                            返回同上（可能再次 need_captcha——验证码错了换图）
- POST /login/close      → 请求 {"sessionId"} 关闭会话
- POST /slider           → 请求 {"url"?,"cookies"?,"userAgent"?,"proxy"?,"maxWaitMs"?}
                            加载（可选）→ 检测滑块 → 可信拖拽 → {"ok","diagnostics"}
- POST /screenshot       → 请求 {"url"?,"selector"?,"clip"?{x,y,w,h},"cookies"?,"proxy"?}
                            → {"base64": png}

依赖：pip install camoufox && camoufox fetch（浏览器二进制）
用法：python scripts/camoufox_solver.py [--port 8196] [--host 127.0.0.1]
测试：python -m py_compile scripts/camoufox_solver.py

69shuba 实测：Chrome wire UA 直过 CF；search.php 是 Turnstile managed challenge
（/solve post.mode="navigate" 表单提交触发 widget 自动点击）。
2026-08-06 结论：数据中心 IP 上 Turnstile 挑战平台直接拒绝（event:fail code:400030
——环境风控，与 UA/头/指纹无关）——需住宅 IP 代理（proxy 参数）才能全自动。
"""
import argparse
import asyncio
import base64
import json
import os
import random
import re
import sys
import time
import uuid

from camoufox.async_api import AsyncCamoufox, AsyncNewContext

PORT = int(os.environ.get("CAMOUFOX_SOLVER_PORT", "8196"))
DEFAULT_MAX_WAIT_MS = 60000
# 登录/滑块等交互流程默认等待上限
LOGIN_MAX_WAIT_MS = 60000
# 会话闲置回收时限
SESSION_TTL_SEC = 600.0

# UA 覆盖（69shuba 等站点 UA 门禁）：Playwright user_agent 选项只改线上（wire）UA——
# camoufox 指纹注入脚本（setNavigatorUserAgent）会把 JS 可见 navigator.userAgent 改回
# Firefox，两侧不一致会触发站点门禁/指纹检测。正解：generate_context_fingerprint 的
# config_overrides={'navigator.userAgent': ...}——wire UA 与 JS UA 同时为覆盖值。
# 回退：AsyncNewContext(user_agent=...) + 追加 init script 二次补丁 navigator.userAgent。
UA_PATCH_INIT_JS = """
(() => {
  const ua = %r;
  try { Object.defineProperty(Navigator.prototype, 'userAgent', { get: () => ua, configurable: true }); } catch (e) {}
  try { Object.defineProperty(navigator, 'userAgent', { get: () => ua, configurable: true }); } catch (e) {}
})()
"""


def _proxy_opts(proxy):
    """代理字符串 → Playwright proxy 选项（None/空 → {}）"""
    if not proxy or not str(proxy).strip():
        return {}
    p = str(proxy).strip()
    return {"proxy": {"server": p}}


async def new_context_with_ua(browser, user_agent=None, proxy=None):
    """新建 camoufox 指纹 context；user_agent 覆盖时保证 wire 与 JS 两侧一致；
    proxy（socks5://… 住宅代理）时传给 context——机房 IP 解 Turnstile 必需。

    优先 config_overrides（camoufox 0.5.4 generate_context_fingerprint 私有 API，
    try/except 回退到二次 init script 补丁——两路均已实测：JS/WIRE UA 均为覆盖值）。
    """
    px = _proxy_opts(proxy)
    if not user_agent:
        return await AsyncNewContext(browser, os="windows", **px)
    try:
        from camoufox.fingerprints import generate_context_fingerprint

        fp = await asyncio.get_event_loop().run_in_executor(
            None,
            lambda: generate_context_fingerprint(
                os="windows", config_overrides={"navigator.userAgent": user_agent}
            ),
        )
        opts = dict(fp.get("context_options") or {})
        opts["extra_http_headers"] = chrome_hint_headers(user_agent)
        opts.update(px)
        ctx = await browser.new_context(**opts)
        await ctx.add_init_script(fp.get("init_script") or "")
        return ctx
    except Exception:
        ctx = await AsyncNewContext(
            browser,
            os="windows",
            user_agent=user_agent,
            extra_http_headers=chrome_hint_headers(user_agent),
            **px,
        )
        await ctx.add_init_script(UA_PATCH_INIT_JS % user_agent)
        return ctx


def chrome_hint_headers(user_agent):
    """Chrome UA 时补 sec-ch-ua 客户端提示头（Chromium 系默认携带；Firefox 不发送）。
    69shuba 等站点会用 Sec-CH-UA 交叉验证 UA——缺失即非 Chrome 判定。"""
    m = re.search(r"Chrome/(\d+)", user_agent or "")
    if not m:
        return {}
    v = m.group(1)
    return {
        "Sec-CH-UA": f'"Chromium";v="{v}", "Google Chrome";v="{v}", "Not.A/Brand";v="24"',
        "Sec-CH-UA-Mobile": "?0",
        "Sec-CH-UA-Platform": '"Windows"',
    }


# ==================== 页内 JS（与 browser.rs 同源语义） ====================

# 页内 fetch POST（搜索等表单链路——同源 cookie/referer 自动携带，CF 视为真实请求）。
# 响应按 charset 解码（69shuba search.php 为 GBK——res.text() 会按 UTF-8 乱码）。
# 注意：Playwright evaluate 字符串里 arguments 不可用——payload 直接 json.dumps 嵌入。
POST_FETCH_JS = """
(async () => {
  const p = %s;
  try {
    const r = await fetch(p.action, {
      method: 'POST',
      headers: { 'Content-Type': p.contentType || 'application/x-www-form-urlencoded' },
      body: p.body || '',
      credentials: 'include'
    });
    let text = '';
    try {
      const buf = await r.arrayBuffer();
      text = new TextDecoder(p.charset || 'utf-8').decode(buf);
    } catch (e) { text = await r.text(); }
    return { status: r.status, url: r.url, html: text };
  } catch (e) {
    return { error: String((e && e.message) || e) };
  }
})()
"""

# 质询状态求值 JS（与 browser.rs CF_CHALLENGE_STATE_JS / TURNSTILE_DETECT_JS 同特征）：
# challenge = 仍在质询页；hasInput = Turnstile 隐藏 input 已渲染（managed challenge
# 勾选成功的标志）；inputValue = cf-turnstile-response 值（非空即通过）
CHALLENGE_STATE_JS = """
(function(){
  try {
    var features = document.querySelector('#challenge-form, [id^="cf-chl-"], [class*="cf-chl"], iframe[src*="challenges.cloudflare.com"], #cfts, [name="cf-turnstile-response"]');
    var t = (document.title || '').toLowerCase();
    var input = document.querySelector('[name="cf-turnstile-response"]');
    return {
      challenge: !!(features || t.indexOf('just a moment') >= 0 || t.indexOf('turnstile') >= 0 || t.indexOf('verifying') >= 0),
      hasInput: !!input,
      inputValue: input && input.value ? input.value : '',
      title: document.title || '',
      url: location.href,
      bodyChildren: document.body ? document.body.children.length : 0
    };
  } catch (e) { return { challenge: true, hasInput: false, inputValue: '', title: '', url: '', bodyChildren: 0 }; }
})()
"""

# 登录表单填写（原生 setter 触发 input/change——Vue/React 表单可识别；USERNAME/PASSWORD
# 占位符由 Python 侧替换为 json.dumps 转义的字符串）
FILL_FORM_JS = """
(function(){
  function setVal(el, v){
    var proto = el.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
    var setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
    setter.call(el, v);
    el.dispatchEvent(new Event('input', {bubbles:true}));
    el.dispatchEvent(new Event('change', {bubbles:true}));
  }
  var pw = document.querySelector('input[type="password"]');
  if (!pw) return {ok:false, reason:'no-password-input'};
  setVal(pw, 'PASSWORD');
  var user = document.querySelector('input[name*="user" i], input[id*="user" i], input[name*="name" i], input[placeholder*="用户" i], input[placeholder*="账号" i]');
  if (!user) {
    var inputs = document.querySelectorAll('input');
    for (var i = 0; i < inputs.length; i++) {
      var it = inputs[i];
      if (it === pw) continue;
      var t = (it.type||'text').toLowerCase();
      if (t === 'text' || t === 'email' || t === '' || t === 'tel' || t === 'number') {
        var r = it.getBoundingClientRect();
        if (r.width > 2 && r.height > 2) { user = it; break; }
      }
    }
  }
  if (user) setVal(user, 'USERNAME');
  return {ok:true, filled:!!user};
})()
"""

# 表单提交（优先 submit 按钮点击，其次 form.requestSubmit，最后 form.submit）
SUBMIT_FORM_JS = """
(function(){
  try {
  var btn = document.querySelector('button[type="submit"], input[type="submit"], button.btn-primary, button.btn, form button');
  if (btn) { btn.click(); return {ok:true, how:'click'}; }
  var form = document.querySelector('form');
  if (form) {
    if (form.requestSubmit) { form.requestSubmit(); return {ok:true, how:'requestSubmit'}; }
    form.submit(); return {ok:true, how:'submit'};
  }
  return {ok:false, reason:'no-form'};
  } catch(e) { return {ok:false, reason:'exception'}; }
})()
"""

# 验证码输入框填写（CAPTCHA 占位符由 Python 侧替换）
FILL_CAPTCHA_JS = """
(function(){
  try {
  var el = document.querySelector('input[name*="captcha" i], input[id*="captcha" i], input[placeholder*="验证码" i], input[placeholder*="captcha" i], input[type="text"][name*="code" i]');
  if (!el) return {ok:false, reason:'no-captcha-input'};
  var setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
  setter.call(el, 'CAPTCHA');
  el.dispatchEvent(new Event('input', {bubbles:true}));
  el.dispatchEvent(new Event('change', {bubbles:true}));
  return {ok:true};
  } catch(e) { return {ok:false, reason:'exception'}; }
})()
"""

# 验证码检测（与 browser.rs DETECT_CAPTCHA_JS 同源）：kind=image / slider / click / null
DETECT_CAPTCHA_JS = """
(function(){
  try {
  function visible(el){
    if(!el) return false;
    var r = el.getBoundingClientRect();
    return r.width > 2 && r.height > 2 && r.top < innerHeight && r.left < innerWidth;
  }
  var imgs = document.querySelectorAll('img');
  for (var i = 0; i < imgs.length; i++) {
    var im = imgs[i];
    var ctx = ((im.src||'') + ' ' + (im.id||'') + ' ' + (im.className||'') + ' ' + (im.alt||'')).toLowerCase();
    if (/captcha|vcode|verify|yzm|checkcode|验证码|randimg|kaptcha/.test(ctx) && visible(im)) {
      var r = im.getBoundingClientRect();
      return {kind:'image', x:r.x, y:r.y, w:r.width, h:r.height, src:im.src};
    }
  }
  var sliderSels = ['.geetest_slider_button','.geetest_slider','.slide-verify','.slider-verify','.captcha-slider',
    '[class*="geetest"]','[class*="slide-verify"]','#nc_1_n1z','.nc_iconfont','.btn_slide','.drag-slider',
    '.verify-slider','[class*="jigsaw"]','[class*="slider-btn"]','[class*="slider-button"]','[class*="captcha-slider"]'];
  for (var i = 0; i < sliderSels.length; i++) {
    var el = document.querySelector(sliderSels[i]);
    if (visible(el)) {
      var r = el.getBoundingClientRect();
      var track = el, tr = r;
      var p = el.parentElement;
      while (p) {
        var pr = p.getBoundingClientRect();
        var pc = ((p.className||'') + ' ' + (p.id||'')).toLowerCase();
        if (pr.width > tr.width + 20 && /slider|geetest|captcha|nc_|verify|drag/.test(pc)) { track = p; tr = pr; }
        p = p.parentElement;
      }
      return {kind:'slider', x:r.x, y:r.y, w:r.width, h:r.height,
              trackX:tr.x, trackY:tr.y, trackW:tr.width, trackH:tr.height};
    }
  }
  var clickSels = ['[class*="click-verify"]','[class*="clickCaptcha"]','[class*="tcaptcha"]','[class*="verify-point"]','[class*="points-verify"]'];
  for (var i = 0; i < clickSels.length; i++) {
    var el = document.querySelector(clickSels[i]);
    if (visible(el)) {
      var r = el.getBoundingClientRect();
      return {kind:'click', x:r.x, y:r.y, w:r.width, h:r.height};
    }
  }
  return null;
  } catch(e) { return null; }
})()
"""

# 页内表单导航式 POST（post.mode="navigate"）：隐藏表单 submit——同源 cookie/referer
# 自动携带，且页面级导航会渲染 Turnstile widget（fetch 模式拿不到 widget 交互能力）。
POST_NAVIGATE_JS = """
(() => {
  const fields = %s;
  const action = %s;
  const f = document.createElement('form');
  f.method = 'POST';
  f.action = action;
  f.style.display = 'none';
  for (const [n, v] of fields) {
    const inp = document.createElement('input');
    inp.type = 'hidden';
    inp.name = n;
    inp.value = v;
    f.appendChild(inp);
  }
  document.body.appendChild(f);
  f.submit();
  return true;
})()
"""


def form_fields_from_body(body):
    """URL 编码表单体 → [name, value] 对（百分号解码：先 UTF-8，失败回退 GBK——
    69shuba searchkey 为 GBK 字节；提交时浏览器按页面 charset 重新编码）"""
    from urllib.parse import unquote_to_bytes

    fields = []
    for kv in str(body or "").split("&"):
        if "=" not in kv:
            continue
        k, v = kv.split("=", 1)
        k = unquote_to_bytes(k).decode("utf-8", "replace")
        raw = unquote_to_bytes(v)
        try:
            v = raw.decode("utf-8")
        except UnicodeDecodeError:
            v = raw.decode("gbk", "replace")
        fields.append([k, v])
    return fields


_browser = None
_browser_ready = False

# 登录会话：sessionId → {"ctx","page","host","diag","last_used"}
SESSIONS = {}


async def get_browser():
    """惰性启动常驻 camoufox 浏览器（进程生命周期内复用；并发请求经锁排队）"""
    global _browser, _browser_ready
    if _browser is None:
        _browser = await AsyncCamoufox(headless=True, humanize=True).__aenter__()
        _browser_ready = True
    return _browser


def cookies_for_host(cookies, host):
    """筛选目标主机（含父域）的 cookie——与 browser.rs cookie_domain_matches 同语义"""
    out = []
    for c in cookies:
        dom = (c.get("domain") or "").lstrip(".")
        if not dom:
            continue
        if host == dom or (dom.count(".") >= 1 and host.endswith("." + dom)):
            out.append({"name": c.get("name", ""), "value": c.get("value", "")})
    return out


def host_of_url(url):
    try:
        from urllib.parse import urlparse

        return urlparse(url).hostname or ""
    except Exception:
        return ""


async def _inject_cookies_async(context, cookies, host):
    await context.add_cookies(
        [
            {
                "name": c.get("name"),
                "value": c.get("value"),
                "domain": host,
                "path": "/",
                "sameSite": "Lax",
            }
            for c in cookies
            if c.get("name") and c.get("value") is not None
        ]
    )


async def wait_challenge_clear(page, deadline, diag, max_wait_ms, prefix=""):
    """质询等待循环：每 500ms 求值——input 值非空（Turnstile 通过）或 challenge 特征消失
    （经典 CF JS 质询自动解）→ 退出。返回 (ok, error_or_None)。
    注意：部分站点（如 69shuba）有 hidden input 但永远不写值（token 走自定义 callback）——
    点击条件按 inputValue 判，不按 hasInput（否则永不点击）。"""
    start = time.monotonic()
    while True:
        try:
            state = await page.evaluate(CHALLENGE_STATE_JS)
        except Exception:
            state = {"challenge": True, "hasInput": False, "inputValue": "", "title": "", "url": "", "bodyChildren": 0}
        diag["title"] = state.get("title", "")
        diag["hasInput"] = bool(state.get("hasInput"))
        diag["waitMs"] = int((time.monotonic() - start) * 1000)
        if state.get("inputValue"):
            return True, None  # Turnstile token 已生成 → 通过
        if not state.get("challenge"):
            return True, None  # 质询特征消失 → 通过
        if time.monotonic() >= deadline:
            return False, (
                f"质询求解超时（{max_wait_ms / 1000:.0f}s）——页面仍停留在质询页"
                f"（title={diag['title']!r} hasInput={diag['hasInput']} clicks={diag['clicks']}）"
            )
        # Turnstile widget iframe → 坐标点击勾选；无 iframe 时（mock/直接渲染场景）
        # 点击 .cf-turnstile 容器
        if not state.get("inputValue"):
            try:
                frame = next(
                    (f for f in page.frames if "challenges.cloudflare.com" in (f.url or "")),
                    None,
                )
                clicked = False
                if frame is not None:
                    await frame.click("body", timeout=3000)
                    clicked = True
                else:
                    loc = page.locator('iframe[src*="challenges.cloudflare.com"]')
                    if await loc.count() > 0:
                        await loc.first.click(timeout=3000)
                        clicked = True
                if not clicked:
                    cont = page.locator(".cf-turnstile")
                    if await cont.count() > 0:
                        await cont.first.click(timeout=3000)
                        clicked = True
                if clicked:
                    diag["clicks"] += 1
            except Exception:
                pass
        await asyncio.sleep(0.5)


async def post_navigate(page, post, max_wait_ms, diag):
    """post.mode="navigate"：页内表单提交（页面导航式 POST）→ 二次质询等待循环
    （渲染出的 Turnstile widget 自动点击）→ 最终页 HTML。返回 (postResult, err)"""
    action = str(post.get("action") or "")
    fields = form_fields_from_body(post.get("body") or "")
    try:
        await page.evaluate(POST_NAVIGATE_JS % (json.dumps(fields, ensure_ascii=True), json.dumps(action)))
    except Exception as e:
        return {"error": f"表单提交失败: {e}"[:300]}, str(e)[:200]
    # 等导航离开起始页且文档加载完成（避免在加载中的文档上误判"无质询"）
    start_url = page.url
    dl = time.monotonic() + 20
    while time.monotonic() < dl:
        try:
            if page.url != start_url:
                try:
                    rs = await page.evaluate("document.readyState")
                    if rs == "complete":
                        break
                except Exception:
                    break
        except Exception:
            break
        await asyncio.sleep(0.4)
    # 二次质询等待（Turnstile widget 等）
    deadline = time.monotonic() + max_wait_ms / 1000.0
    ok, err = await wait_challenge_clear(page, deadline, diag, max_wait_ms, prefix="post")
    await asyncio.sleep(1.0)
    try:
        html = await page.evaluate("document.documentElement.outerHTML")
    except Exception:
        html = await page.content()
    res = {"status": 200, "url": page.url, "html": html}
    if not ok:
        res["error"] = err
    return res, (err or "")


async def post_fetch(page, post, diag):
    """post.mode="fetch"（默认）：页内 fetch POST——响应按 charset 解码（GBK 支持）"""
    try:
        post_js = POST_FETCH_JS % json.dumps(
            {
                "action": str(post.get("action")),
                "body": str(post.get("body") or ""),
                "contentType": str(post.get("contentType") or "") or None,
                "charset": str(post.get("charset") or "") or None,
            },
            ensure_ascii=True,
        )
        result = await page.evaluate(post_js)
        if isinstance(result, dict):
            diag["postStatus"] = result.get("status")
            diag["postError"] = result.get("error")
        return result, (result.get("error") if isinstance(result, dict) else None)
    except Exception as e:
        return {"error": str(e)[:300]}, str(e)[:200]


async def drag_slider(page, x1, y1, x2, y2):
    """Playwright 可信鼠标拖拽（三次贝塞尔 + 随机噪声 + 微停——isTrusted=true）"""
    steps = 28 + random.randrange(25)
    pts = [(x1, y1)]
    ctrl1 = (x1 + (x2 - x1) * 0.4 + random.uniform(-10, 10), y1)
    ctrl2 = (x1 + (x2 - x1) * 0.6 + random.uniform(-10, 10), y2)
    for i in range(1, steps + 1):
        t = i / steps
        inv = 1.0 - t
        x = inv * inv * inv * x1 + 3 * inv * inv * t * ctrl1[0] + 3 * inv * t * t * ctrl2[0] + t * t * t * x2
        y = inv * inv * inv * y1 + 3 * inv * inv * t * ctrl1[1] + 3 * inv * t * t * ctrl2[1] + t * t * t * y2
        pts.append((x + random.uniform(-2, 2), y + random.uniform(-2, 2)))
    pts.append((x2, y2))
    try:
        await page.mouse.move(x1, y1)
        await page.mouse.down()
        for x, y in pts[1:]:
            await page.mouse.move(x, y, steps=1)
            await asyncio.sleep(0.008)
        await page.mouse.up()
        return True
    except Exception:
        try:
            await page.mouse.up()
        except Exception:
            pass
        return False


async def detect_and_drag_slider(page, diag):
    """检测滑块并可信拖拽；命中返回 True"""
    try:
        det = await page.evaluate(DETECT_CAPTCHA_JS)
    except Exception:
        return False
    if not det or det.get("kind") != "slider":
        return False
    bx = float(det.get("x") or 0)
    by = float(det.get("y") or 0)
    bw = float(det.get("w") or 40)
    track_w = float(det.get("trackW") or 300)
    start_x = bx + bw / 2.0
    start_y = by + 12.0
    dist = (track_w - bw) * (0.55 + random.random() * 0.35)
    end_x = bx + dist
    ok = await drag_slider(page, start_x, start_y, end_x, start_y)
    diag["sliderDrags"] = diag.get("sliderDrags", 0) + 1
    return ok


async def screenshot_clip(page, x, y, w, h):
    """区域截图 → PNG 字节"""
    try:
        return await page.screenshot(clip={"x": x, "y": y, "width": w, "height": h})
    except Exception:
        try:
            return await page.screenshot()
        except Exception:
            return None


async def extract_result(page, context, host):
    """提取最终 HTML + 站点 cookie + UA + Turnstile token + 最终 URL"""
    try:
        html = await page.evaluate("document.documentElement.outerHTML")
    except Exception:
        html = await page.content()
    ua = ""
    try:
        ua = await page.evaluate("navigator.userAgent") or ""
    except Exception:
        pass
    token = ""
    try:
        token = await page.evaluate(
            "(function(){var el=document.querySelector('[name=\"cf-turnstile-response\"]');"
            "return el&&el.value?el.value:'';})()"
        ) or ""
    except Exception:
        pass
    all_cookies = await context.cookies()
    site_cookies = cookies_for_host(all_cookies, host)
    return {"html": html, "cookies": site_cookies, "userAgent": ua, "turnstileToken": token, "url": page.url}


async def detect_captcha(page):
    """验证码检测（image/slider/click）→ dict 或 None"""
    try:
        return await page.evaluate(DETECT_CAPTCHA_JS)
    except Exception:
        return None


async def challenge_state(page):
    """质询状态求值 → dict（失败时给全 challenge=True 兜底）"""
    try:
        return await page.evaluate(CHALLENGE_STATE_JS)
    except Exception:
        return {"challenge": True, "hasInput": False, "inputValue": "", "title": "", "url": "", "bodyChildren": 0}


async def click_turnstile_widget(page, diag=None):
    """点击 Turnstile widget（challenges.cloudflare.com iframe 坐标点击；无 iframe 时
    点击 .cf-turnstile 容器）。返回是否执行了点击。"""
    try:
        frame = next(
            (f for f in page.frames if "challenges.cloudflare.com" in (f.url or "")),
            None,
        )
        if frame is not None:
            await frame.click("body", timeout=3000)
            if diag is not None:
                diag["clicks"] = diag.get("clicks", 0) + 1
            return True
        loc = page.locator('iframe[src*="challenges.cloudflare.com"]')
        if await loc.count() > 0:
            await loc.first.click(timeout=3000)
            if diag is not None:
                diag["clicks"] = diag.get("clicks", 0) + 1
            return True
        cont = page.locator(".cf-turnstile")
        if await cont.count() > 0:
            await cont.first.click(timeout=3000)
            if diag is not None:
                diag["clicks"] = diag.get("clicks", 0) + 1
            return True
    except Exception:
        pass
    return False


async def login_wait(page, session, deadline, diag, max_wait_ms):
    """登录等待循环（每 500ms 一轮，图片验证码优先于滑块优先于质询）：
    ① 图片验证码 → 截图返回 need_captcha（会话保留，两步回填）
    ② 滑块 → 自动可信拖拽（限频，避免死循环）
    ③ CF/Turnstile 质询 → 自动点击 widget，inputValue 非空 / challenge 消失 → ok
    ④ 超时 → timeout（关闭会话由调用方处理）

    返回 (status, result)：need_captcha / ok / timeout
    """
    start = time.monotonic()
    last_slider_at = 0.0
    while True:
        det = await detect_captcha(page)
        # ① 图片验证码（登录页表单内 / 提交后出现）→ 截图给前端回填
        if det and det.get("kind") == "image":
            x = float(det.get("x") or 0)
            y = float(det.get("y") or 0)
            w = float(det.get("w") or 0)
            h = float(det.get("h") or 0)
            if w >= 2 and h >= 2:
                png = await screenshot_clip(page, x, y, w, h)
                if png:
                    session["last_used"] = time.monotonic()
                    return (
                        "need_captcha",
                        {
                            "status": "need_captcha",
                            "sessionId": session["id"],
                            "captcha": {
                                "base64": base64.b64encode(png).decode("ascii"),
                                "x": x,
                                "y": y,
                                "w": w,
                                "h": h,
                            },
                            "diagnostics": diag,
                        },
                    )
        # ② 滑块 → 自动可信拖拽（限频 2s）
        if det and det.get("kind") == "slider":
            if time.monotonic() - last_slider_at > 2.0:
                await detect_and_drag_slider(page, diag)
                last_slider_at = time.monotonic()
            if time.monotonic() >= deadline:
                break
            await asyncio.sleep(0.5)
            continue
        # ③ 质询状态（CF/Turnstile）
        state = await challenge_state(page)
        diag["title"] = state.get("title", "")
        diag["hasInput"] = bool(state.get("hasInput"))
        diag["waitMs"] = int((time.monotonic() - start) * 1000)
        if state.get("inputValue") or not state.get("challenge"):
            await asyncio.sleep(1.0)
            session["last_used"] = time.monotonic()
            return ("ok", {"status": "ok", "sessionId": session["id"], "diagnostics": diag})
        # Turnstile widget 自动点击（input 值为空时）
        if not state.get("inputValue"):
            await click_turnstile_widget(page, diag)
        # ④ 超时
        if time.monotonic() >= deadline:
            break
        await asyncio.sleep(0.5)
    return (
        "timeout",
        {
            "status": "timeout",
            "error": f"登录等待超时（{max_wait_ms / 1000:.0f}s）——title={diag.get('title', '')!r}",
            "diagnostics": diag,
        },
    )


async def login_once(browser, url, username, password, cookies, user_agent, proxy, max_wait_ms):
    """登录第一步：填表+提交 → 等待循环（图片验证码/滑块/质询）→ 结果。"""
    host = host_of_url(url)
    diag = {"title": "", "waitMs": 0, "clicks": 0, "sliderDrags": 0, "url": url}
    session_id = uuid.uuid4().hex[:16]
    ctx = await new_context_with_ua(browser, user_agent, proxy)
    try:
        page = await ctx.new_page()
        await _inject_cookies_async(ctx, cookies, host)
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=min(max_wait_ms, 60000))
        except Exception as e:
            await ctx.close()
            return {"status": "error", "error": f"导航失败: {e}"[:300], "diagnostics": diag}
        # 填表（占位符为单引号包裹——replace 需匹配 'USERNAME'/'PASSWORD'）
        try:
            fill_js = FILL_FORM_JS.replace("'USERNAME'", json.dumps(str(username), ensure_ascii=True)) \
                                  .replace("'PASSWORD'", json.dumps(str(password), ensure_ascii=True))
            fill = await page.evaluate(fill_js)
        except Exception as e:
            fill = {"ok": False, "reason": f"evaluate error: {e}"[:200]}
        diag["fill"] = fill
        if not (isinstance(fill, dict) and fill.get("ok")):
            await ctx.close()
            return {
                "status": "error",
                "error": f"登录表单填写失败: {(fill or {}).get('reason', 'unknown')}",
                "diagnostics": diag,
            }
        # 会话先建立（图片验证码两步流需要保留页面）
        session = {
            "id": session_id,
            "ctx": ctx,
            "page": page,
            "host": host,
            "diag": diag,
            "last_used": time.monotonic(),
        }
        SESSIONS[session_id] = session
        deadline = time.monotonic() + max_wait_ms / 1000.0
        # 提交前先查图片验证码（很多站点验证码在表单内，需先回填再提交）
        pre = await detect_captcha(page)
        if pre and pre.get("kind") == "image":
            x = float(pre.get("x") or 0)
            y = float(pre.get("y") or 0)
            w = float(pre.get("w") or 0)
            h = float(pre.get("h") or 0)
            if w >= 2 and h >= 2:
                png = await screenshot_clip(page, x, y, w, h)
                if png:
                    return {
                        "status": "need_captcha",
                        "sessionId": session_id,
                        "captcha": {
                            "base64": base64.b64encode(png).decode("ascii"),
                            "x": x,
                            "y": y,
                            "w": w,
                            "h": h,
                        },
                        "diagnostics": diag,
                    }
        try:
            submit = await page.evaluate(SUBMIT_FORM_JS)
        except Exception:
            submit = {}
        diag["submit"] = submit
        status, result = await login_wait(page, session, deadline, diag, max_wait_ms)
        if status == "ok":
            ctx_keep = SESSIONS.pop(session_id, None)
            result.update(await extract_result(page, ctx, host))
            try:
                await ctx.close()
            except Exception:
                pass
            del ctx_keep
            return result
        if status == "need_captcha":
            return result
        # timeout/error → 关闭会话
        SESSIONS.pop(session_id, None)
        try:
            await ctx.close()
        except Exception:
            pass
        return result
    except Exception as e:
        SESSIONS.pop(session_id, None)
        try:
            await ctx.close()
        except Exception:
            pass
        return {"status": "error", "error": f"登录异常: {e}"[:300], "diagnostics": diag}


async def login_captcha_step(session_id, captcha_text, max_wait_ms):
    """登录第二步：回填验证码 → 重新提交 → 等待循环（可能再次 need_captcha）"""
    s = SESSIONS.get(session_id)
    if not s:
        return {"status": "error", "error": "会话不存在或已过期（请重新发起 /login）"}
    page = s["page"]
    diag = s["diag"]
    try:
        fill_js = FILL_CAPTCHA_JS.replace("'CAPTCHA'", json.dumps(str(captcha_text), ensure_ascii=True))
        r = await page.evaluate(fill_js)
    except Exception as e:
        r = {"ok": False, "reason": f"evaluate error: {e}"[:200]}
    if not (isinstance(r, dict) and r.get("ok")):
        return {"status": "error", "error": f"验证码填写失败: {(r or {}).get('reason', 'unknown')}"}
    try:
        submit = await page.evaluate(SUBMIT_FORM_JS)
    except Exception:
        submit = {}
    diag["submit"] = submit
    deadline = time.monotonic() + max_wait_ms / 1000.0
    status, result = await login_wait(page, s, deadline, diag, max_wait_ms)
    if status == "ok":
        ctx = s["ctx"]
        SESSIONS.pop(session_id, None)
        result.update(await extract_result(page, ctx, s["host"]))
        try:
            await ctx.close()
        except Exception:
            pass
        return result
    if status == "need_captcha":
        return result
    SESSIONS.pop(session_id, None)
    try:
        await s["ctx"].close()
    except Exception:
        pass
    return result


async def reap_sessions():
    """会话闲置回收：TTL 内无使用 → 关闭 context 移除"""
    while True:
        await asyncio.sleep(60)
        now = time.monotonic()
        stale = [sid for sid, s in SESSIONS.items() if now - s.get("last_used", 0) > SESSION_TTL_SEC]
        for sid in stale:
            s = SESSIONS.pop(sid, None)
            if s:
                try:
                    await s["ctx"].close()
                except Exception:
                    pass


async def handle_solve(reader, payload):
    """POST /solve：读 body → 求解 → JSON 响应"""
    url = str(payload.get("url") or "")
    if not url:
        return 400, {"error": "url 不能为空"}
    cookies = payload.get("cookies") or []
    max_wait_ms = int(payload.get("maxWaitMs") or DEFAULT_MAX_WAIT_MS)
    user_agent = str(payload.get("userAgent") or "") or None
    proxy = str(payload.get("proxy") or "") or None
    post = payload.get("post")
    if post is not None and not isinstance(post, dict):
        return 400, {"error": "post 必须是对象 {action, body, contentType?, charset?}"}
    try:
        browser = await get_browser()
    except Exception as e:
        return 502, {"error": f"camoufox 浏览器启动失败: {e}（请先执行 camoufox fetch 下载浏览器）"}
    result, diag = await solve_once(browser, url, cookies, max_wait_ms, user_agent, proxy, post)
    result["diagnostics"] = diag
    return 200, result


async def solve_once(browser, url, cookies, max_wait_ms, user_agent=None, proxy=None, post=None):
    """单次求解：新建指纹 context → 导航 → 质询等待循环 →（可选页内 POST）→ 结果/诊断"""
    host = host_of_url(url)
    diag = {"title": "", "hasInput": False, "url": url, "waitMs": 0, "clicks": 0}
    if user_agent:
        diag["userAgent"] = user_agent
    context = await new_context_with_ua(browser, user_agent, proxy)
    try:
        page = await context.new_page()
        # 书源既有 cookie 注入（domain 由目标主机推导）
        if cookies and host:
            try:
                await _inject_cookies_async(context, cookies, host)
            except Exception as e:
                diag["cookieError"] = str(e)[:200]
        # 导航（domcontentloaded 即可进入等待循环；导航失败 → 明确错误）
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=min(max_wait_ms, 60000))
        except Exception as e:
            return {"error": f"导航失败: {e}"}, diag
        # 登录页滑块（mode=browser 登录场景）→ 自动可信拖拽
        await detect_and_drag_slider(page, diag)
        # 质询等待循环（详见 wait_challenge_clear）
        deadline = time.monotonic() + max_wait_ms / 1000.0
        ok, err = await wait_challenge_clear(page, deadline, diag, max_wait_ms)
        if not ok:
            return {"error": err}, diag
        # 稳定等待（质询跳转后的重绘）
        await asyncio.sleep(1.0)
        # 可选页内 POST（搜索等表单链路——同源 cookie/referer 自动携带）
        post_result = None
        if post and isinstance(post, dict) and post.get("action"):
            if str(post.get("mode") or "") == "navigate":
                post_result, _ = await post_navigate(page, post, max_wait_ms, diag)
            else:
                post_result, _ = await post_fetch(page, post, diag)
        result = await extract_result(page, context, host)
        if post_result is not None:
            result["postResult"] = post_result
        return result, diag
    finally:
        try:
            await context.close()
        except Exception:
            pass


async def handle_slider(reader, payload):
    """POST /slider：加载（可选）→ 检测滑块 → 可信拖拽 → 结果"""
    try:
        browser = await get_browser()
    except Exception as e:
        return 502, {"error": f"camoufox 浏览器启动失败: {e}（请先执行 camoufox fetch 下载浏览器）"}
    url = str(payload.get("url") or "") or None
    cookies = payload.get("cookies") or []
    max_wait_ms = int(payload.get("maxWaitMs") or 30000)
    user_agent = str(payload.get("userAgent") or "") or None
    proxy = str(payload.get("proxy") or "") or None
    host = host_of_url(url) if url else ""
    diag = {"sliderDrags": 0, "url": url or ""}
    ctx = await new_context_with_ua(browser, user_agent, proxy)
    try:
        page = await ctx.new_page()
        if cookies and host:
            try:
                await _inject_cookies_async(ctx, cookies, host)
            except Exception:
                pass
        if url:
            try:
                await page.goto(url, wait_until="domcontentloaded", timeout=min(max_wait_ms, 60000))
            except Exception as e:
                return 200, {"ok": False, "error": f"导航失败: {e}"[:300], "diagnostics": diag}
        deadline = time.monotonic() + max_wait_ms / 1000.0
        start = time.monotonic()
        while True:
            hit = await detect_and_drag_slider(page, diag)
            if hit:
                await asyncio.sleep(2.0)
                # 复查滑块是否消失
                try:
                    det = await page.evaluate(DETECT_CAPTCHA_JS)
                except Exception:
                    det = None
                if not det or det.get("kind") != "slider":
                    return 200, {"ok": True, "diagnostics": diag}
                return 200, {"ok": False, "error": "拖拽后滑块仍在", "diagnostics": diag}
            if time.monotonic() >= deadline:
                return 200, {"ok": False, "error": "未检测到滑块（超时）", "diagnostics": diag}
            await asyncio.sleep(0.5)
    finally:
        try:
            await ctx.close()
        except Exception:
            pass


async def handle_screenshot(reader, payload):
    """POST /screenshot：加载（可选）→ 按 selector/clip/整页截图 → base64"""
    try:
        browser = await get_browser()
    except Exception as e:
        return 502, {"error": f"camoufox 浏览器启动失败: {e}（请先执行 camoufox fetch 下载浏览器）"}
    url = str(payload.get("url") or "") or None
    selector = str(payload.get("selector") or "") or None
    clip = payload.get("clip")
    cookies = payload.get("cookies") or []
    user_agent = str(payload.get("userAgent") or "") or None
    proxy = str(payload.get("proxy") or "") or None
    host = host_of_url(url) if url else ""
    ctx = await new_context_with_ua(browser, user_agent, proxy)
    try:
        page = await ctx.new_page()
        if cookies and host:
            try:
                await _inject_cookies_async(ctx, cookies, host)
            except Exception:
                pass
        if url:
            try:
                await page.goto(url, wait_until="domcontentloaded", timeout=60000)
            except Exception as e:
                return 200, {"error": f"导航失败: {e}"[:300]}
        png = None
        if selector:
            loc = page.locator(selector)
            try:
                png = await loc.screenshot(timeout=10000)
            except Exception:
                try:
                    box = await loc.bounding_box()
                    if box:
                        png = await page.screenshot(clip=box)
                except Exception:
                    png = None
        elif clip and isinstance(clip, dict):
            try:
                png = await page.screenshot(
                    clip={
                        "x": float(clip.get("x") or 0),
                        "y": float(clip.get("y") or 0),
                        "width": float(clip.get("w") or clip.get("width") or 0),
                        "height": float(clip.get("h") or clip.get("height") or 0),
                    }
                )
            except Exception:
                png = None
        else:
            try:
                png = await page.screenshot()
            except Exception:
                png = None
        if not png:
            return 200, {"error": "截图失败"}
        return 200, {"base64": base64.b64encode(png).decode("ascii")}
    finally:
        try:
            await ctx.close()
        except Exception:
            pass


async def handle_probe(reader, payload):
    """POST /probe：导航 → 检测验证码类型（image/slider/click/none）+ 图片截图"""
    try:
        browser = await get_browser()
    except Exception as e:
        return 502, {"error": f"camoufox 浏览器启动失败: {e}（请先执行 camoufox fetch 下载浏览器）"}
    url = str(payload.get("url") or "")
    if not url:
        return 400, {"error": "url 不能为空"}
    cookies = payload.get("cookies") or []
    user_agent = str(payload.get("userAgent") or "") or None
    proxy = str(payload.get("proxy") or "") or None
    host = host_of_url(url)
    ctx = await new_context_with_ua(browser, user_agent, proxy)
    try:
        page = await ctx.new_page()
        if cookies and host:
            try:
                await _inject_cookies_async(ctx, cookies, host)
            except Exception:
                pass
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60000)
        except Exception as e:
            return 200, {"error": f"导航失败: {e}"[:300]}
        await asyncio.sleep(1.0)
        det = await detect_captcha(page)
        page_url = page.url
        if not det or det.get("kind") not in ("image", "slider", "click"):
            return 200, {"captchaType": "none", "pageUrl": page_url, "message": "未检测到验证码"}
        kind = det.get("kind")
        if kind == "image":
            x = float(det.get("x") or 0)
            y = float(det.get("y") or 0)
            w = float(det.get("w") or 0)
            h = float(det.get("h") or 0)
            png = await screenshot_clip(page, x, y, w, h) if w >= 2 and h >= 2 else None
            return 200, {
                "captchaType": "image",
                "pageUrl": page_url,
                "captcha": {
                    "base64": base64.b64encode(png).decode("ascii") if png else "",
                    "x": x, "y": y, "w": w, "h": h,
                },
            }
        return 200, {"captchaType": kind, "pageUrl": page_url,
                     "message": "滑块验证码（请重新调用登录自动处理）" if kind == "slider"
                     else "点选类验证码（无法自动识别）"}
    finally:
        try:
            await ctx.close()
        except Exception:
            pass


async def handle_client(reader, writer):
    """极简 HTTP/1.1（单请求/连接——reqwest 客户端兼容）"""
    try:
        request_line = await asyncio.wait_for(reader.readline(), timeout=10)
        if not request_line:
            return
        parts = request_line.decode("latin-1", "replace").split()
        if len(parts) < 2:
            return
        method, path = parts[0].upper(), parts[1]
        payload = {}
        if method == "POST":
            payload = await read_json_body(reader)
        if method == "GET" and path in ("/health", "/health/"):
            status, payload = 200, {
                "ok": True,
                "camoufoxVersion": "0.5.4",
                "browserReady": _browser_ready,
                "port": PORT,
                "sessions": len(SESSIONS),
            }
        elif method == "POST" and path in ("/solve", "/solve/"):
            status, payload = await handle_solve(reader, payload)
        elif method == "POST" and path in ("/login", "/login/"):
            status, payload = await handle_login(reader, payload)
        elif method == "POST" and path in ("/login/captcha", "/login/captcha/"):
            status, payload = await handle_login_captcha(reader, payload)
        elif method == "POST" and path in ("/login/close", "/login/close/"):
            status, payload = await handle_login_close(reader, payload)
        elif method == "POST" and path in ("/slider", "/slider/"):
            status, payload = await handle_slider(reader, payload)
        elif method == "POST" and path in ("/screenshot", "/screenshot/"):
            status, payload = await handle_screenshot(reader, payload)
        elif method == "POST" and path in ("/probe", "/probe/"):
            status, payload = await handle_probe(reader, payload)
        else:
            status, payload = 404, {"error": "not found（GET /health | POST /solve | /login | /login/captcha | /login/close | /slider | /screenshot | /probe）"}
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        writer.write(
            (
                f"HTTP/1.1 {status} OK\r\n"
                "Content-Type: application/json; charset=utf-8\r\n"
                f"Content-Length: {len(data)}\r\n"
                "Connection: close\r\n"
                "\r\n"
            ).encode("latin-1")
            + data
        )
    except Exception:
        pass
    finally:
        try:
            await writer.drain()
        except Exception:
            pass
        writer.close()


async def read_json_body(reader):
    """读 Content-Length body → dict（解析失败返回 {}）"""
    try:
        length = 0
        while True:
            line = await asyncio.wait_for(reader.readline(), timeout=10)
            if not line or line in (b"\r\n", b"\n"):
                break
            low = line.strip().lower()
            if low.startswith(b"content-length:") and b":" in line:
                length = int(line.split(b":", 1)[1].strip() or 0)
        body = await asyncio.wait_for(reader.readexactly(length), timeout=10) if length else b""
        return json.loads(body.decode("utf-8", "replace")) if body else {}
    except Exception:
        return {}


async def handle_login(reader, payload):
    """POST /login：表单登录（填表+提交+滑块/质询自动处理；图片验证码两步流）"""
    url = str(payload.get("url") or "")
    if not url:
        return 400, {"error": "url 不能为空"}
    username = str(payload.get("username") or "")
    password = str(payload.get("password") or "")
    if not username or not password:
        return 400, {"error": "username/password 不能为空"}
    cookies = payload.get("cookies") or []
    max_wait_ms = int(payload.get("maxWaitMs") or LOGIN_MAX_WAIT_MS)
    user_agent = str(payload.get("userAgent") or "") or None
    proxy = str(payload.get("proxy") or "") or None
    try:
        browser = await get_browser()
    except Exception as e:
        return 502, {"error": f"camoufox 浏览器启动失败: {e}（请先执行 camoufox fetch 下载浏览器）"}
    result = await login_once(browser, url, username, password, cookies, user_agent, proxy, max_wait_ms)
    return 200, result


async def handle_login_captcha(reader, payload):
    """POST /login/captcha：两步流第二步——回填验证码 → 重新提交 → 等待"""
    session_id = str(payload.get("sessionId") or "")
    captcha = str(payload.get("captcha") or "")
    if not session_id or not captcha:
        return 400, {"error": "sessionId/captcha 不能为空"}
    max_wait_ms = int(payload.get("maxWaitMs") or LOGIN_MAX_WAIT_MS)
    result = await login_captcha_step(session_id, captcha, max_wait_ms)
    return 200, result


async def handle_login_close(reader, payload):
    """POST /login/close：关闭登录会话"""
    session_id = str(payload.get("sessionId") or "")
    if not session_id:
        return 400, {"error": "sessionId 不能为空"}
    s = SESSIONS.pop(session_id, None)
    if s:
        try:
            await s["ctx"].close()
        except Exception:
            pass
        return 200, {"ok": True}
    return 200, {"ok": False, "error": "会话不存在"}


async def amain(host, port):
    server = await asyncio.start_server(handle_client, host, port)
    asyncio.create_task(reap_sessions())
    print(f"CAMOUFOX_SOLVER listening on {host}:{port}（camoufox 常驻浏览器惰性启动）", flush=True)
    async with server:
        await server.serve_forever()


def main():
    ap = argparse.ArgumentParser(description="camoufox 验证码/登录 HTTP 服务")
    ap.add_argument("--port", type=int, default=PORT)
    ap.add_argument("--host", default="127.0.0.1")
    args = ap.parse_args()
    try:
        asyncio.run(amain(args.host, args.port))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
