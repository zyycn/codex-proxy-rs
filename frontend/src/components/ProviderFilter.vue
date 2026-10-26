<script setup lang="ts">
import { ZSegmented } from '@codex-proxy/ui'

import { LayoutGrid } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'

defineProps<{ disabled?: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const provider = defineModel<string>({ required: true })
const options = computed(() => ([
  {
    label: t('common.providers.all'),
    value: '',
    icon: LayoutGrid,
  },
  ...PROVIDER_IDS.map(value => ({
    label: formatProviderLabel(value),
    value,
    icon: providerIcon(value),
  })),
]))
</script>

<template>
  <div class="flex min-w-0 max-w-full items-center gap-1">
    <div class="min-w-0 overflow-x-auto">
      <ZSegmented
        v-model="provider"
        :aria-label="t('common.providers.filter')"
        display="icon"
        :style="{ width: `${options.length * 40 + 4}px` }"
        :options="options"
        :disabled="disabled"
      />
    </div>
  </div>
</template>
