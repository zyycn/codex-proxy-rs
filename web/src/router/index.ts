import { createRouter, createWebHistory } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { routes } from './routes'

export const router = createRouter({
  history: createWebHistory('/'),
  routes,
})

// 路由守卫
router.beforeEach(async (to) => {
  const authStore = useAuthStore()

  // 登录页面不需要认证
  if (to.path === '/login') {
    // 如果已登录，重定向到首页
    if (authStore.isAuthenticated) {
      return '/'
    }
    return
  }

  // 其他页面需要认证
  if (!authStore.isAuthenticated && !authStore.sessionChecked) {
    // 尝试检查认证状态
    const isAuth = await authStore.checkAuth()
    if (!isAuth) {
      // 未认证，跳转到登录页
      return '/login'
    }
  }

  if (!authStore.isAuthenticated) {
    return '/login'
  }
})
