import { useAuthStore } from '@/stores/modules/auth'

import { useAdminOverviewSource } from './useAdminOverviewSource'
import { useKeyOverviewSource } from './useKeyOverviewSource'

/** 当前会话只选择一次数据源，页面和展示组件不感知身份。 */
export function useOverview() {
  const authStore = useAuthStore()
  return authStore.session?.type === 'key'
    ? useKeyOverviewSource()
    : useAdminOverviewSource()
}
