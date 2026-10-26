<script setup lang="ts">
import type { JsonSchema } from '../utils/model'

import type { ConfigurePluginInstanceRequest, PluginArtifactMetadata, PluginInstance } from '@/api'
import { ZButton, ZDialog, ZTag } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import PluginConfigurationSummary from './PluginConfigurationSummary.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{
  request?: ConfigurePluginInstanceRequest
  metadata?: PluginArtifactMetadata
  replacements: (Pick<PluginInstance, 'id' | 'name'> & { version?: string })[]
  saving: boolean
}>()

defineEmits<{ confirm: [], closed: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const parameters = computed(() => {
  const schema = props.metadata?.configurationSchema as JsonSchema | undefined
  return Object.entries(props.request?.configuration ?? {})
    .filter(([key]) => !props.metadata?.secretFields.includes(key))
    .map(([key, value]) => ({
      key,
      label: schema?.properties?.[key]?.title ?? key,
      value: value == null
        ? t('plugins.enable.notSet')
        : typeof value === 'string'
          ? value
          : JSON.stringify(value),
    }))
})
</script>

<template>
  <ZDialog v-model="open" :title="replacements.length ? t('plugins.enable.switchActiveConfiguration') : t('plugins.enable.enablePluginConfiguration')" width="36rem" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving" @closed="$emit('closed')">
    <div v-if="request" class="grid gap-4">
      <div class="grid gap-3 rounded-cp bg-cp-fill-alter p-4">
        <div class="flex min-w-0 flex-wrap items-center gap-2">
          <strong class="min-w-0 flex-1 wrap-anywhere text-cp-sm">{{ request.name }}</strong>
          <ZTag v-if="metadata">
            {{ metadata.version }}
          </ZTag>
          <ZTag type="primary">
            {{ t('plugins.enable.willBeEnabled') }}
          </ZTag>
        </div>
        <dl v-if="parameters.length" class="m-0 grid grid-cols-1 gap-3 text-cp-sm sm:grid-cols-2">
          <div v-for="parameter in parameters" :key="parameter.key" class="grid min-w-0 gap-1">
            <dt class="wrap-anywhere text-cp-text-secondary">
              {{ parameter.label }}
            </dt>
            <dd class="m-0 max-h-24 overflow-y-auto whitespace-pre-wrap wrap-anywhere">
              {{ parameter.value }}
            </dd>
          </div>
        </dl>
        <PluginConfigurationSummary :instance="request" :metadata="metadata" :show-command="false" />
      </div>
      <div v-if="replacements.length" class="grid gap-2">
        <div class="flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
          <span>{{ t('plugins.enable.configurationsToDisable', { count: replacements.length }) }}</span>
          <PluginHelpPopover :label="t('plugins.enable.toggleEnablementInstructions')">
            {{ t('plugins.enable.disableDescription') }}
          </PluginHelpPopover>
        </div>
        <ul class="m-0 max-h-40 list-none space-y-2 overflow-y-auto p-0">
          <li v-for="instance in replacements" :key="instance.id" class="flex items-center gap-3 rounded-cp bg-cp-fill-alter p-3">
            <span class="min-w-0 flex-1 wrap-anywhere text-cp-sm font-emphasis">{{ instance.name }}</span>
            <div class="flex shrink-0 flex-wrap items-center justify-end gap-2">
              <ZTag v-if="instance.version">
                {{ instance.version }}
              </ZTag>
              <ZTag type="warning">
                {{ t('plugins.enable.willBeDeactivated') }}
              </ZTag>
            </div>
          </li>
        </ul>
      </div>
    </div>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="saving" :disabled="!request" @click="$emit('confirm')">
        {{ replacements.length ? t('plugins.enable.confirmSwitch') : t('plugins.enable.confirmEnable') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
