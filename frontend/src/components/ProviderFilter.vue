<script setup lang="ts">
import { LayoutGrid } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import ProviderSelect from '@/components/ProviderSelect.vue'

defineProps<{ disabled?: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const provider = defineModel<string>({ required: true })
</script>

<template>
  <div class="flex min-w-0 max-w-full items-center gap-1">
    <div class="min-w-0 overflow-x-auto">
      <ProviderSelect
        v-model="provider"
        :aria-label="t('common.providers.filter')"
        :leading-option="{ label: t('common.providers.all'), value: '', icon: LayoutGrid }"
        :disabled="disabled"
      />
    </div>
  </div>
</template>
