<script setup lang="ts">
import type { Account, AccountResetCredit } from '@/api'
import { ZButton, ZDialog, ZEmpty, ZIconButton, ZTag } from '@codex-proxy/ui'

import { AlertTriangle, RefreshCw, TicketCheck } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useAccountResetCredits } from '../../composables/useAccountResetCredits'
import AccountQuotaCredits from './Credits.vue'
import ResetCreditAutomation from './ResetCreditAutomation.vue'
import UsageLimits from './UsageLimits.vue'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{
  consumed: [accountId: string]
}>()

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
    return '额度重置'
  return confirmation.value.ambiguous ? '确认上次重置' : '确认重置额度'
})
const triggerLabel = computed(() => {
  if (ambiguous.value)
    return '查看主动重置卡，有一项操作待确认'
  if (loadError.value) {
    return hasSnapshot.value
      ? `查看主动重置卡，查询失败，最近查询 ${availableCount.value} 张可用`
      : '查看主动重置卡，查询失败'
  }
  return hasSnapshot.value ? `查看主动重置卡，最近查询 ${availableCount.value} 张可用` : '查看主动重置卡'
})
const showTriggerCount = computed(() => hasSnapshot.value && availableCount.value > 0)
const confirmCreditTitle = computed(() => confirmation.value?.credit
  ? creditTitle(confirmation.value.credit)
  : '使用一次重置（由上游选择）')
const creditItems = computed(() => availableCredits.value.map(credit => ({
  id: credit.id,
  title: creditTitle(credit),
  expiry: credit.expiresAtDisplay ? `将于 ${credit.expiresAtDisplay} 到期` : '有效期由上游决定',
})))
const showCountOnlyAction = computed(() => !loadError.value
  && hasSnapshot.value
  && availableCount.value > 0
  && creditItems.value.length === 0)
const countLabel = computed(() => {
  if (!hasSnapshot.value)
    return loading.value ? '查询中' : '待查询'
  return `可用 ${availableCount.value} 次`
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
  if (panelOpen.value)
    confirmation.value = confirming ? { credit: consumptionCredit.value, ambiguous: ambiguous.value } : null
})

function creditTitle(credit: AccountResetCredit | undefined) {
  return credit?.title?.trim() || '用量重置'
}

function handleRequestConsume(creditId: string) {
  if (!props.account.capabilities.consumeResetCredit || loading.value || consuming.value || ambiguous.value)
    return
  selectCredit(creditId)
  requestConsume()
}
</script>

