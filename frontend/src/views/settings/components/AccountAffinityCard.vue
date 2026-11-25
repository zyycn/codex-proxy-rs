<script setup lang="ts">
import type { AccountAffinity } from '@/api'

import { ZCard, ZFormItem, ZIconButton, ZInput, ZPopover, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const accountAffinity = defineModel<AccountAffinity | ''>({ required: true })
const affinityTtlHours = defineModel<string>('ttlHours', { required: true })
const affinityOptions = computed(() => ([
  {
    label: t('settings.affinity.modes.relaxed'),
    value: 'relaxed',
  },
  {
    label: t('settings.affinity.modes.preferred'),
    value: 'preferred',
  },
  {
    label: t('settings.affinity.modes.strict'),
    value: 'strict',
  },
]))
</script>

<template>
  <ZCard :title="t('settings.affinity.title')" :description="t('settings.affinity.description')" class="@container">
    <div class="grid min-w-0 gap-4.5 @md:grid-cols-2 @md:items-end">
      <ZFormItem :show-message="false" prop="accountAffinity" :label="t('settings.affinity.mode')" :description="t('settings.affinity.policyHelp')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton variant="link" :aria-label="t('settings.affinity.accountAffinityInstructions')" :aria-expanded="open" class="size-6!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
              <p>{{ t('settings.affinity.modes.relaxedHelp') }}</p>
              <p>{{ t('settings.affinity.modes.preferredHelp') }}</p>
              <p>{{ t('settings.affinity.modes.strictHelp') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZSegmented v-model="accountAffinity" :aria-label="t('settings.affinity.mode')" :options="affinityOptions" :disabled="props.disabled" class="w-full" />
      </ZFormItem>
      <ZFormItem :show-message="Boolean(affinityTtlHours.trim())" prop="sessionAffinityTtlHours" :label="t('settings.affinity.duration')" :description="t('settings.affinity.durationHelp')">
        <ZInput v-model="affinityTtlHours" :aria-label="t('settings.affinity.durationHours')" type="number" min="1" max="720" step="1" :disabled="props.disabled">
          <template #suffix>
            {{ t('settings.affinity.hours') }}
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
