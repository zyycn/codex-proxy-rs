import type { PluginManagementPage, PluginManagementView } from '@/api'

export function pluginPageLocation(
  view: PluginManagementView,
  page: PluginManagementPage,
) {
  return {
    name: 'plugin-page',
    params: {
      instanceId: view.target.instanceId,
      pageId: page.id,
    },
  }
}

export function shortPluginInstanceId(instanceId: string) {
  return instanceId.length > 12
    ? `${instanceId.slice(0, 7)}…${instanceId.slice(-4)}`
    : instanceId
}
