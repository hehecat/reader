/**
 * 没有自带类型声明的 cytoscape 布局扩展.
 * 只声明注册函数(cytoscape.Ext), 布局参数走 cytoscape 的宽松 LayoutOptions.
 */
declare module "cytoscape-klay" {
  import type cytoscape from "cytoscape";

  const register: (cy: typeof cytoscape) => void;
  export default register;
}

declare module "cytoscape-fcose" {
  import type cytoscape from "cytoscape";

  const register: (cy: typeof cytoscape) => void;
  export default register;
}
