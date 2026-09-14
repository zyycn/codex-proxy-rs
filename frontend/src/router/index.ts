import { createRouter, createWebHistory } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { routes } from './routes'

export const router = createRouter({
  history: createWebHistory('/'),
  routes,
})

router.beforeEach(async (to) => {
  const authStore = useAuthStore()
  const login = { name: 'login', query: { redirect: to.fullPath }, state: { loginType: to.meta.role === 'key' ? 'key' : 'admin' } }

  if (!authStore.sessionChecked) {
    try {
      await authStore.checkAuth()
    }
    catch {
      // 暂时无法确认会话时不进入受保护页面，也不缓存成“已退出”。
      return to.name === 'login' ? undefined : login
    }
  }

  const identity = authStore.session?.type
  if (to.name === 'login') {
    if (identity)
      return identity === 'key' ? { name: 'key-overview' } : { name: 'dashboard' }
    return
  }

  if (!identity)
    return login

  if (to.meta.role !== identity)
    return identity === 'key' ? { name: 'key-overview' } : { name: 'dashboard' }
})
