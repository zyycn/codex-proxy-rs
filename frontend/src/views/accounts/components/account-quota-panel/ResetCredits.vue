<script setup lang="ts">
import type { Account, AccountResetCredit } from '@/api'

import { ZButton, ZDialog, ZEmpty, ZIconButton, ZTag } from '@codex-proxy/ui'
import { AlertTriangle, RefreshCw, TicketCheck } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useAccountResetCredits } from '../../composables/useAccountResetCredits'
import AccountQuotaCredits from './Credits.vue'
import ResetCreditAutomation from './ResetCreditAutomation.vue'
import UsageLimits from './UsageLimits.vue'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{
  accountUpdated: [account: Account]
  consumed: [accountId: string]
}>()

const { t } = useI18n({ useScope: 'global' })

const panelOpen = shallowRef(false)
const confirmation = shallowRef<{ credit?: AccountResetCredit, ambiguous: boolean } | null>(null)
const {
  availableCredits,
  availableCount,
  consumptionCredit,
  canRequestConsume,
  hasSnapshot,
  loading,
  consuming,
  loadError,
  ambiguous,
  showConfirm,
  loadCredits,
  selectCredit,
  requestConsume,
  cancelConsume,
  confirmConsume,
} = useAccountResetCredits({
  accountId: () => props.account.id,
  capabilities: () => props.account.capabilities,
  onConsumed: (accountId) => {
    if (props.account.id === accountId)
      panelOpen.value = false
    emit('consumed', accountId)
  },
})

const modalTitle = computed(() => {
  if (!confirmation.value)
    return t('accounts.quotaReset.limitReset')
  return confirmation.value.ambiguous
    ? t('accounts.quotaReset.confirmLastReset')
    : t('accounts.quotaReset.confirmResetAmount')
})
const triggerLabel = computed(() => {
  if (ambiguous.value)
    return t('accounts.quotaReset.pendingAction')
  if (loadError.value) {
    return hasSnapshot.value
      ? t('accounts.quotaReset.queryFailedWithCount', { count: availableCount.value })
      : t('accounts.quotaReset.queryFailedLabel')
  }
  return hasSnapshot.value
    ? t('accounts.quotaReset.availableCount', { count: availableCount.value })
    : t('accounts.quotaReset.viewCredits')
})
const showTriggerCount = computed(() => hasSnapshot.value && availableCount.value > 0)
const confirmCreditTitle = computed(() => confirmation.value?.credit
  ? creditTitle(confirmation.value.credit)
  : t('accounts.quotaReset.useResetSelectedByUpstream'))
const creditItems = computed(() => availableCredits.value.map(credit => ({
  id: credit.id,
  title: creditTitle(credit),
  expiry: credit.expiresAtDisplay
    ? t('accounts.quotaReset.expires', { expiresAtDisplay: credit.expiresAtDisplay })
    : t('accounts.quotaReset.expiryHelp'),
})))
const showCountOnlyAction = computed(() => !loadError.value
  && hasSnapshot.value
  && availableCount.value > 0
  && creditItems.value.length === 0)
const countLabel = computed(() => {
  if (!hasSnapshot.value) {
    return loading.value
      ? t('accounts.quotaReset.querying')
      : t('accounts.quotaReset.toBeQueried')
  }
  return t('accounts.quotaReset.usesAvailable', { count: availableCount.value })
})

watch(panelOpen, (isOpen) => {
  if (isOpen) {
    confirmation.value = null
    void loadCredits()
    return
  }
  if (showConfirm.value)
    cancelConsume()
})

watch(showConfirm, (confirming) => {
  if (panelOpen.value) {
    confirmation.value = confirming
      ? { credit: consumptionCredit.value, ambiguous: ambiguous.value }
      : null
  }
})

function creditTitle(credit: AccountResetCredit | undefined) {
  return credit?.title?.trim() || t('accounts.quotaReset.usageReset')
}

function handleRequestConsume(creditId: string) {
  if (!props.account.capabilities.consumeResetCredit || loading.value || consuming.value || ambiguous.value)
    return
  selectCredit(creditId)
  requestConsume()
}
</script>

