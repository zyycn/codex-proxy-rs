<script setup lang="ts">
import type { AccountCreateSource } from '../../utils/accountCreate'

import { LayoutGrid } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderSelect from '@/components/ProviderSelect.vue'
import { isBuiltinProvider } from '@/providers/catalog'

defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const source = defineModel<AccountCreateSource | null>({ required: true })
const selected = computed({
  get: () => source.value?.kind === 'provider'
    ? source.value.id
    : source.value?.kind ?? '',
  set: (value: string) => {
    if (value !== 'bundle' && !isBuiltinProvider(value))
      return
    source.value = value === 'bundle'
      ? { kind: 'bundle' }
      : {
          kind: 'provider',
          id: value,
        }
  },
})
</script>

<template>
  <ProviderSelect
    v-model="selected"
    class="w-full"
    :aria-label="t('accounts.onboarding.selectAccountPlatform')"
    :leading-option="{ value: 'bundle', label: t('accounts.onboarding.batchImport'), icon: LayoutGrid }"
    display="label"
    :disabled="disabled"
    size="large"
  />
</template>
