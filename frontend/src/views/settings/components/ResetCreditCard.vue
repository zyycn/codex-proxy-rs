<script setup lang="ts">
import { ZCard, ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const leadMinutes = defineModel<string>({ required: true })
const onLowWeeklyQuota = defineModel<boolean>('onLowWeeklyQuota', { required: true })
const maxWeeklyRemainingPercent = defineModel<string>('maxWeeklyRemainingPercent', { required: true })
const modes = computed(() => [
  {
    value: 'expiry',
    label: t('settings.quotaReset.expiryMode'),
  },
  {
    value: 'weekly',
    label: t('settings.quotaReset.weeklyMode'),
  },
])
</script>

<template>
  <ZCard id="reset-credits" :title="t('settings.quotaReset.title')" :description="t('settings.quotaReset.enableHelp')">
    <div class="grid min-w-0 max-w-6xl gap-4.5 sm:grid-cols-2">
      <ZSegmented
        :model-value="onLowWeeklyQuota ? 'weekly' : 'expiry'"
        class="col-span-full justify-self-start"
        :options="modes"
        :aria-label="t('settings.quotaReset.timing')"
        @update:model-value="onLowWeeklyQuota = $event === 'weekly'"
      />
      <ZFormItem v-if="!onLowWeeklyQuota" :show-message="Boolean(leadMinutes.trim())" prop="resetCreditLeadMinutes" :label="t('settings.advanceUsageTimeMinutes')" :description="t('settings.quotaReset.description')">
        <ZInput v-model="leadMinutes" type="number" min="1" max="1440" step="1" :aria-label="t('settings.resetCardAdvanceTime')" />
      </ZFormItem>
      <ZFormItem prop="resetCreditMaxWeeklyRemainingPercent" :label="t('settings.remainingWeeklyQuotaLimit')" :description="onLowWeeklyQuota ? t('settings.quotaReset.earlyThresholdHelp') : t('settings.quotaReset.thresholdHelp')">
        <ZInput v-model="maxWeeklyRemainingPercent" type="number" min="0" max="100" step="1" :placeholder="onLowWeeklyQuota ? '0' : t('common.values.unlimited')" :aria-label="t('settings.quotaReset.weeklyQuotaLimit')" />
      </ZFormItem>
    </div>
  </ZCard>
</template>
