import { createApp } from 'vue'
import App from './App.vue'

import { loading } from './directives/loading'
import { authPlugin } from './plugins/auth'
import { router } from './router'
import { pinia } from './stores'
import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'

import './styles/index.css'

createApp(App)
  .directive('loading', loading)
  .use(pinia)
  .use(router)
  .use(authPlugin)
  .mount('#app')
