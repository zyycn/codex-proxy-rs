<script setup lang="ts">
import type { PluginArtifactMetadata, PluginCapabilityBinding } from '@/api'
import { BaseCheckbox, BaseFormItem, BaseNumberInput, BaseSelect, BaseSwitch } from '@codex-proxy/ui'
import { computed, ref, watch } from 'vue'
import { pluginCapabilityLabel } from '../utils/model'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginResourcePicker from './PluginResourcePicker.vue'
import PluginScopeInput from './PluginScopeInput.vue'

const props = defineProps<{ metadata: PluginArtifactMetadata, disabled?: boolean }>()
const emit = defineEmits<{ validityChange: [valid: boolean] }>()
const bindings = defineModel<PluginCapabilityBinding[]>({ required: true })
const stages: Record<string, string[]> = {
  middleware: ['request', 'attempt'],
  model_router: ['routing'],
  scheduler: ['scheduling'],
  retry_policy: ['retry'],
  request_lifecycle: ['observation'],
  usage: ['observation'],
  web_socket_observer: ['observation'],
}
const stageLabels: Record<string, string> = { request: '请求开始', attempt: '每次尝试', routing: '模型路由', scheduling: '账号调度', retry: '重试决策', observation: '请求结束' }
const entries = computed(() => Object.entries(props.metadata.contributes).flatMap(([capability, contribution]) =>
  contribution.stages.filter(stage => stages[capability]?.includes(stage))
    .map(stage => ({ capability, contribution: contribution.id, stage, key: `${contribution.id}:${stage}` })),
))
function keyOf(binding: PluginCapabilityBinding) {
  return `${binding.contribution}:${binding.stage}`
}
function hasScope(binding: PluginCapabilityBinding) {
  return Boolean(binding.clientKeyIds.length || binding.accountGroupIds.length || binding.models.length || binding.providerIds.length)
}
const globalScopes = ref(new Set(bindings.value.filter(binding => !hasScope(binding)).map(keyOf)))
type RequestScope = Pick<PluginCapabilityBinding, 'clientKeyIds' | 'accountGroupIds' | 'models' | 'providerIds'>
const scopeDrafts = new Map<string, RequestScope>()
function bindingFor(key: string) {
  return bindings.value.find(binding => keyOf(binding) === key)
}
function patch(key: string, value: Partial<PluginCapabilityBinding>) {
  bindings.value = bindings.value.map(binding => keyOf(binding) === key ? { ...binding, ...value } : binding)
}
function toggle(entry: typeof entries.value[number], enabled: boolean) {
  globalScopes.value.delete(entry.key)
  scopeDrafts.delete(entry.key)
  bindings.value = bindings.value.filter(binding => keyOf(binding) !== entry.key)
  if (enabled) {
    const order = entry.stage === 'observation' ? bindings.value.find(binding => binding.stage === 'observation')?.order ?? 0 : 0
    bindings.value = [...bindings.value, { contribution: entry.contribution, stage: entry.stage, order, failurePolicy: entry.stage === 'observation' ? 'observe' : entry.stage === 'retry' ? 'delegate' : 'reject', providerIds: [], models: [], clientKeyIds: [], accountGroupIds: [], identityBindings: [] }]
  }
}
function setGlobal(key: string, value: boolean) {
  if (value) {
    const binding = bindingFor(key)
    if (binding && !globalScopes.value.has(key)) {
      const { clientKeyIds, accountGroupIds, models, providerIds } = binding
      scopeDrafts.set(key, { clientKeyIds, accountGroupIds, models, providerIds })
    }
    globalScopes.value.add(key)
    patch(key, { clientKeyIds: [], accountGroupIds: [], models: [], providerIds: [] })
  }
  else {
    globalScopes.value.delete(key)
    const draft = scopeDrafts.get(key)
    if (draft)
      patch(key, draft)
  }
}
const valid = computed(() => entries.value.every((entry) => {
  const binding = bindingFor(entry.key)
  return !binding || globalScopes.value.has(entry.key) || hasScope(binding)
}))
watch(valid, value => emit('validityChange', value), { immediate: true })
</script>

<template>
  <div class="grid gap-4">
    <article v-for="entry in entries" :key="entry.key" class="grid gap-4 rounded-cp bg-cp-fill-alter p-4">
      <div class="flex items-center gap-2">
        <BaseSwitch :model-value="Boolean(bindingFor(entry.key))" :label="`${pluginCapabilityLabel(entry.capability)} · ${stageLabels[entry.stage]}`" show-label :disabled="disabled" @update:model-value="toggle(entry, $event)" />
        <PluginHelpPopover v-if="entry.stage === 'observation'" label="请求观察说明">
          仅观察请求，不改变请求结果
        </PluginHelpPopover>
      </div>
      <template v-if="bindingFor(entry.key)">
        <div class="flex items-center gap-2">
          <BaseCheckbox :model-value="globalScopes.has(entry.key)" label="应用于所有请求" show-label :disabled="disabled" @update:model-value="setGlobal(entry.key, $event)" />
          <PluginHelpPopover label="生效请求范围说明">
            关闭后至少选择一项范围，不同条件同时满足才生效，同一条件内任意一项匹配即可
          </PluginHelpPopover>
        </div>
        <div v-if="!globalScopes.has(entry.key)" class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="客户端 Key">
            <PluginResourcePicker :model-value="bindingFor(entry.key)!.clientKeyIds" kind="keys" :disabled="disabled" @update:model-value="patch(entry.key, { clientKeyIds: $event })" />
          </BaseFormItem>
          <BaseFormItem label="账号分组">
            <PluginResourcePicker :model-value="bindingFor(entry.key)!.accountGroupIds" kind="groups" :disabled="disabled" @update:model-value="patch(entry.key, { accountGroupIds: $event })" />
          </BaseFormItem>
          <PluginScopeInput :model-value="bindingFor(entry.key)!.models" label="模型范围" placeholder="每行一个模型名称，精确匹配" :class="['routing', 'request'].includes(entry.stage) ? 'sm:col-span-2' : undefined" :disabled="disabled" @update:model-value="patch(entry.key, { models: $event })" />
          <PluginScopeInput v-if="!['routing', 'request'].includes(entry.stage)" :model-value="bindingFor(entry.key)!.providerIds" label="Provider 范围" placeholder="每行一个 Provider 标识" :disabled="disabled" @update:model-value="patch(entry.key, { providerIds: $event })" />
        </div>
        <div v-if="entry.stage !== 'observation'" class="grid items-end gap-4 sm:grid-cols-2">
          <BaseFormItem label="执行顺序">
            <template #label-extra>
              <PluginHelpPopover label="执行顺序说明">
                数值较小的先执行
              </PluginHelpPopover>
            </template>
            <BaseNumberInput :model-value="bindingFor(entry.key)!.order" label="执行顺序" size="md" :min="-2147483648" :max="2147483647" :disabled="disabled" class="w-full" @update:model-value="patch(entry.key, { order: $event })" />
          </BaseFormItem>
          <BaseFormItem label="插件失败时">
            <BaseSelect :model-value="bindingFor(entry.key)!.failurePolicy" :options="entry.stage === 'retry' ? [{ label: '交给后续处理', value: 'delegate' }] : [{ label: '拒绝请求', value: 'reject' }, { label: '交给后续处理', value: 'delegate' }]" :disabled="disabled || entry.stage === 'retry'" class="w-full" @update:model-value="patch(entry.key, { failurePolicy: $event as 'reject' | 'delegate' })" />
          </BaseFormItem>
        </div>
      </template>
    </article>
  </div>
</template>
