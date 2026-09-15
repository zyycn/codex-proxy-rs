import type { Plugin } from 'vue'

import { setUnauthorizedHandler } from '@/api/request'
import { router } from '@/router'
import { pinia } from '@/stores'
import { useAuthStore } from '@/stores/modules/auth'

export const authPlugin: Plugin = {
  install() {
    const authStore = useAuthStore(pinia)

    setUnauthorizedHandler(async () => {
      const loginMode = authStore.session?.role ?? 'admin'
      authStore.invalidateSession()

      if (router.currentRoute.value.name !== 'login')
        await router.replace({ name: 'login', state: { loginMode } })
    })
  },
}
