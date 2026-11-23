//! SSRF 防护（入口 URL 与重定向跳转目标的公网地址校验）

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Mutex;

/// 测试钩子：允许私网/回环回源目标（仅测试代码设置；生产恒为 false，或由 `SSRF_ALLOW_PRIVATE=1` 开启）。
pub static SSRF_ALLOW_PRIVATE: AtomicBool = AtomicBool::new(false);

/// 书源抓取专用放行开关（`READER_ALLOW_PRIVATE_SOURCE=1`）：独立于全局
/// `SSRF_ALLOW_PRIVATE`，默认关——书源规则(search/toc/正文/换源/订阅)不可访问
/// 私网/回环地址，即使全局已为内网 TTS/本地同步放行。
pub static SOURCE_ALLOW_PRIVATE: AtomicBool = AtomicBool::new(false);

/// 测试互斥锁：所有读写 `SSRF_ALLOW_PRIVATE` 的测试持同一把锁（串行），
/// 避免并行测试互相干扰（放行态与拦截态断言互斥）。
#[cfg(test)]
static SSRF_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 测试用 RAII 守卫：持全局互斥锁并把两个私网放行开关同时置为 `allow`；Drop 时恢复原值。
///
/// 两个开关必须一起翻：图片/TTS 等回源只看 `SSRF_ALLOW_PRIVATE`，而书源抓取
/// (search/toc/正文/换源/订阅) 只认 `SOURCE_ALLOW_PRIVATE`（`ed2fbcd` 起独立收紧）。
/// mock 站点绑定 127.0.0.1 的测试两条路径都会走到，只翻前者就会出现
/// 「图片抓得到、书源抓不到」的假失败。
///
/// 用法：`let _g = ssrf_allow_private_guard(true);`（mock 服务器绑定 127.0.0.1 的测试）；
/// 拦截断言测试用 `ssrf_allow_private_guard(false)` 持锁确保无并发放行。
#[cfg(test)]
pub struct SsrfAllowGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: bool,
    prev_source: bool,
}

/// 取测试守卫（见 [`SsrfAllowGuard`]）
#[cfg(test)]
pub fn ssrf_allow_private_guard(allow: bool) -> SsrfAllowGuard {
    let _lock = SSRF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = SSRF_ALLOW_PRIVATE.swap(allow, Ordering::Relaxed);
    let prev_source = SOURCE_ALLOW_PRIVATE.swap(allow, Ordering::Relaxed);
    SsrfAllowGuard {
        _lock,
        prev,
        prev_source,
    }
}

#[cfg(test)]
impl Drop for SsrfAllowGuard {
    fn drop(&mut self) {
        SSRF_ALLOW_PRIVATE.store(self.prev, Ordering::Relaxed);
        SOURCE_ALLOW_PRIVATE.store(self.prev_source, Ordering::Relaxed);
    }
}

/// 目标 IP 是否为应拦截的私网/回环/链路本地地址：
/// IPv4：127.0.0.0/8（回环）、10/8、172.16/12、192.168/16（私网）、169.254/16（链路本地）、
/// 0.0.0.0（未指定）、255.255.255.255（广播）；
/// IPv6：::1（回环）、fc00::/7（ULA）、fe80::/10（链路本地）、::（未指定）、
/// IPv4 映射地址（::ffff:a.b.c.d）递归按 IPv4 判定。
pub fn is_private_target_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private_target_ip(std::net::IpAddr::V4(v4)))
        }
    }
}

/// 重定向跳转目标校验（同步版——reqwest redirect `Policy::custom` 闭包内调用；
/// 语义与 [`validate_public_target`] 一致：字面 IP 直接判定、域名解析后逐个 IP 校验、
/// 私网/回环/链路本地/未指定/广播一律拒绝、解析失败拒绝；测试钩子
/// `SSRF_ALLOW_PRIVATE` 放行态同样生效）。
pub fn validate_redirect_target(url: &str) -> Result<()> {
    if SSRF_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    validate_redirect_target_strict(url)
}

/// 书源抓取专用重定向校验：只受 `SOURCE_ALLOW_PRIVATE` 控制
pub fn validate_source_redirect_target(url: &str) -> Result<()> {
    if SOURCE_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    validate_redirect_target_strict(url)
}

/// 严格版（不看任何放行开关）
fn validate_redirect_target_strict(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("重定向目标 URL 非法: {e}"))?;
    // 字面 IP 快速路径（不经 DNS——Host::Ipv6 直接判回环，不依赖系统 IPv6 支持）
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V4(ip)) {
                anyhow::bail!("重定向目标为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Ipv6(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V6(ip)) {
                anyhow::bail!("重定向目标为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Domain(_)) => {}
        None => anyhow::bail!("重定向目标缺少主机名"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("重定向目标缺少主机名"))?;
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!("重定向目标 localhost 为回环地址，已拦截");
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .map_err(|e| anyhow!("重定向目标域名解析失败（{host}）: {e}"))?;
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_private_target_ip(addr.ip()) {
            anyhow::bail!(
                "重定向目标域名 {host} 解析到内网/回环地址（{}），已拦截",
                addr.ip()
            );
        }
    }
    if !resolved_any {
        anyhow::bail!("重定向目标域名 {host} 无可用地址");
    }
    Ok(())
}

/// 校验回源目标为公网地址（SSRF 防护，M1）：
/// - 字面 IP：直接判定（回环/私网/链路本地/未指定/广播一律拒绝）；
/// - 域名：DNS 解析后逐个 IP 校验（任一解析到私网即拒绝）；localhost 直接拒绝；
/// - 解析失败 / 无地址 → 拒绝。
///
/// 供 fetch_image 每跳调用（含重定向目标）——/assets/proxy 非 secure 模式同样生效。
pub async fn validate_public_target(url: &str) -> Result<()> {
    if SSRF_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    validate_target_strict(url).await
}

/// 书源抓取入口校验：只受 `SOURCE_ALLOW_PRIVATE` 控制（默认拦截私网/回环）
pub async fn validate_source_target(url: &str) -> Result<()> {
    if SOURCE_ALLOW_PRIVATE.load(Ordering::Relaxed) {
        return Ok(());
    }
    validate_target_strict(url).await
}

/// 严格版（不看任何放行开关）
async fn validate_target_strict(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("目标 URL 非法: {e}"))?;
    // 字面 IP 快速路径（不经 DNS——Host::Ipv6 直接判回环，不依赖系统 IPv6 支持）
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V4(ip)) {
                anyhow::bail!("目标地址为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Ipv6(ip)) => {
            if is_private_target_ip(std::net::IpAddr::V6(ip)) {
                anyhow::bail!("目标地址为内网/回环地址（{ip}），已拦截");
            }
            return Ok(());
        }
        Some(url::Host::Domain(_)) => {}
        None => anyhow::bail!("目标 URL 缺少主机名"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("目标 URL 缺少主机名"))?;
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!("目标地址 localhost 为回环地址，已拦截");
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let mut resolved_any = false;
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow!("目标域名解析失败（{host}）: {e}"))?;
    for addr in addrs {
        resolved_any = true;
        if is_private_target_ip(addr.ip()) {
            anyhow::bail!(
                "目标域名 {host} 解析到内网/回环地址（{}），已拦截",
                addr.ip()
            );
        }
    }
    if !resolved_any {
        anyhow::bail!("目标域名 {host} 无可用地址");
    }
    Ok(())
}
