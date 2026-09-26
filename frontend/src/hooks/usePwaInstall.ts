import * as React from "react";

/** PWA 安装状态: 可安装/已安装/需 iOS 手动添加到主屏 */
export interface UsePwaInstallResult {
  /** 浏览器提供了安装提示(可调起原生安装弹窗) */
  canInstall: boolean;
  /** 已在独立窗口运行(装到桌面/主屏后打开) */
  installed: boolean;
  /** iOS Safari: 无编程安装接口, 只能引导「分享 → 添加到主屏幕」 */
  needsIosHint: boolean;
  /** 调起安装弹窗; 不可安装时无操作 */
  install: () => void;
}

type BeforeInstallPromptEvent = Event & {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
};

/** 是否以「独立应用」方式运行(已安装或在独立窗口打开) */
function detectStandalone(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  const iosStandalone = (window.navigator as { standalone?: boolean }).standalone === true;
  return window.matchMedia("(display-mode: standalone)").matches || iosStandalone;
}

function detectIos(): boolean {
  if (typeof navigator === "undefined") {
    return false;
  }
  return /iPad|iPhone|iPod/.test(navigator.userAgent);
}

/**
 * PWA 安装入口状态: 捕获 beforeinstallprompt(Chrome/Edge/Android) 供设置页调起,
 * 仅负责安装入口; 离线能力由 public/sw.js 提供(正文来自书源 API, 不在离线范围),
 * 并区分「已安装」与「iOS 需手动添加」两种情形.
 */
export function usePwaInstall(): UsePwaInstallResult {
  const [deferred, setDeferred] = React.useState<BeforeInstallPromptEvent | null>(null);
  const [installed, setInstalled] = React.useState(detectStandalone);

  React.useEffect(() => {
    const onPrompt = (event: Event) => {
      // 阻止浏览器自带的迷你信息条, 由设置页在合适时机调起
      event.preventDefault();
      setDeferred(event as BeforeInstallPromptEvent);
    };
    const onInstalled = () => {
      setDeferred(null);
      setInstalled(true);
    };
    window.addEventListener("beforeinstallprompt", onPrompt);
    window.addEventListener("appinstalled", onInstalled);
    const media = window.matchMedia("(display-mode: standalone)");
    const onModeChange = () => setInstalled(detectStandalone());
    media.addEventListener("change", onModeChange);
    return () => {
      window.removeEventListener("beforeinstallprompt", onPrompt);
      window.removeEventListener("appinstalled", onInstalled);
      media.removeEventListener("change", onModeChange);
    };
  }, []);

  const install = React.useCallback(() => {
    if (!deferred) {
      return;
    }
    void deferred
      .prompt()
      .then(() => deferred.userChoice)
      .then((choice) => {
        if (choice.outcome === "accepted") {
          setInstalled(true);
          setDeferred(null);
        }
      })
      .catch(() => undefined);
  }, [deferred]);

  return {
    canInstall: deferred !== null,
    installed,
    needsIosHint: !installed && !deferred && detectIos(),
    install,
  };
}
