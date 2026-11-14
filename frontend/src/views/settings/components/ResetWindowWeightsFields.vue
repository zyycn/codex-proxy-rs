<script setup lang="ts">
import type { ResetWindowWeights } from '@/api'
import { ZFormItem, ZIconButton, ZInput, ZPopover } from '@codex-proxy/ui'
import { CircleHelp } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { validSchedulingWeight } from '../utils/scheduling'

defineProps<{ submitted: boolean, help: string }>()
const model = defineModel<Record<keyof ResetWindowWeights, string>>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const periods = computed(() => [
  {
    key: 'fiveHour',
    label: t('settings.scheduling.fiveHour'),
  },
  {
    key: 'weekly',
    label: t('settings.scheduling.weekly'),
  },
  {
    key: 'monthly',
    label: t('settings.scheduling.monthly'),
  },
] as const)
</script>

<template>
  <section class="grid gap-3" :aria-label="t('settings.scheduling.windowMode')">
    <div class="flex items-center gap-1">
      <h3 class="m-0 text-z font-emphasis">
        {{ t('settings.scheduling.windowMode') }}
      </h3>
      <ZPopover trigger="hover-click" placement="top-start">
        <template #reference="{ open: helpOpen }">
          <ZIconButton variant="link" :aria-label="t('settings.scheduling.about', { label: t('settings.scheduling.windowMode') })" :aria-expanded="helpOpen" class="size-6!">
            <CircleHelp class="size-3.5" />
          </ZIconButton>
        </template>
        <p class="m-0 max-w-64 px-3 py-2 text-z-sm leading-relaxed">
          {{ help }}
        </p>
      </ZPopover>
    </div>
    <div class="grid grid-cols-3 gap-3">
      <ZFormItem v-for="period in periods" :key="period.key" class="min-w-0" :label="period.label" :error="submitted && !validSchedulingWeight(model[period.key]) ? t('settings.scheduling.enterValueFromToUpToOne') : undefined">
        <ZInput v-model="model[period.key]" type="number" min="0" max="10" step="0.1" :aria-label="period.label" :aria-invalid="submitted && !validSchedulingWeight(model[period.key])" />
      </ZFormItem>
    </div>
  </section>
</template>
