<script setup lang="ts">
import { ZCard, ZFormItem, ZInput } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const maxWaitingPerKey = defineModel<string>('maxWaitingPerKey', { required: true })
const maxWaitingPerAccount = defineModel<string>('maxWaitingPerAccount', { required: true })
const keyQueueTimeoutSeconds = defineModel<string>('keyQueueTimeoutSeconds', { required: true })
const accountQueueTimeoutSeconds = defineModel<string>('accountQueueTimeoutSeconds', { required: true })
</script>

<template>
  <ZCard :title="t('settings.concurrency.title')" :description="t('settings.concurrency.description')" class="@container">
    <div class="grid min-w-0 gap-4.5 @md:grid-cols-2 @md:items-end @5xl:grid-cols-5">
      <ZFormItem
        class="col-span-full @5xl:col-span-1"
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
      <ZFormItem :show-message="Boolean(accountQueueTimeoutSeconds.trim())" prop="accountQueueTimeoutSeconds" :label="t('settings.concurrency.accountTimeout')" :description="t('settings.concurrency.accountTimeoutHelp')">
        <ZInput v-model="accountQueueTimeoutSeconds" :aria-label="t('settings.units.inSeconds', { label: t('settings.concurrency.accountTimeout') })" type="number" min="1" max="120" step="1">
          <template #suffix>
            {{ t('settings.units.seconds') }}
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
