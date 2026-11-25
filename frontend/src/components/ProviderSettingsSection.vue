<script setup lang="ts">
import type { BuiltinProvider } from '@/providers/catalog'
import { useI18n } from 'vue-i18n'
import ProviderSelect from '@/components/ProviderSelect.vue'

defineProps<{ disabled?: boolean }>()
const provider = defineModel<BuiltinProvider>({ required: true })
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <section class="grid min-w-0 gap-4" :aria-label="t('common.providers.settings')">
    <div class="flex justify-end">
      <ProviderSelect
        v-model="provider"
        :disabled="disabled"
        class="shrink-0"
      />
    </div>
    <slot />
  </section>
</template>
