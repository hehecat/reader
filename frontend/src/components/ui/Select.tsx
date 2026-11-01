import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronDown, ChevronUp } from "lucide-react";
import * as React from "react";

import { cn } from "./cn";

import "./motion.css";

export type SelectProps = React.ComponentProps<typeof SelectPrimitive.Root>;
export type SelectValueProps = React.ComponentProps<typeof SelectPrimitive.Value>;
export type SelectGroupProps = React.ComponentProps<typeof SelectPrimitive.Group>;
export type SelectItemProps = React.ComponentProps<typeof SelectPrimitive.Item>;
export type SelectContentProps = React.ComponentProps<typeof SelectPrimitive.Content>;

/** 选择器根节点: value / onValueChange 受控, 支持键盘导航与类型ahead. */
export const Select = SelectPrimitive.Root;

/** 选择器当前值展示, 未选中时显示 placeholder. */
export const SelectValue = SelectPrimitive.Value;

/** 选择器分组(与 SelectLabel 搭配). */
export const SelectGroup = SelectPrimitive.Group;

export interface SelectTriggerProps extends React.ComponentProps<typeof SelectPrimitive.Trigger> {
  /** 高度档位, 默认 md(40px) */
  size?: "sm" | "md";
  /** 校验失败态: 边框转 danger */
  invalid?: boolean;
}

/** 选择器触发器: 边框输入框外观 + 右侧下拉箭头, 展开时高亮. */
export function SelectTrigger({
  size = "md",
  invalid = false,
  className,
  children,
  ...props
}: SelectTriggerProps) {
  return (
    <SelectPrimitive.Trigger
      className={cn(
        "flex w-full cursor-pointer items-center justify-between gap-2 rounded-lg border bg-background text-foreground outline-none transition duration-150 ease-out data-[placeholder]:text-muted-foreground data-[state=open]:border-accent data-[state=open]:ring-2 data-[state=open]:ring-ring/70 focus-visible:border-accent focus-visible:ring-2 focus-visible:ring-ring/70 disabled:pointer-events-none disabled:opacity-50 [&_svg]:shrink-0",
        invalid ? "border-danger" : "border-border",
        size === "sm" ? "h-8 px-2.5 text-xs" : "h-10 px-3 text-sm",
        className,
      )}
      {...props}
    >
      {children}
      <SelectPrimitive.Icon asChild>
        <ChevronDown className="size-4 text-muted-foreground" />
      </SelectPrimitive.Icon>
    </SelectPrimitive.Trigger>
  );
}

/** 选择器浮层: 至少与触发器同宽, 超出高度内部滚动并带上下的滚动按钮. */
export function SelectContent({
  className,
  children,
  position = "popper",
  ...props
}: SelectContentProps) {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Content
        position={position}
        sideOffset={6}
        collisionPadding={8}
        className={cn(
          "ui-anchor ui-select-content relative z-100 max-h-72 overflow-hidden rounded-xl border border-border bg-surface text-foreground shadow-xl",
          className,
        )}
        {...props}
      >
        <SelectScrollUpButton />
        <SelectPrimitive.Viewport className="p-1.5">{children}</SelectPrimitive.Viewport>
        <SelectScrollDownButton />
      </SelectPrimitive.Content>
    </SelectPrimitive.Portal>
  );
}

/** 选项: 悬浮/键盘高亮, 选中项右侧显示 Check. */
export function SelectItem({ className, children, ...props }: SelectItemProps) {
  return (
    <SelectPrimitive.Item
      className={cn(
        "relative flex w-full cursor-pointer select-none items-center rounded-lg py-1.5 pr-8 pl-2.5 text-sm outline-none transition duration-150 ease-out data-[highlighted]:bg-surface-muted data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
        className,
      )}
      {...props}
    >
      <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
      <span className="absolute right-2.5 flex size-4 items-center justify-center">
        <SelectPrimitive.ItemIndicator>
          <Check className="size-4 text-accent" />
        </SelectPrimitive.ItemIndicator>
      </span>
    </SelectPrimitive.Item>
  );
}

/** 分组标题. */
export function SelectLabel({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Label>) {
  return (
    <SelectPrimitive.Label
      className={cn("px-2.5 py-1.5 text-xs font-medium text-muted-foreground", className)}
      {...props}
    />
  );
}

/** 选项分隔线. */
export function SelectSeparator({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Separator>) {
  return (
    <SelectPrimitive.Separator
      className={cn("-mx-1.5 my-1.5 h-px bg-border", className)}
      {...props}
    />
  );
}

const scrollButtonClass =
  "flex cursor-pointer items-center justify-center py-1 text-muted-foreground";

function SelectScrollUpButton({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.ScrollUpButton>) {
  return (
    <SelectPrimitive.ScrollUpButton className={cn(scrollButtonClass, className)} {...props}>
      <ChevronUp className="size-4" />
    </SelectPrimitive.ScrollUpButton>
  );
}

function SelectScrollDownButton({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.ScrollDownButton>) {
  return (
    <SelectPrimitive.ScrollDownButton className={cn(scrollButtonClass, className)} {...props}>
      <ChevronDown className="size-4" />
    </SelectPrimitive.ScrollDownButton>
  );
}
