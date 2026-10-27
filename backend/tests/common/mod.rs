//! 集成测试公共工具（P1）
//!
//! 本机 mock 站点（127.0.0.1）需要两处放行（仅测试进程可设置，生产无入口）：
//! 1. **爬虫/求解入口 SSRF 校验**（P1 全覆盖）：`crawler::validate_public_target`
//!    拒绝私网/回环目标——测试经公开静态 `SSRF_ALLOW_PRIVATE` 放行（RAII 守卫）。
//! 2. **obscura 内网导航**：应用已不再传 `--allow-private-network`（P1 收紧，
//!    obscura 默认禁 RFC1918）——mock 类测试需自行以
//!    `obscura serve --allow-private-network` 启动浏览器并经
//!    `READER_OBSCURA_URL=ws://127.0.0.1:<port>/devtools/browser` 连接；
//!    未配置时相关测试跳过（is_browser_available 为 false）。

use std::sync::atomic::Ordering;

/// RAII 守卫：临时放行 SSRF 私网校验（swap 保存旧值，Drop 恢复——嵌套安全）
///
/// 同时翻 `SSRF_ALLOW_PRIVATE`（图片/TTS 等回源）与 `SOURCE_ALLOW_PRIVATE`
/// （书源抓取，`ed2fbcd` 起独立收紧）——mock 站点的书源抓取只认后者。
pub struct PrivateNetGuard {
    prev: bool,
    prev_source: bool,
}

impl PrivateNetGuard {
    /// 放行私网/回环目标（仅测试进程可设置该静态；生产代码无任何入口）
    pub fn on() -> PrivateNetGuard {
        let prev = reader_dev::service::crawler::SSRF_ALLOW_PRIVATE.swap(true, Ordering::Relaxed);
        let prev_source =
            reader_dev::service::crawler::SOURCE_ALLOW_PRIVATE.swap(true, Ordering::Relaxed);
        PrivateNetGuard { prev, prev_source }
    }
}

impl Drop for PrivateNetGuard {
    fn drop(&mut self) {
        reader_dev::service::crawler::SSRF_ALLOW_PRIVATE.store(self.prev, Ordering::Relaxed);
        reader_dev::service::crawler::SOURCE_ALLOW_PRIVATE
            .store(self.prev_source, Ordering::Relaxed);
    }
}
