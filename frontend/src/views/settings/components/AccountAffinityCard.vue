<script setup lang="ts">
import type { AccountAffinity } from '@/api'

import { ZCard, ZForm, ZFormItem, ZIconButton, ZInput, ZPopover, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const accountAffinity = defineModel<AccountAffinity | ''>({ required: true })
const affinityTtlHours = defineModel<string>('ttlHours', { required: true })
const maxAccountRotations = defineModel<string>('maxAccountRotations', { required: true })
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
  <ZCard :title="t('settings.affinity.title')">
    <ZForm class="max-w-6xl sm:grid-cols-2 lg:grid-cols-3">
      <ZFormItem :label="t('settings.affinity.mode')" :description="t('settings.affinity.description')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton :aria-label="t('settings.affinity.accountAffinityInstructions')" :title="undefined" :aria-expanded="open" class="size-6! hover:bg-transparent! active:bg-transparent!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
              <p>{{ t('settings.affinity.modes.relaxedHelp') }}</p>
              <p>{{ t('settings.affinity.modes.preferredHelp') }}</p>
              <p>{{ t('settings.affinity.modes.strictHelp') }}</p>
              <p>{{ t('settings.affinity.policyHelp') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZSegmented v-model="accountAffinity" :aria-label="t('settings.affinity.mode')" :options="affinityOptions" :disabled="props.disabled" class="w-full" />
      </ZFormItem>
      <ZFormItem :label="t('settings.affinity.duration')" :description="t('settings.affinity.durationHelp')">
        <ZInput v-model="affinityTtlHours" :aria-label="t('settings.affinity.durationHours')" type="number" min="1" max="720" step="1" :disabled="props.disabled">
          <template #suffix>
            {{ t('settings.affinity.hours') }}
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :label="t('settings.affinity.maxAccountSwitches')" :description="t('settings.affinity.switchLimitHelp')">
        <ZInput v-model="maxAccountRotations" :aria-label="t('settings.affinity.maxAccountSwitches')" type="number" min="0" max="31" step="1" :disabled="props.disabled" />
      </ZFormItem>
    </ZForm>
  </ZCard>
</template>
