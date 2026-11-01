import * as SwitchPrimitive from "@radix-ui/react-switch";
import * as React from "react";

import { cn } from "./cn";

export type SwitchProps = React.ComponentProps<typeof SwitchPrimitive.Root> & {
  /** 轨道尺寸档位, 默认 md(24x44) */
  size?: "sm" | "md";
};

const trackClass: Record<"sm" | "md", string> = {
  sm: "h-5 w-9",
  md: "h-6 w-11",
};

const thumbClass: Record<"sm" | "md", string> = {
  sm: "size-4 data-[state=checked]:translate-x-4",
  md: "size-5 data-[state=checked]:translate-x-5",
};

/** 开关: checked / onCheckedChange 受控, 选中态轨道转 accent, 滑块用 transform 平移. */
export function Switch({ size = "md", className, ...props }: SwitchProps) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "inline-flex shrink-0 cursor-pointer items-center rounded-full border-2 border-border bg-surface-muted outline-none transition duration-150 ease-out focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background active:scale-97 data-[state=checked]:border-transparent data-[state=checked]:bg-accent disabled:pointer-events-none disabled:opacity-50",
        trackClass[size],
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        className={cn(
          "pointer-events-none block rounded-full bg-background shadow-sm transition-transform duration-150 ease-out",
          thumbClass[size],
        )}
      />
    </SwitchPrimitive.Root>
  );
}
