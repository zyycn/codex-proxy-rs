<script setup lang="ts">
import { ZCard, ZFormItem, ZIconButton, ZInput, ZPopover, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'

const { t } = useI18n({ useScope: 'global' })

const switchAccountOnQueueFailure = defineModel<boolean>('switchAccountOnQueueFailure', { required: true })
const maxAccountRotations = defineModel<string>('maxAccountRotations', { required: true })
const maxRetryAfterSeconds = defineModel<string>('maxRetryAfterSeconds', { required: true })
const queueFailureOptions = computed(() => [
  {
    value: 'reject',
    label: t('settings.concurrency.returnError'),
  },
  {
    value: 'switch',
    label: t('settings.concurrency.trySwitch'),
  },
])
</script>

<template>
  <ZCard :title="t('settings.concurrency.retryTitle')" :description="t('settings.concurrency.retryDescription')" class="@container">
    <div class="grid min-w-0 gap-4.5 @md:grid-cols-2 @md:items-end @xl:grid-cols-3">
      <ZFormItem :show-message="Boolean(maxAccountRotations.trim())" prop="maxAccountRotations" :label="t('settings.affinity.maxAccountSwitches')" :description="t('settings.affinity.switchLimitHelp')">
        <ZInput v-model="maxAccountRotations" :aria-label="t('settings.affinity.maxAccountSwitches')" type="number" min="0" max="31" step="1" />
      </ZFormItem>
      <ZFormItem :show-message="Boolean(maxRetryAfterSeconds.trim())" prop="maxRetryAfterSeconds" :label="t('settings.concurrency.retryAfter.label')" :description="t('settings.concurrency.retryAfter.help')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton variant="link" :aria-label="t('settings.concurrency.retryAfter.helpTitle')" :aria-expanded="open" class="size-6!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
              <p>{{ t('settings.concurrency.retryAfter.waitLimit') }}</p>
              <p>{{ t('settings.concurrency.retryAfter.zeroLimit') }}</p>
              <p>{{ t('settings.concurrency.retryAfter.switchConditions') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZInput v-model="maxRetryAfterSeconds" :aria-label="t('settings.units.inSeconds', { label: t('settings.concurrency.retryAfter.label') })" type="number" min="0" max="4294967295" step="1">
          <template #suffix>
            {{ t('settings.units.seconds') }}
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :label="t('settings.concurrency.queueFailure')" :description="t('settings.concurrency.queueFailureDescription')">
        <template #label-extra>
          <HelpPopover :label="t('settings.concurrency.queueFailureHelpTitle')">
            <div class="space-y-2">
              <p class="m-0">
                {{ t('settings.concurrency.queueFailureHelp') }}
              </p>
              <p class="m-0">
                {{ t('settings.concurrency.queueFailureAttempt') }}
              </p>
              <p class="m-0">
                {{ t('settings.concurrency.queueFailureRestrictions') }}
              </p>
            </div>
          </HelpPopover>
        </template>
        <ZSegmented
          :model-value="switchAccountOnQueueFailure ? 'switch' : 'reject'"
          :aria-label="t('settings.concurrency.queueFailure')"
          :options="queueFailureOptions"
          class="w-full"
          @update:model-value="switchAccountOnQueueFailure = $event === 'switch'"
        />
      </ZFormItem>
    </div>
  </ZCard>
</template>
