<script setup lang="ts">
import type { BuiltinProvider } from '@/providers/catalog'
import { ZSegmented } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, isBuiltinProvider, PROVIDER_IDS, providerIcon } from '@/providers/catalog'

const { t } = useI18n({ useScope: 'global' })
const provider = defineModel<BuiltinProvider>({ required: true })
const options = PROVIDER_IDS.map(value => ({
  value,
  label: value === 'openai'
    ? 'OpenAI'
    : formatProviderLabel(value),
  icon: providerIcon(value),
}))

function selectProvider(value: string) {
  if (isBuiltinProvider(value))
    provider.value = value
}
</script>

<template>
  <ZSegmented
    :model-value="provider"
    :options="options"
    :aria-label="t('settings.page.providerLabel')"
    class="w-full @sm/settings:w-auto @max-sm/settings:[&>button]:px-2"
    @update:model-value="selectProvider"
  />
</template>