<template>
  <ZIconButton
    size="small"
    class="[&>span]:inline-flex [&>span]:items-center [&>span]:gap-1"
    :class="showTriggerCount ? 'w-auto! px-2' : undefined"
    :aria-label="triggerLabel"
    :aria-pressed="panelOpen || undefined"
    @click="panelOpen = true"
  >
    <TicketCheck class="size-3.5 shrink-0" />
    <span v-if="showTriggerCount" class="translate-y-px font-mono text-[10px] leading-none font-heavy tabular-nums">
      x{{ availableCount }}
    </span>
  </ZIconButton>

  <ZDialog
    v-model="panelOpen"
    :title="modalTitle"
    :type="confirmation ? 'warning' : 'default'"
    :width="confirmation ? '28rem' : '36rem'"
    :show-close="!consuming" :close-on-click-modal="!consuming" :close-on-press-escape="!consuming"
  >
    <div v-if="confirmation" class="grid gap-3">
      <section class="rounded-z bg-z-fill-quaternary px-4 py-3.5">
        <p class="m-0 text-z-xs font-heavy text-z-text-quaternary">
          {{ t('accounts.quotaReset.thisTimeUse') }}
        </p>
        <p class="mt-1.5 mb-0 text-z-lg leading-snug font-heavy text-z-text">
          {{ confirmCreditTitle }}
        </p>
        <p
          v-if="confirmation.credit"
          class="mt-1 mb-0 font-mono text-[10px] leading-normal font-emphasis text-z-text-quaternary"
        >
          {{ confirmation.credit.expiresAtDisplay ? t('accounts.quotaReset.expires', { expiresAtDisplay: confirmation.credit.expiresAtDisplay }) : t('accounts.quotaReset.expiryHelp') }}
        </p>
      </section>
    </div>

    <div v-else class="grid gap-4">
      <section class="overflow-hidden rounded-z bg-z-fill-quaternary" :aria-label="t('accounts.quotaReset.currentQuota')">
        <UsageLimits :windows="account.quota.windows" />
      </section>

      <AccountQuotaCredits :key="account.id" :account="account" @account-updated="emit('accountUpdated', $event)" />

      <ResetCreditAutomation v-if="account.capabilities.consumeResetCredit" :key="account.id" :account-id="account.id" />

      <section v-if="ambiguous" class="flex items-start gap-3 rounded-z bg-z-warning-container px-4 py-3.5" role="status">
        <AlertTriangle class="mt-0.5 size-4 shrink-0 text-z-warning-on-container" />
        <div class="min-w-0 flex-1">
          <p class="m-0 text-z-sm font-heavy text-z-warning-on-container">
            {{ t('accounts.quotaReset.operationUnconfirmed') }}
          </p>
          <p class="mt-1 mb-0 text-z-xs leading-normal font-emphasis text-z-text-secondary">
            {{ t('accounts.quotaReset.confirmPrevious') }}
          </p>
        </div>
        <ZButton size="small" type="primary" variant="plain" :disabled="loading || consuming || !canRequestConsume" @click="requestConsume">
          {{ t('accounts.quotaReset.continueToConfirm') }}
        </ZButton>
      </section>

      <section class="grid gap-3" :aria-label="t('accounts.quotaReset.availableResetCards')">
        <div class="flex items-center gap-2">
          <h3 class="m-0 min-w-0 flex-1 text-z-sm font-heavy text-z-text">
            {{ t('accounts.quotaReset.availableResetCards') }}
          </h3>
          <ZTag size="small" :type="hasSnapshot && availableCount > 0 ? 'success' : 'default'" role="status">
            {{ countLabel }}
          </ZTag>
          <ZIconButton
            size="small"
            :aria-label="t('accounts.quotaReset.refreshCredits')"
            :loading="loading"
            :disabled="loading || consuming"
            @click="loadCredits"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
            </template>
            <RefreshCw class="size-3.5" />
          </ZIconButton>
        </div>

        <div v-if="!loadError" :aria-busy="loading || consuming">
          <p
            v-if="loading && !hasSnapshot"
            class="m-0 px-4 py-4 text-z-xs font-emphasis text-z-text-secondary"
            role="status"
          >
            {{ t('accounts.quotaReset.queryingAvailableResetTimes') }}
          </p>
          <ul v-else-if="creditItems.length" class="m-0 grid list-none gap-2 p-0">
            <li
              v-for="credit in creditItems"
              :key="credit.id"
              class="flex items-center gap-4 rounded-z bg-z-fill-quaternary px-4 py-3.5"
            >
              <div class="min-w-0 flex-1">
                <p class="m-0 text-z-sm leading-normal font-heavy wrap-anywhere text-z-text">
                  {{ credit.title }}
                </p>
                <p class="mt-1 mb-0 text-z-xs leading-normal font-emphasis text-z-text-secondary">
                  {{ credit.expiry }}
                </p>
              </div>
              <ZButton
                size="small"
                type="primary"
                :disabled="!account.capabilities.consumeResetCredit || loading || consuming || ambiguous || availableCount <= 0"
                :aria-label="t('accounts.quotaReset.useResetCredit', { title: credit.title, expiry: credit.expiry })"
                @click="handleRequestConsume(credit.id)"
              >
                {{ t('accounts.quotaReset.useReset') }}
              </ZButton>
            </li>
          </ul>

          <ZEmpty
            v-else-if="!showCountOnlyAction"
            :icon="TicketCheck"
            size="small"
            surface="none"
            :title="t('accounts.quotaReset.noResetCardAvailableYet')"
          />
        </div>
      </section>
    </div>

    <template v-if="confirmation || showCountOnlyAction" #footer>
      <template v-if="confirmation">
        <ZButton :disabled="consuming" @click="cancelConsume">
          {{ t('common.actions.back') }}
        </ZButton>
        <ZButton type="primary" :loading="consuming" :disabled="loading || !canRequestConsume" @click="confirmConsume">
          {{ confirmation.ambiguous ? t('accounts.quotaReset.confirmAgain') : t('accounts.quotaReset.confirmReset') }}
        </ZButton>
      </template>
      <ZButton
        v-else
        size="small"
        type="primary"
        :disabled="loading || consuming || ambiguous || !canRequestConsume"
        @click="requestConsume"
      >
        {{ t('accounts.quotaReset.useResetSelectedByUpstream') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
