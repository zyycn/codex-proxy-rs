import type { Plugin } from 'vite'
import { fileURLToPath } from 'node:url'
import { build } from 'vite'

// 桥运行在隔离 iframe 中，独立编译以保证注入源码不依赖宿主闭包或分块。
export function pluginBridge(): Plugin {
  const id = 'virtual:plugin-bridge-source'
  const resolvedId = `\0${id}`
  const entry = fileURLToPath(new URL('../src/views/plugins/management/bridge-runtime.ts', import.meta.url))
  const dependencies = new Set<string>()
  return {
    name: 'plugin-management-bridge',
    resolveId: source => source === id ? resolvedId : undefined,
    async load(source) {
      if (source !== resolvedId)
        return
      const result = await build({
        configFile: false,
        logLevel: 'silent',
        build: {
          write: false,
          minify: true,
          lib: { entry, name: 'CodexProxyPluginBridge', formats: ['iife'] },
        },
      })
      const outputs = (Array.isArray(result) ? result : [result])
        .flatMap(output => 'output' in output ? output.output : [])
      const [chunk] = outputs
      if (outputs.length !== 1 || chunk?.type !== 'chunk' || chunk.imports.length || chunk.dynamicImports.length)
        throw new Error('插件管理桥必须构建为单个独立脚本')
      dependencies.clear()
      for (const dependency of Object.keys(chunk.modules)) {
        dependencies.add(dependency)
        this.addWatchFile(dependency)
      }
      return `export default ${JSON.stringify(chunk.code)}`
    },
    handleHotUpdate({ file, server }) {
      if (dependencies.has(file)) {
        const module = server.moduleGraph.getModuleById(resolvedId)
        return module ? [module] : []
      }
    },
  }
}
