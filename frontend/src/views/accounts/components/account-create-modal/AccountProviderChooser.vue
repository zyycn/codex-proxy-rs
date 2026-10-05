<script setup lang="ts">
import type { AccountCreateSource } from '../../utils/accountCreate'
import { BaseSegmented } from '@codex-proxy/ui'
import { LayoutGrid } from '@lucide/vue'
import { computed } from 'vue'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'

defineProps<{ disabled: boolean }>()
const source = defineModel<AccountCreateSource | null>({ required: true })
const options = [
  { value: 'bundle', label: '批量导入', icon: LayoutGrid },
  ...PROVIDER_IDS.map(provider => ({
    value: `provider:${provider}`,
    label: formatProviderLabel(provider),
    icon: providerIcon(provider),
  })),
]
const selected = computed({
  get: () => source.value?.kind === 'provider' ? `provider:${source.value.id}` : source.value?.kind ?? '',
  set: (value: string) => {
    if (!options.some(option => option.value === value))
      return
    source.value = value === 'bundle' ? { kind: 'bundle' } : { kind: 'provider', id: value.slice('provider:'.length) }
  },
})
</script>

<template>
  <BaseSegmented
    v-model="selected"
    class="w-full"
    label="选择账号平台"
    :options="options"
    :disabled="disabled"
    size="lg"
  />
</template>
