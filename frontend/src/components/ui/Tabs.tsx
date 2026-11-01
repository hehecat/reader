import * as TabsPrimitive from "@radix-ui/react-tabs";
import * as React from "react";

import { cn } from "./cn";

export type TabsProps = React.ComponentProps<typeof TabsPrimitive.Root>;
export type TabsListProps = React.ComponentProps<typeof TabsPrimitive.List>;
export type TabsTriggerProps = React.ComponentProps<typeof TabsPrimitive.Trigger>;
export type TabsContentProps = React.ComponentProps<typeof TabsPrimitive.Content>;

/** 选项卡根节点: value / onValueChange 受控, 方向键切换由 Radix 处理. */
export const Tabs = TabsPrimitive.Root;

/** 选项卡轨道: 胶囊底槽, 窄屏可横向滚动. */
export function TabsList({ className, ...props }: TabsListProps) {
  return (
    <TabsPrimitive.List
      className={cn(
        "inline-flex h-10 max-w-full items-center gap-1 overflow-x-auto rounded-xl bg-surface-muted p-1",
        className,
      )}
      {...props}
    />
  );
}

/** 选项卡按钮: 选中项浮起为背景色胶囊. */
export function TabsTrigger({ className, ...props }: TabsTriggerProps) {
  return (
    <TabsPrimitive.Trigger
      className={cn(
        "inline-flex shrink-0 cursor-pointer items-center justify-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium whitespace-nowrap text-muted-foreground outline-none transition duration-150 ease-out hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
        className,
      )}
      {...props}
    />
  );
}

/** 选项卡内容面板: 只在选中时挂载. */
export function TabsContent({ className, ...props }: TabsContentProps) {
  return (
    <TabsPrimitive.Content
      className={cn("mt-3 outline-none focus-visible:ring-2 focus-visible:ring-ring", className)}
      {...props}
    />
  );
}
