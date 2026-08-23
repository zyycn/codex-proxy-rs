import { createApp } from 'vue'
import App from './App.vue'

import { loading } from './directives/loading'
import { authPlugin } from './plugins/auth'
import { router } from './router'
import { pinia } from './stores'
import { useThemeStore } from './stores/modules/theme'
import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'

import './styles/index.css'

const app = createApp(App)

app.directive('loading', loading)
app.use(pinia)

useThemeStore(pinia).initializeTheme()

app.use(router)
app.use(authPlugin)
app.mount('#app')
