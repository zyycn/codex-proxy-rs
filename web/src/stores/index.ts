import { createPinia } from 'pinia'
import piniaPluginPersistedstate from 'pinia-plugin-persistedstate'

export const pinia = createPinia()

pinia.use(piniaPluginPersistedstate)

// 导出所有 store
export * from './modules/auth'
export * from './modules/ui'
