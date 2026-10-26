import type { Plugin } from 'vue'

import { setSessionRecoveryHandler, setUnauthorizedHandler } from '@/api/request'
import { router } from '@/router'
import { pinia } from '@/stores'
import { useAuthStore } from '@/stores/modules/auth'

export const authPlugin: Plugin = {
  install(app) {
    const authStore = useAuthStore(pinia)

    async function redirectToLogin(loginMode = authStore.session?.role ?? 'admin') {
      authStore.invalidateSession()
      if (router.currentRoute.value.name !== 'login') {
        await router.replace({
          name: 'login',
          state: { loginMode },
        })
      }
    }

    setUnauthorizedHandler(() => redirectToLogin())
    setSessionRecoveryHandler(() => authStore.refreshSession())

    async function restoreInitialNavigation() {
      if (document.visibilityState !== 'visible' || authStore.loading || authStore.checking || authStore.sessionChecked)
        return
      try {
        // 首次导航被临时故障中止时，只重试路由守卫的只读校验
        await router.replace(`${window.location.pathname}${window.location.search}${window.location.hash}`)
      }
      catch {
        // 服务不可用不代表退出，恢复联网后仍可再次校验
      }
    }

    const resume = () => {
      void restoreInitialNavigation()
    }
    document.addEventListener('visibilitychange', resume)
    window.addEventListener('focus', resume)
    window.addEventListener('online', resume)
    app.onUnmount(() => {
      document.removeEventListener('visibilitychange', resume)
      window.removeEventListener('focus', resume)
      window.removeEventListener('online', resume)
    })
  },
}
