<script setup lang="ts">
import type { PricingRow } from './model'

import type { PricingChange } from '@/api'
import { ZButton, ZDialog, ZFormItem, ZInput, ZScrollbar } from '@codex-proxy/ui'
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { multiplierText, parseMultiplier } from './model'

defineProps<{ rows: PricingRow[], reset: boolean, saving: boolean }>()

defineEmits<{ confirm: [change: PricingChange] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const multiplier = ref('1')
const bps = computed(() => parseMultiplier(multiplier.value))
watch(open, (value) => {
  if (value)
    multiplier.value = '1'
})
</script>

<template>
  <ZDialog v-model="open" :title="reset ? t('settings.pricing.clearOverrides') : t('settings.pricing.setMultipliers')" :description="t('settings.pricing.batch.description', { count: rows.length })" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving" type="warning">
    <div class="grid gap-4">
      <p v-if="reset" class="m-0 text-z leading-relaxed text-z-text-secondary">
        {{ t('settings.pricing.batch.resetHelp') }}
      </p>
      <ZFormItem v-else :label="t('settings.pricing.targetMultiplier')" :description="t('settings.pricing.batch.multiplierHelp')">
        <ZInput v-model="multiplier" :aria-label="t('settings.pricing.targetMultiplier')" inputmode="decimal" :disabled="saving">
          <template #suffix>
            ×
          </template>
        </ZInput>
      </ZFormItem>
      <ZScrollbar max-height="16rem" class="rounded-z bg-z-fill-quaternary">
        <ul class="m-0 list-none p-3 text-z-sm">
          <li v-for="row in rows" :key="row.model" class="flex items-start justify-between gap-3 py-2">
            <span class="min-w-0 break-all font-mono">{{ row.model }}</span>
            <span class="shrink-0 font-mono text-z-text-secondary">{{ multiplierText(row.effective.multiplierBps) }} → {{ reset ? '1×' : bps === undefined ? '—' : multiplierText(bps) }}</span>
          </li>
        </ul>
      </ZScrollbar>
      <p v-if="!reset && bps === undefined" role="alert" class="m-0 text-z-sm text-z-error">
        {{ t('settings.pricing.enterLegalMultiplier') }}
      </p>
    </div>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="saving" :disabled="!rows.length || (!reset && bps === undefined)" @click="$emit('confirm', reset ? { action: 'reset' } : { action: 'multiplier', multiplierBps: bps! })">
        {{ reset ? t('settings.pricing.confirmClearOverride') : t('settings.pricing.confirmSettingMultiplier') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
