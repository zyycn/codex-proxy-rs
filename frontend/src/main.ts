import { createApp } from 'vue'
import { setUnauthorizedHandler } from './api/request'
import App from './App.vue'

import { loading } from './directives/loading'
import { router } from './router'
import { pinia } from './stores'
import { useAuthStore } from './stores/modules/auth'
import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'

import './styles/index.css'

setUnauthorizedHandler(async () => {
  useAuthStore(pinia).invalidateSession()
  if (router.currentRoute.value.path !== '/login') {
    await router.replace({ name: 'login' })
  }
})

createApp(App).directive('loading', loading).use(pinia).use(router).mount('#app')
