import { createRouter, createWebHistory } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { routes } from './routes'

export const router = createRouter({
  history: createWebHistory('/'),
  routes,
})

router.beforeEach(async (to) => {
  const authStore = useAuthStore()

  // 登录页不依赖会话恢复；只使用当前已知身份决定是否跳转。
  if (to.name === 'login') {
    if (authStore.isAuthenticated)
      return { name: 'dashboard' }
    return
  }

  const login = { name: 'login', query: { redirect: to.fullPath } }

  if (!authStore.sessionChecked) {
    try {
      await authStore.checkAuth()
    }
    catch {
      // 暂时无法确认会话时不进入受保护页面，也不缓存成“已退出”。
      return login
    }
  }

  if (!authStore.isAuthenticated)
    return login

  if (to.meta.role === 'admin' && !authStore.isAdmin)
    return { name: 'dashboard' }
})
