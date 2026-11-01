import type { Locale } from '../i18n/index.js';

const shell_catalog = {
  en: {
    navigation: 'Workspace navigation',
    agents: 'Coding agents',
    extensions: 'Tools & runtimes',
    supply: 'Supply',
    suppliers: 'Suppliers',
    suppliers_description: 'Manage API suppliers, protocol endpoints, and model catalogs',
    workspace: 'Workspace',
    provider_workspace: 'Provider workspace',
    client_workspace: 'Router workspace',
    tools_workspace: 'Tool workspace',
    runtime_workspace: 'Runtime workspace',
    local_workspace: 'Your local workspace',
    local_description: 'Providers, tools, and runtimes. Connected on your terms.',
    demo_workspace: 'Browser preview',
    demo_description: 'Synthetic data only. Native changes are not applied.',
    skip_content: 'Skip to workspace content',
    dsh_description: 'Manage the isolated runtime and its plugins',
    mcp_description: 'Manage MCP servers and review client synchronization',
    prompt_description: 'Turn a short Chinese request into an optimized English prompt',
    language: 'EN',
  },
  zh: {
    navigation: '工作区导航',
    agents: '编程助手',
    extensions: '工具与运行时',
    supply: '供应链',
    suppliers: '供货商',
    suppliers_description: '管理 API 供货商、协议端点与模型目录',
    workspace: '工作区',
    provider_workspace: '供应商工作区',
    client_workspace: '路由工作区',
    tools_workspace: '工具工作区',
    runtime_workspace: '运行时工作区',
    local_workspace: '本地工作区',
    local_description: '供应商、工具与运行时，由你决定如何连接。',
    demo_workspace: '浏览器预览',
    demo_description: '仅使用模拟数据，不会执行原生配置变更。',
    skip_content: '跳至工作区内容',
    dsh_description: '管理隔离运行时及其插件',
    mcp_description: '管理 MCP 服务并审阅客户端同步',
    prompt_description: '把简短的中文需求扩写为英文优化提示词',
    language: 'ZH',
  },
} as const;

export function get_shell_copy(locale: Locale) {
  return shell_catalog[locale];
}
