<script setup lang="ts">
import type { AccountResetCreditAutomation } from '@/api'

import { ZButton, ZEmpty, ZIconButton, ZPopover, ZSwitch } from '@codex-proxy/ui'
import { CircleHelp, Clock3 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { getAccountResetCreditAutomation, setAccountResetCreditAutomation } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/operation'

const props = defineProps<{ accountId: string }>()

const { t } = useI18n({ useScope: 'global' })

const router = useRouter()
const status = shallowRef<AccountResetCreditAutomation>()
const loadError = shallowRef('')
const switchEnabled = shallowRef(false)
const loadAction = useAsyncAction({ errorText: false })
const saveAction = useAsyncAction()
const busy = computed(() => loadAction.loading.value || saveAction.loading.value)
const resultText = computed(() => {
  switch (status.value?.lastResult) {
    case 'pending': return status.value?.enabled
      ? t('accounts.quotaReset.automation.confirming')
      : t('accounts.quotaReset.automation.unconfirmed')
    case 'expired_unknown': return t('accounts.quotaReset.automation.expiredUnconfirmed')
    case 'discovery_failed': return t('accounts.quotaReset.automation.queryFailed')
    case 'weekly_quota_above_threshold': return t('accounts.quotaReset.automation.aboveThreshold')
    case 'weekly_quota_unavailable': return t('accounts.quotaReset.automation.quotaUnavailable')
    case 'nothing_to_reset': return t('accounts.quotaReset.automation.resetNotNeeded')
    case 'not_redeemed': return t('accounts.quotaReset.automation.resetIncomplete')
    default: return ''
  }
})

async function load() {
  const accountId = props.accountId
  loadError.value = ''
  await loadAction.run(async () => {
    try {
      const result = await getAccountResetCreditAutomation({ accountId }, { silent: true })
      if (props.accountId === accountId) {
        status.value = result
        switchEnabled.value = result.enabled
      }
    }
    catch (error) {
      if (props.accountId === accountId)
        loadError.value = errorMessage(error)
    }
  })
}

async function toggle(enabled: boolean) {
  const accountId = props.accountId
  switchEnabled.value = enabled
  await saveAction.run(async () => {
    const result = await setAccountResetCreditAutomation({ accountId, enabled })
    if (props.accountId === accountId)
      status.value = result
  })
  if (props.accountId === accountId)
    switchEnabled.value = status.value?.enabled ?? false
}

watch(() => props.accountId, () => {
  status.value = undefined
  switchEnabled.value = false
  void load()
}, { immediate: true })
</script>

<template>
  <section class="grid gap-2 rounded-cp bg-cp-fill-quaternary px-4 py-3" :aria-label="t('accounts.quotaReset.automaticallyUseResetCard')">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div class="flex items-center gap-1">
        <h3 class="m-0 text-cp-sm font-heavy text-cp-text">
          {{ t('accounts.quotaReset.automaticallyUsed') }}
        </h3>
        <ZPopover trigger="click" placement="bottom-start" role="dialog">
          <template #reference="{ open, id }">
            <ZIconButton size="small" class="size-6! p-0!" :aria-label="t('accounts.quotaReset.automaticUsageRules')" :aria-expanded="open" :aria-controls="id">
              <CircleHelp class="size-3.5" />
            </ZIconButton>
          </template>
          <div class="grid max-w-64 gap-2 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
            <p>{{ t('accounts.quotaReset.automation.scheduleHelp') }}</p>
            <p>{{ t('accounts.quotaReset.automation.verifyHelp') }}</p>
          </div>
        </ZPopover>
      </div>
      <div class="flex items-center gap-3">
        <ZButton size="small" variant="text" :aria-label="t('accounts.quotaReset.setGlobalLeadTime')" @click="router.push('/settings#reset-credits')">
          <template #icon>
            <Clock3 class="size-3.5" />
          </template>
          {{ status ? t('accounts.quotaReset.minutesInAdvance', { leadMinutes: status.leadMinutes }) : t('accounts.quotaReset.leadTime') }}
        </ZButton>
        <ZSwitch
          class="reset-credit-switch"
          :model-value="switchEnabled"
          :disabled="busy || !status"
          :aria-label="t('accounts.quotaReset.automation.label')"
          @update:model-value="toggle"
        />
      </div>
    </div>
    <p v-if="status?.enabled && status.nextConsumeAtDisplay" class="text-cp-xs text-cp-text-secondary">
      {{ t('accounts.quotaReset.nextUse', { nextConsumeAtDisplay: status.nextConsumeAtDisplay }) }}
    </p>
    <p v-if="resultText" class="text-cp-xs text-cp-warning" role="status">
      {{ resultText }}
    </p>
    <ZEmpty v-if="loadError" :title="t('accounts.quotaReset.autoUsageSettingsFailedToLoad')" :description="loadError" size="small" surface="none" role="alert">
      <template #action>
        <ZButton size="small" variant="plain" :loading="loadAction.loading.value" @click="load">
          {{ t('common.actions.retry') }}
        </ZButton>
      </template>
    </ZEmpty>
  </section>
</template>

<style scoped>
.reset-credit-switch :deep(input:not(:checked) + span) {
  background-color: var(--cp-switch-unchecked-bg);
}
</style>
