import { motion, AnimatePresence, useReducedMotion, type Transition } from "motion/react";
import * as React from "react";
import { cn } from "@/lib/cn";

export const SPRING_SNAPPY: Transition = {
  type: "spring",
  stiffness: 460,
  damping: 36,
  mass: 0.78,
};

export const SPRING_SHEET: Transition = {
  type: "spring",
  stiffness: 380,
  damping: 36,
  mass: 0.85,
};

export const SPRING_SILK: Transition = {
  type: "spring",
  stiffness: 300,
  damping: 32,
  mass: 0.9,
};

export const SPRING_POP: Transition = {
  type: "spring",
  stiffness: 500,
  damping: 34,
  mass: 0.72,
};

/** 按压回弹：极短促的触感反馈（按钮 / 图标按钮 / Tab） */
export const SPRING_PRESS: Transition = {
  type: "spring",
  stiffness: 720,
  damping: 30,
  mass: 0.5,
};

/** 滑块 / 开关阻尼：带轻微过冲的液态手感 */
export const SPRING_LIQUID: Transition = {
  type: "spring",
  stiffness: 540,
  damping: 30,
  mass: 0.7,
};

/** 宣纸展开曲线 (easeOutQuint) */
export const EASE_PAPER: [number, number, number, number] = [0.22, 1, 0.36, 1];

export { motion, AnimatePresence, useReducedMotion };

/** 订阅媒体查询（SSR 安全），供浮层在桌面 / 移动端切换形态 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = React.useState(
    () => typeof window !== "undefined" && window.matchMedia(query).matches,
  );
  React.useEffect(() => {
    const mq = window.matchMedia(query);
    const onChange = () => setMatches(mq.matches);
    onChange();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

export function PageEnter({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  const reduced = useReducedMotion();
  return (
    <motion.div
      initial={reduced ? false : { opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3, ease: EASE_PAPER }}
      className={cn("min-h-0", className)}
    >
      {children}
    </motion.div>
  );
}
