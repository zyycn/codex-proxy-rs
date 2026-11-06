<script setup lang="ts">
import { ZCard, ZFormItem, ZInput } from '@codex-proxy/ui'

import { Timer, Zap } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const refreshMarginSeconds = defineModel<string>('refreshMarginSeconds', { required: true })
const refreshConcurrency = defineModel<string>('refreshConcurrency', { required: true })
</script>

<template>
  <ZCard :title="t('settings.tokenRefresh.title')">
    <div class="grid min-w-0 gap-4.5 max-w-6xl sm:grid-cols-2">
      <ZFormItem :show-message="Boolean(refreshMarginSeconds.trim())" prop="refreshMarginSeconds" :label="t('settings.tokenRefresh.leadTimeLabel')" :description="t('settings.tokenRefresh.leadTimeHelp')">
        <ZInput v-model="refreshMarginSeconds" :aria-label="t('settings.units.inSeconds', { label: t('settings.tokenRefresh.leadTimeLabel') })" type="number">
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            {{ t('settings.units.seconds') }}
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :show-message="Boolean(refreshConcurrency.trim())" prop="refreshConcurrency" :label="t('settings.tokenRefresh.concurrencyLabel')" :description="t('settings.tokenRefresh.concurrencyHelp')">
        <ZInput v-model="refreshConcurrency" :aria-label="t('settings.tokenRefresh.concurrencyLabel')" type="number">
          <template #prefix>
            <Zap class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