<template>
  <button
    type="button"
    class="inline-flex shrink-0 touch-manipulation items-center justify-center rounded-cp border-0 bg-transparent text-cp-text-secondary outline-none transition-[background-color,color,opacity,transform] duration-150 hover:bg-cp-fill-quaternary hover:text-cp-text active:bg-cp-fill-tertiary focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container motion-safe:active:scale-[0.96] motion-reduce:transition-none"
    :class="showTriggerCount ? 'h-cp-control-sm gap-1 px-2' : 'size-cp-control-sm'"
    :aria-label="triggerLabel"
    :aria-pressed="panelOpen || undefined"
    :title="triggerLabel"
    @click="panelOpen = true"
  >
    <TicketCheck class="size-3.5 shrink-0" />
    <span v-if="showTriggerCount" class="translate-y-px font-mono text-[10px] leading-none font-heavy tabular-nums">
      x{{ availableCount }}
    </span>
  </button>

  <ZDialog
    v-model="panelOpen"
    :title="modalTitle"
    :type="confirmation ? 'warning' : 'default'"
    :width="confirmation ? '28rem' : '36rem'"
    :show-close="!consuming" :close-on-click-modal="!consuming" :close-on-press-escape="!consuming"
  >
    <div v-if="confirmation" class="grid gap-3">
      <section class="rounded-cp bg-cp-fill-quaternary px-4 py-3.5">
        <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
          本次使用
        </p>
        <p class="mt-1.5 mb-0 text-cp-lg leading-snug font-heavy text-cp-text">
          {{ confirmCreditTitle }}
        </p>
        <p
          v-if="confirmation.credit"
          class="mt-1 mb-0 font-mono text-[10px] leading-normal font-emphasis text-cp-text-quaternary"
        >
          {{ confirmation.credit.expiresAtDisplay ? `将于 ${confirmation.credit.expiresAtDisplay} 到期` : '有效期由上游决定' }}
        </p>
      </section>
    </div>

    <div v-else class="grid gap-4">
      <section class="overflow-hidden rounded-cp bg-cp-fill-quaternary" aria-label="当前额度">
        <UsageLimits :windows="account.quota.windows" />
        <AccountQuotaCredits :credits="account.quota.credits" />
      </section>

      <ResetCreditAutomation v-if="account.capabilities.consumeResetCredit" :key="account.id" :account-id="account.id" />

      <section v-if="ambiguous" class="flex items-start gap-3 rounded-cp bg-cp-warning-container px-4 py-3.5" role="status">
        <AlertTriangle class="mt-0.5 size-4 shrink-0 text-cp-warning-on-container" />
        <div class="min-w-0 flex-1">
          <p class="m-0 text-cp-sm font-heavy text-cp-warning-on-container">
            上次操作结果待确认
          </p>
          <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
            请继续确认上次重置结果
          </p>
        </div>
        <ZButton size="small" type="primary" variant="plain" :disabled="loading || consuming || !canRequestConsume" @click="requestConsume">
          继续确认
        </ZButton>
      </section>

      <section class="grid gap-3" aria-label="可用重置卡">
        <div class="flex items-center gap-2">
          <h3 class="m-0 min-w-0 flex-1 text-cp-sm font-heavy text-cp-text">
            可用重置卡
          </h3>
          <ZTag size="small" :type="hasSnapshot && availableCount > 0 ? 'success' : 'default'" role="status">
            {{ countLabel }}
          </ZTag>
          <ZIconButton
            size="small"
            aria-label="刷新主动重置卡"
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

        <div :aria-busy="loading || consuming">
          <p
            v-if="loadError"
            class="m-0 rounded-cp bg-cp-error-container px-4 py-3 text-cp-xs leading-normal font-emphasis text-cp-error-on-container"
            role="status"
          >
            {{ loadError }}，请刷新重试
          </p>
          <p
            v-else-if="loading && !hasSnapshot"
            class="m-0 px-4 py-4 text-cp-xs font-emphasis text-cp-text-secondary"
            role="status"
          >
            正在查询可用重置次数…
          </p>
          <ul v-else-if="creditItems.length" class="m-0 grid list-none gap-2 p-0">
            <li
              v-for="credit in creditItems"
              :key="credit.id"
              class="flex items-center gap-4 rounded-cp bg-cp-fill-quaternary px-4 py-3.5"
            >
              <div class="min-w-0 flex-1">
                <p class="m-0 text-cp-sm leading-normal font-heavy wrap-anywhere text-cp-text">
                  {{ credit.title }}
                </p>
                <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
                  {{ credit.expiry }}
                </p>
              </div>
              <ZButton
                size="small"
                type="primary"
                :disabled="!account.capabilities.consumeResetCredit || loading || consuming || ambiguous || availableCount <= 0"
                :aria-label="`使用重置：${credit.title}，${credit.expiry}`"
                @click="handleRequestConsume(credit.id)"
              >
                使用重置
              </ZButton>
            </li>
          </ul>

          <ZEmpty
            v-else-if="!showCountOnlyAction"
            :icon="TicketCheck"
            size="small"
            surface="none"
            title="暂无可用重置卡"
          />
        </div>
      </section>
    </div>

    <template v-if="confirmation || showCountOnlyAction" #footer>
      <template v-if="confirmation">
        <ZButton :disabled="consuming" @click="cancelConsume">
          返回
        </ZButton>
        <ZButton type="primary" :loading="consuming" :disabled="loading || !canRequestConsume" @click="confirmConsume">
          {{ confirmation.ambiguous ? '再次确认' : '确认重置' }}
        </ZButton>
      </template>
      <ZButton
        v-else
        size="small"
        type="primary"
        :disabled="loading || consuming || ambiguous || !canRequestConsume"
        @click="requestConsume"
      >
        使用一次重置（由上游选择）
      </ZButton>
    </template>
  </ZDialog>
</template>
