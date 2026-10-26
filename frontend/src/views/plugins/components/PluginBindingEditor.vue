<script setup lang="ts">
import type { PluginArtifactMetadata, PluginCapabilityBinding } from '@/api'

import { ZCheckbox, ZFormItem, ZInputNumber, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { pluginCapabilityLabel, pluginRequestBindingEntries } from '../utils/model'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginResourcePicker from './PluginResourcePicker.vue'
import PluginScopeInput from './PluginScopeInput.vue'

const props = withDefaults(defineProps<{ metadata: PluginArtifactMetadata, disabled?: boolean, active?: boolean }>(), { active: true })

const emit = defineEmits<{ validityChange: [valid: boolean] }>()

const { t } = useI18n({ useScope: 'global' })

const bindings = defineModel<PluginCapabilityBinding[]>({ required: true })
const entries = computed(() => pluginRequestBindingEntries(props.metadata).map(entry => ({
  ...entry,
  key: keyOf(entry),
})))
function keyOf(binding: Pick<PluginCapabilityBinding, 'contribution' | 'stage' | 'event'>) {
  return `${binding.contribution}:${binding.stage}:${binding.event ?? ''}`
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
  bindings.value = bindings.value.map(binding => keyOf(binding) === key
    ? {
        ...binding,
        ...value,
      }
    : binding)
}
function toggle(entry: typeof entries.value[number], enabled: boolean) {
  globalScopes.value.delete(entry.key)
  scopeDrafts.delete(entry.key)
  bindings.value = bindings.value.filter(binding => keyOf(binding) !== entry.key)
  if (enabled) {
    const order = entry.stage === 'observation'
      ? bindings.value.find(binding => binding.stage === 'observation')?.order ?? 0
      : 0
    bindings.value = [
      ...bindings.value,
      {
        contribution: entry.contribution,
        stage: entry.stage,
        event: entry.event,
        order,
        failurePolicy: entry.stage === 'observation'
          ? 'observe'
          : entry.stage === 'retry'
            ? 'delegate'
            : 'reject',
        providerIds: [],
        models: [],
        clientKeyIds: [],
        accountGroupIds: [],
        identityBindings: [],
      },
    ]
  }
}
function setGlobal(key: string, value: boolean) {
  if (value) {
    const binding = bindingFor(key)
    if (binding && !globalScopes.value.has(key)) {
      const { clientKeyIds, accountGroupIds, models, providerIds } = binding
      scopeDrafts.set(key, {
        clientKeyIds,
        accountGroupIds,
        models,
        providerIds,
      })
    }
    globalScopes.value.add(key)
    patch(key, {
      clientKeyIds: [],
      accountGroupIds: [],
      models: [],
      providerIds: [],
    })
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
  return !binding || entry.scope === 'none' || globalScopes.value.has(entry.key) || hasScope(binding)
}))
watch(valid, value => emit('validityChange', value), { immediate: true })
</script>

<template>
  <div class="grid gap-4">
    <article v-for="entry in entries" :key="entry.key" class="grid gap-4 rounded-cp bg-cp-fill-alter p-4">
      <div class="flex items-center gap-2">
        <ZSwitch :model-value="Boolean(bindingFor(entry.key))" :label="`${pluginCapabilityLabel(entry.capability)} · ${entry.label}`" :disabled="disabled" @update:model-value="toggle(entry, $event)" />
        <PluginHelpPopover v-if="entry.stage === 'observation'" :label="t('plugins.bindings.observationHelpTitle')">
          {{ t('plugins.bindings.observationHelp') }}
        </PluginHelpPopover>
      </div>
      <template v-if="bindingFor(entry.key)">
        <p v-if="entry.scope === 'none'" class="text-cp-xs text-cp-text-secondary">
          {{ entry.globalLabel }}
        </p>
        <div v-else class="flex items-center gap-2">
          <ZCheckbox :model-value="globalScopes.has(entry.key)" :label="t('plugins.bindings.applyToAllRequests')" :disabled="disabled" @update:model-value="setGlobal(entry.key, $event)" />
          <PluginHelpPopover :label="t('plugins.bindings.scopeHelpTitle')">
            {{ t('plugins.bindings.scopeHelp') }}
          </PluginHelpPopover>
        </div>
        <div v-if="entry.scope !== 'none' && !globalScopes.has(entry.key)" class="grid gap-4 sm:grid-cols-2">
          <ZFormItem :label="t('plugins.bindings.clientKey')">
            <PluginResourcePicker :model-value="bindingFor(entry.key)!.clientKeyIds" kind="keys" :active="active" :disabled="disabled" @update:model-value="patch(entry.key, { clientKeyIds: $event })" />
          </ZFormItem>
          <ZFormItem :label="t('plugins.bindings.accountGrouping')">
            <PluginResourcePicker :model-value="bindingFor(entry.key)!.accountGroupIds" kind="groups" :active="active" :disabled="disabled" @update:model-value="patch(entry.key, { accountGroupIds: $event })" />
          </ZFormItem>
          <PluginScopeInput :model-value="bindingFor(entry.key)!.models" :label="t('plugins.bindings.modelScope')" :placeholder="t('plugins.bindings.oneModelNamePerLineExactMatch')" :class="entry.scope === 'model' ? 'sm:col-span-2' : undefined" :disabled="disabled" @update:model-value="patch(entry.key, { models: $event })" />
          <PluginScopeInput v-if="entry.scope === 'provider'" :model-value="bindingFor(entry.key)!.providerIds" :label="t('plugins.bindings.providerScope')" :placeholder="t('plugins.bindings.oneProviderIdentifierPerLine')" :disabled="disabled" @update:model-value="patch(entry.key, { providerIds: $event })" />
        </div>
        <div v-if="entry.stage !== 'observation'" class="grid items-end gap-4" :class="entry.stage !== 'upstream' ? 'sm:grid-cols-2' : undefined">
          <ZFormItem v-if="entry.stage !== 'upstream'" :label="t('plugins.bindings.executionOrder')">
            <template #label-extra>
              <PluginHelpPopover :label="t('plugins.bindings.executionSequenceDescription')">
                {{ t('plugins.bindings.priorityHelp') }}
              </PluginHelpPopover>
            </template>
            <ZInputNumber :model-value="bindingFor(entry.key)!.order" :aria-label="t('plugins.bindings.executionOrder')" size="default" :min="-2147483648" :max="2147483647" :disabled="disabled" class="w-full" @update:model-value="patch(entry.key, { order: $event })" />
          </ZFormItem>
          <ZFormItem :label="t('plugins.bindings.whenPluginFails')">
            <ZSelect :model-value="bindingFor(entry.key)!.failurePolicy" :options="entry.stage === 'retry' ? [{ label: t('plugins.bindings.continueToNextHandler'), value: 'delegate' }] : entry.stage === 'upstream' ? [{ label: t('plugins.bindings.rejectRequest'), value: 'reject' }] : [{ label: t('plugins.bindings.rejectRequest'), value: 'reject' }, { label: t('plugins.bindings.continueToNextHandler'), value: 'delegate' }]" :disabled="disabled || ['retry', 'upstream'].includes(entry.stage)" class="w-full" @update:model-value="patch(entry.key, { failurePolicy: $event as 'reject' | 'delegate' })" />
          </ZFormItem>
        </div>
      </template>
    </article>
  </div>
</template>
