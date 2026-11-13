<script setup lang="ts">
import { ZCard, ZFormItem, ZInput } from '@codex-proxy/ui'

import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<string>({ required: true })
</script>

<template>
  <ZCard :title="t('settings.history.title')" :description="t('settings.history.retentionHelp')">
    <div class="grid min-w-0 gap-4.5 max-w-6xl sm:grid-cols-2 sm:items-end">
      <ZFormItem :show-message="Boolean(model.trim())" prop="usageRetentionDays" :label="t('settings.retentionDays')" :description="t('settings.history.retentionConstraints')">
        <ZInput v-model="model" :aria-label="t('settings.history.retentionDaysLabel')" type="number" min="1" step="1" :placeholder="t('settings.enterDays')">
          <template #suffix>
            {{ t('settings.day') }}
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
