<script setup lang="ts">
import { ZCard, ZFormItem, ZIconButton, ZInput, ZPopover, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const maxWaitingPerKey = defineModel<string>('maxWaitingPerKey', { required: true })
const maxWaitingPerAccount = defineModel<string>('maxWaitingPerAccount', { required: true })
const keyQueueTimeoutSeconds = defineModel<string>('keyQueueTimeoutSeconds', { required: true })
const accountQueueTimeoutSeconds = defineModel<string>('accountQueueTimeoutSeconds', { required: true })
const switchAccountOnQueueFailure = defineModel<boolean>('switchAccountOnQueueFailure', { required: true })
const openaiGuardianReservedConcurrency = defineModel<string>('openaiGuardianReservedConcurrency', { required: true })
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
  <ZCard :title="t('settings.concurrency.title')">
    <div class="grid min-w-0 gap-4.5 max-w-6xl sm:grid-cols-2 sm:items-end">
      <ZFormItem
        :show-message="Boolean(maxConcurrentPerAccount.trim())"
        prop="maxConcurrentPerAccount"
        :label="t('settings.concurrency.accountLimit.label')"
        :description="t('settings.concurrency.accountLimit.help')"
      >
        <ZInput
          v-model="maxConcurrentPerAccount"
          :aria-label="t('settings.concurrency.accountLimit.label')"
          type="number"
          min="0"
          max="4294967295"
          step="1"
        />
      </ZFormItem>

      <ZFormItem :show-message="Boolean(openaiGuardianReservedConcurrency.trim())" prop="openaiGuardianReservedConcurrency" :label="t('settings.concurrency.autoReview.label')" :description="t('settings.concurrency.autoReview.description')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton variant="link" :aria-label="t('settings.concurrency.autoReview.helpTitle')" :aria-expanded="open" class="size-6!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
              <p>{{ t('settings.scheduling.autoReviewSlotsHelp') }}</p>
              <p>{{ t('settings.scheduling.autoReviewQueueHelp') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZInput v-model="openaiGuardianReservedConcurrency" :aria-label="t('settings.concurrency.autoReview.label')" type="number" min="0" max="4294967295" step="1" />
      </ZFormItem>
      <ZFormItem :show-message="Boolean(maxWaitingPerKey.trim())" prop="maxWaitingPerKey" :label="t('settings.concurrency.keyQueueCapacity')" :description="t('settings.concurrency.keyQueueHelp')">
        <ZInput v-model="maxWaitingPerKey" :aria-label="t('settings.concurrency.keyQueueCapacity')" type="number" min="0" max="1000" step="1" />
      </ZFormItem>
      <ZFormItem :show-message="Boolean(keyQueueTimeoutSeconds.trim())" prop="keyQueueTimeoutSeconds" :label="t('settings.concurrency.keyTimeout')" :description="t('settings.concurrency.keyTimeoutHelp')">
        <ZInput v-model="keyQueueTimeoutSeconds" :aria-label="t('settings.units.inSeconds', { label: t('settings.concurrency.keyTimeout') })" type="number" min="1" max="120" step="1">
          <template #suffix>
            {{ t('settings.units.seconds') }}
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :show-message="Boolean(maxWaitingPerAccount.trim())" prop="maxWaitingPerAccount" :label="t('settings.concurrency.accountQueueCapacity')" :description="t('settings.concurrency.accountQueueHelp')">
        <ZInput v-model="maxWaitingPerAccount" :aria-label="t('settings.concurrency.accountQueueCapacity')" type="number" min="0" max="1000" step="1" />
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
      <ZFormItem :show-message="Boolean(accountQueueTimeoutSeconds.trim())" prop="accountQueueTimeoutSeconds" :label="t('settings.concurrency.accountTimeout')" :description="t('settings.concurrency.accountTimeoutHelp')">
        <ZInput v-model="accountQueueTimeoutSeconds" :aria-label="t('settings.units.inSeconds', { label: t('settings.concurrency.accountTimeout') })" type="number" min="1" max="120" step="1">
          <template #suffix>
            {{ t('settings.units.seconds') }}
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :label="t('settings.concurrency.queueFailure')" :description="t('settings.concurrency.queueFailureHelp')">
        <template #label-extra>
          <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #reference="{ open }">
              <ZIconButton variant="link" :aria-label="t('settings.concurrency.queueFailureHelpTitle')" :aria-expanded="open" class="size-6!">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </ZIconButton>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
              <p>{{ t('settings.concurrency.queueFailureAttempt') }}</p>
              <p>{{ t('settings.concurrency.queueFailureRestrictions') }}</p>
            </div>
          </ZPopover>
        </template>
        <ZSegmented
          :model-value="switchAccountOnQueueFailure ? 'switch' : 'reject'"
          :aria-label="t('settings.concurrency.queueFailure')"
          :options="queueFailureOptions"
          @update:model-value="switchAccountOnQueueFailure = $event === 'switch'"
        />
      </ZFormItem>
    </div>
  </ZCard>
</template>
