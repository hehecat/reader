/**
 * 正文远程图片的「高度预留」: 给每张 <img> 套一个定比例的容器 (.reader-figure),
 * 图片解码完成前容器就占住了位置 —— 否则图一加载完, 下面的正文会被整体顶下去 (CLS).
 *
 * 比例来源: 清洗后的 HTML 里没有宽高属性 (DOMPurify 的 ALLOWED_ATTR 只留 src/alt/style/class),
 * 所以加载前用 CSS 的占位比例 (3 / 2), 图片 load 后按 naturalWidth/naturalHeight 校正成真实比例.
 * 正文图片是 loading="lazy" 的, load 发生在图片接近视口时 (即视口下沿), 这次校正顶到的都是
 * 还没读到的内容, 不会把正在读的行顶走.
 *
 * 只动 DOM 结构, 不动清洗后的 HTML 字符串 —— 段落 pos 是按清洗后 HTML 的字符偏移算的
 * (见 buildParagraphs), 往里塞标记会让所有后续段落的 pos 漂移, 进度恢复就跟着偏.
 */

/** 图片容器类名 (样式在 globals.css) */
const FIGURE_CLASS = "reader-figure";

/**
 * 给容器内所有图片套上定比例容器; 返回清理函数 (移除事件监听).
 * 幂等: 已经套过的图片会被跳过, 重复调用不会套两层.
 */
export function decorateReaderImages(host: HTMLElement): () => void {
  const cleanups: Array<() => void> = [];
  for (const img of host.querySelectorAll<HTMLImageElement>("img")) {
    const parent = img.parentElement;
    if (parent === null || parent.classList.contains(FIGURE_CLASS)) {
      continue;
    }
    const figure = document.createElement("span");
    figure.className = FIGURE_CLASS;
    parent.insertBefore(figure, img);
    figure.appendChild(img);
    const settle = (): void => {
      figure.dataset.loaded = "true";
      if (img.naturalWidth > 0 && img.naturalHeight > 0) {
        figure.style.setProperty("--reader-img-ratio", `${img.naturalWidth} / ${img.naturalHeight}`);
      }
    };
    if (img.complete) {
      // 已经解码完 (内存缓存/服务端秒回): 直接定比例, 不用等 load
      settle();
      continue;
    }
    img.addEventListener("load", settle, { once: true });
    cleanups.push(() => img.removeEventListener("load", settle));
    // 加载失败: 撤掉预留框, 别在正文里留一块空白 (失败图本身就是坏内容)
    const onError = (): void => {
      figure.dataset.failed = "true";
    };
    img.addEventListener("error", onError, { once: true });
    cleanups.push(() => img.removeEventListener("error", onError));
  }
  return () => {
    for (const cleanup of cleanups) {
      cleanup();
    }
  };
}
