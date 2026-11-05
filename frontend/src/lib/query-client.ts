import { QueryClient } from "@tanstack/react-query";

/**
 * 全局 QueryClient 独立成模块.
 *
 * 为什么不在 main.tsx 里内联创建: 换用户时必须 `queryClient.clear()`,
 * 否则 A 打开过的 /users 列表在 SPA 内换 B 之后仍然原样渲染.
 * 清理方(user-scope.ts)不在组件树里, 拿不到 Provider 上下文里的实例, 只能共享同一个引用.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 30_000,
      refetchOnWindowFocus: false,
    },
  },
});
