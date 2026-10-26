<script setup lang="ts">
import type { PluginArtifactMetadata, PluginCapabilityBinding, PluginInstance } from '@/api'

import { ZScrollbar } from '@codex-proxy/ui'
import { ChevronDown } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { PLUGIN_OBSERVER_EVENT_LABELS, PLUGIN_REQUEST_STAGES } from '../constants'
import { pluginCapabilityForContribution, pluginCapabilityLabel } from '../utils/model'

const props = defineProps<{
  instance: Pick<PluginInstance, 'name' | 'bindings'> & { id?: string }
  metadata?: PluginArtifactMetadata
  showCommand: boolean
}>()

const { t } = useI18n({ useScope: 'global' })

const groups = computed(() => {
  const result = new Map<string, { label: string, bindings: PluginCapabilityBinding[] }>()
  for (const binding of props.instance.bindings) {
    const capability = props.metadata
      ? pluginCapabilityForContribution(props.metadata, binding.contribution)
      : undefined
    if (capability === 'management' || capability === 'command_line')
      continue
    const group = result.get(binding.contribution)
    if (group) {
      group.bindings.push(binding)
    }
    else {
      result.set(binding.contribution, {
        label: pluginCapabilityLabel(capability ?? binding.contribution),
        bindings: [binding],
      })
    }
  }
  return [
    ...result.entries(),
  ].map(([id, group]) => {
    const scopes = new Map<string, { binding: PluginCapabilityBinding, stages: string[] }>()
    for (const binding of group.bindings) {
      const stage = PLUGIN_REQUEST_STAGES.value[binding.stage]
      // 按实际范围合并，不能把数量相同、目标不同的绑定当成同一项。
      const key = JSON.stringify([
        stage?.scope === 'none'
          ? binding.stage
          : Boolean(stage),
        binding.stage === 'authentication',
        [...binding.clientKeyIds].sort(),
        [...binding.accountGroupIds].sort(),
        [...binding.providerIds].sort(),
        [...binding.models].sort(),
        binding.identityBindings.map(identity => JSON.stringify([identity.principal, identity.clientKeyId])).sort(),
      ])
      const label = binding.event
        ? PLUGIN_OBSERVER_EVENT_LABELS.value[binding.event]
        : stage?.label ?? (binding.stage === 'authentication'
          ? t('plugins.configuration.clientAuthentication')
          : binding.stage)
      const scope = scopes.get(key)
      if (scope) {
        scope.stages.push(label)
      }
      else {
        scopes.set(key, {
          binding,
          stages: [label],
        })
      }
    }
    return {
      id,
      label: group.label,
      scopes: [
        ...scopes.entries(),
      ].map(([key, scope]) => ({
        key,
        stages: scope.stages.filter(label => label !== group.label).join('、'),
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
  const requests = bindings.filter(binding => PLUGIN_REQUEST_STAGES.value[binding.stage])
  return [
    requests.length && t('plugins.configuration.requestHandling', {
      value: requests.some(hasScope)
        ? t('plugins.configuration.customScope')
        : t('plugins.configuration.applyGlobally'),
    }),
    bindings.some(binding => binding.stage === 'authentication') && t('plugins.configuration.clientAuthentication'),
  ].filter(Boolean).join(' · ') || (groups.value.length
    ? t('plugins.configuration.description')
    : props.showCommand
      ? t('plugins.configuration.providesTerminalCommands')
      : t('plugins.configuration.requestHandlingNotConfigured'))
})

function scopeLabel(binding: PluginCapabilityBinding) {
  const scope = [
    binding.clientKeyIds.length && t('plugins.configuration.apiKeys', { count: binding.clientKeyIds.length }),
    binding.accountGroupIds.length && t('plugins.configuration.groups', { count: binding.accountGroupIds.length }),
    binding.providerIds.length && t('plugins.configuration.providers', { count: binding.providerIds.length }),
    binding.models.length && binding.models.join('、'),
  ].filter(Boolean).join(' · ')
  if (binding.stage === 'authentication') {
    return [
      t('plugins.configuration.identityMappings', { count: binding.identityBindings.length }),
      scope,
    ].filter(Boolean).join(' · ')
  }
  const stage = PLUGIN_REQUEST_STAGES.value[binding.stage]
  return scope || stage?.globalLabel || (stage
    ? t('plugins.configuration.allRequests')
    : t('plugins.configuration.noLimitOnScope'))
}
</script>

<template>
  <details v-if="groups.length || showCommand" class="group text-cp-xs text-cp-text-secondary">
    <summary class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-x-3 gap-y-2 rounded-cp py-1 outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline [&::-webkit-details-marker]:hidden">
      <span class="min-w-0 wrap-break-word">{{ summary }}</span>
      <span class="inline-flex shrink-0 items-center gap-1 text-cp-primary-text">
        {{ t('plugins.configuration.featureDetails') }}
        <ChevronDown class="size-3.5 transition-transform group-open:rotate-180 motion-reduce:transition-none" aria-hidden="true" />
      </span>
    </summary>
    <ZScrollbar class="mt-4 -mr-3" max-height="min(22rem, 38dvh)" role="region" :aria-label="t('plugins.configuration.capabilitiesOf', { name: instance.name })">
      <div class="grid gap-5 pr-6">
        <dl v-if="groups.length" class="m-0 grid grid-cols-1 gap-x-8 gap-y-4 sm:grid-cols-2">
          <div v-for="group in groups" :key="group.id" class="grid min-w-0 content-start gap-1">
            <dt class="wrap-break-word text-cp-sm font-emphasis text-cp-text">
              {{ group.label }}
            </dt>
            <dd class="m-0 grid min-w-0 gap-1 leading-relaxed">
              <div v-for="scope in group.scopes" :key="scope.key" class="wrap-break-word">
                <span v-if="scope.stages">{{ scope.stages }} · </span>
                <span>{{ scope.label }}</span>
              </div>
            </dd>
          </div>
        </dl>
        <div v-if="showCommand" class="grid gap-1.5">
          <span class="text-cp-sm font-emphasis text-cp-text">{{ t('plugins.configuration.terminalCommands') }}</span>
          <code class="break-all">codex-proxy-rs plugin {{ instance.id }} --help</code>
        </div>
      </div>
    </ZScrollbar>
  </details>
</template>
