import { createApp } from 'vue'
import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'

import App from './App.vue'
import { router } from './router'
import { pinia } from './stores'

import './styles/index.css'

createApp(App).use(pinia).use(router).mount('#app')
