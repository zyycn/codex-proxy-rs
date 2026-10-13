/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_TIMEOUT_MS?: string
}

declare module '@fontsource-variable/inter'
declare module '@fontsource-variable/jetbrains-mono'

declare module 'virtual:plugin-bridge-source' {
  const source: string
  export default source
}
