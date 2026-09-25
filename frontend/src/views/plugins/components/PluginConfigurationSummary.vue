<script setup lang="ts">
import type { PluginArtifactMetadata, PluginCapabilityBinding, PluginInstance } from '@/api'
import { BaseScrollbar } from '@codex-proxy/ui'
import { ChevronDown } from '@lucide/vue'
import { computed } from 'vue'
import { pluginCapabilityForContribution, pluginCapabilityLabel } from '../utils/model'

const props = defineProps<{
  instance: Pick<PluginInstance, 'name' | 'bindings'> & { id?: string }
  metadata?: PluginArtifactMetadata
  showCommand: boolean
}>()

const stageLabels: Record<string, string> = {
  request: '请求开始',
  attempt: '每次尝试',
  routing: '模型路由',
  scheduling: '账号调度',
  retry: '重试决策',
  observation: '请求观察',
  management: '管理操作',
  execution: '模型执行',
  maintenance: '后台维护',
  authentication: '客户端认证',
}
const requestStages = new Set(['request', 'attempt', 'routing', 'scheduling', 'retry', 'observation'])
const groups = computed(() => {
  const result = new Map<string, { label: string, bindings: PluginCapabilityBinding[] }>()
  for (const binding of props.instance.bindings) {
    const capability = props.metadata
      ? pluginCapabilityForContribution(props.metadata, binding.contribution)
      : undefined
    if (capability === 'management' || capability === 'command_line')
      continue
    const group = result.get(binding.contribution)
    if (group)
      group.bindings.push(binding)
    else result.set(binding.contribution, { label: pluginCapabilityLabel(capability ?? binding.contribution), bindings: [binding] })
  }
  return [...result.entries()].map(([id, group]) => {
    const scopes = new Map<string, { binding: PluginCapabilityBinding, stages: string[] }>()
    for (const binding of group.bindings) {
      // 按实际范围合并，不能把数量相同、目标不同的绑定当成同一项。
      const key = JSON.stringify([
        requestStages.has(binding.stage),
        binding.stage === 'authentication',
        [...binding.clientKeyIds].sort(),
        [...binding.accountGroupIds].sort(),
        [...binding.providerIds].sort(),
        [...binding.models].sort(),
        binding.identityBindings.map(identity => JSON.stringify([identity.principal, identity.clientKeyId])).sort(),
      ])
      const scope = scopes.get(key)
      if (scope)
        scope.stages.push(binding.stage)
      else scopes.set(key, { binding, stages: [binding.stage] })
    }
    return {
      id,
      label: group.label,
      scopes: [...scopes.entries()].map(([key, scope]) => ({
        key,
        stages: scope.stages.map(stage => stageLabels[stage] ?? stage).filter(label => label !== group.label).join('、'),
        label: scopeLabel(scope.binding),
      })),
    }
  })
})

function hasScope(binding: PluginCapabilityBinding) {
  return Boolean(binding.clientKeyIds.length || binding.accountGroupIds.length || binding.providerIds.length || binding.models.length)
}

const summary = computed(() => {
  const bindings = props.instance.bindings
  const requests = bindings.filter(binding => requestStages.has(binding.stage))
  return [
    requests.length && `请求处理：${requests.some(hasScope) ? '自定义范围' : '所有请求'}`,
    bindings.some(binding => binding.stage === 'maintenance') && '后台维护',
    bindings.some(binding => binding.stage === 'authentication') && '客户端认证',
  ].filter(Boolean).join(' · ') || (groups.value.length ? '按配置提供扩展功能' : props.showCommand ? '提供终端命令' : '未配置请求处理')
})

function scopeLabel(binding: PluginCapabilityBinding) {
  const scope = [
    binding.clientKeyIds.length && `${binding.clientKeyIds.length} 个 Key`,
    binding.accountGroupIds.length && `${binding.accountGroupIds.length} 个分组`,
    binding.providerIds.length && `${binding.providerIds.length} 个 Provider`,
    binding.models.length && binding.models.join('、'),
  ].filter(Boolean).join(' · ')
  if (binding.stage === 'authentication')
    return [`${binding.identityBindings.length} 组身份映射`, scope].filter(Boolean).join(' · ')
  return scope || (requestStages.has(binding.stage) ? '所有请求' : '不限制范围')
}
</script>

<template>
  <details v-if="groups.length || showCommand" class="group text-cp-xs text-cp-text-secondary">
    <summary class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-x-3 gap-y-2 rounded-cp py-1 outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline [&::-webkit-details-marker]:hidden">
      <span class="min-w-0 break-words">{{ summary }}</span>
      <span class="inline-flex shrink-0 items-center gap-1 text-cp-primary-text">
        功能详情
        <ChevronDown class="size-3.5 transition-transform group-open:rotate-180 motion-reduce:transition-none" aria-hidden="true" />
      </span>
    </summary>
    <BaseScrollbar class="mt-4 -mr-3" max-height="min(22rem, 38dvh)" role="region" :aria-label="`${instance.name} 的功能详情`">
      <div class="grid gap-5 pr-6">
        <dl v-if="groups.length" class="m-0 grid grid-cols-1 gap-x-8 gap-y-4 sm:grid-cols-2">
          <div v-for="group in groups" :key="group.id" class="grid min-w-0 content-start gap-1">
            <dt class="break-words text-cp-sm font-emphasis text-cp-text">
              {{ group.label }}
            </dt>
            <dd class="m-0 grid min-w-0 gap-1 leading-relaxed">
              <div v-for="scope in group.scopes" :key="scope.key" class="break-words">
                <span v-if="scope.stages">{{ scope.stages }} · </span>
                <span>{{ scope.label }}</span>
              </div>
            </dd>
          </div>
        </dl>
        <div v-if="showCommand" class="grid gap-1.5">
          <span class="text-cp-sm font-emphasis text-cp-text">终端命令</span>
          <code class="break-all">codex-proxy-rs plugin {{ instance.id }} --help</code>
        </div>
      </div>
    </BaseScrollbar>
  </details>
</template>
