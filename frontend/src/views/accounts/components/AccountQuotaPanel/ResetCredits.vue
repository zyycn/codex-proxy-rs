<script setup lang="ts">
import type { Account, AccountResetCredit } from '@/api'
import { AlertTriangle, RefreshCw, TicketCheck } from '@lucide/vue'
import dayjs from 'dayjs'
import utc from 'dayjs/plugin/utc'
import { computed, shallowRef, watch } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { useAccountResetCredits } from '../../composables/useAccountResetCredits'
import UsageLimits from './UsageLimits.vue'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{
  accountUpdated: [account: Account]
}>()

dayjs.extend(utc)

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
  onAccountUpdated: account => emit('accountUpdated', account),
})

const panelOpen = shallowRef(false)
const modalTitle = computed(() => {
  if (!showConfirm.value)
    return '额度重置'
  return ambiguous.value ? '确认上次重置' : '确认重置额度'
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
const confirmCreditTitle = computed(() => creditTitle(consumptionCredit.value))
const creditItems = computed(() => availableCredits.value.map(credit => ({
  id: credit.id,
  title: creditTitle(credit),
  expiry: expiryLabel(credit.expiresAt),
})))
const countLabel = computed(() => {
  if (!hasSnapshot.value)
    return loading.value ? '查询中' : '待查询'
  return `可用 ${availableCount.value} 次`
})

watch(panelOpen, (isOpen) => {
  if (isOpen) {
    void loadCredits()
    return
  }
  if (showConfirm.value)
    cancelConsume()
})

function expiryLabel(value: string | null) {
  if (!value)
    return '有效期由上游决定'
  const expiry = dayjs(value)
  return expiry.isValid() ? `将于 ${expiry.utcOffset(8).format('YYYY-MM-DD HH:mm')} 到期` : '到期时间未知'
}

function creditTitle(credit: AccountResetCredit | undefined) {
  return credit?.title?.trim() || '用量重置'
}

function handleRequestConsume(creditId: string) {
  if (loading.value || consuming.value || ambiguous.value)
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
    <TicketCheck class="size-4 shrink-0" />
    <span v-if="showTriggerCount" class="translate-y-px font-mono text-[10px] leading-none font-heavy tabular-nums">
      x{{ availableCount }}
    </span>
  </button>

  <BaseModal
    v-model="panelOpen"
    :title="modalTitle"
    :tone="showConfirm ? 'warning' : 'neutral'"
    :size="showConfirm ? 'sm' : 'md'"
    :dismissible="!consuming"
  >
    <div v-if="showConfirm" class="grid gap-3">
      <section class="rounded-cp bg-cp-fill-quaternary px-4 py-3.5">
        <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
          本次使用
        </p>
        <p class="mt-1.5 mb-0 text-cp-lg leading-snug font-heavy text-cp-text">
          {{ confirmCreditTitle }}
        </p>
        <p
          v-if="consumptionCredit"
          class="mt-1 mb-0 font-mono text-[10px] leading-normal font-emphasis text-cp-text-quaternary"
        >
          {{ expiryLabel(consumptionCredit.expiresAt) }}
        </p>
      </section>
    </div>

    <div v-else class="grid gap-4">
      <UsageLimits :windows="account.quota.windows" />

      <section v-if="ambiguous" class="flex items-start gap-3 rounded-cp bg-cp-warning-container px-4 py-3.5" role="status">
        <AlertTriangle class="mt-0.5 size-4 shrink-0 text-cp-warning-on-container" />
        <div class="min-w-0 flex-1">
          <p class="m-0 text-cp-sm font-heavy text-cp-warning-on-container">
            上次操作结果待确认
          </p>
          <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
            请继续确认上次重置结果。
          </p>
        </div>
        <BaseButton size="sm" variant="soft" :disabled="loading || consuming || !canRequestConsume" @click="requestConsume">
          继续确认
        </BaseButton>
      </section>

      <section class="overflow-hidden rounded-cp bg-cp-fill-quaternary" aria-label="使用限额重置">
        <div class="flex items-center gap-3 pt-4 pr-3 pb-1 pl-4">
          <h3 class="m-0 min-w-0 flex-1 text-cp-sm font-heavy text-cp-text">
            使用限额重置
          </h3>
          <span
            class="shrink-0 rounded-cp-sm px-2 py-1 text-cp-xs leading-none font-heavy tabular-nums"
            :class="hasSnapshot && availableCount > 0 ? 'bg-cp-success-container text-cp-success-on-container' : 'bg-cp-fill-tertiary text-cp-text-secondary'"
            role="status"
          >
            {{ countLabel }}
          </span>
          <BaseIconButton
            variant="ghost"
            size="sm"
            label="刷新主动重置卡"
            :loading="loading"
            :disabled="loading || consuming"
            @click="loadCredits"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
            </template>
            <RefreshCw class="size-3.5" />
          </BaseIconButton>
        </div>

        <div :aria-busy="loading || consuming">
          <p
            v-if="loadError"
            class="mx-4 mt-0 mb-4 rounded-cp bg-cp-error-container px-4 py-3 text-cp-xs leading-normal font-emphasis text-cp-error-on-container"
            role="status"
          >
            {{ loadError }}，请刷新重试。
          </p>
          <p
            v-else-if="loading && !hasSnapshot"
            class="m-0 px-4 pt-1 pb-4 text-cp-xs font-emphasis text-cp-text-secondary"
            role="status"
          >
            正在查询可用重置次数…
          </p>
          <ul v-else-if="creditItems.length" class="m-0 list-none px-0 pt-0 pb-1">
            <li
              v-for="credit in creditItems"
              :key="credit.id"
              class="flex items-center gap-4 px-4 py-3"
            >
              <div class="min-w-0 flex-1">
                <p class="m-0 text-cp-sm leading-normal font-heavy wrap-anywhere text-cp-text">
                  {{ credit.title }}
                </p>
                <p class="mt-1 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
                  {{ credit.expiry }}
                </p>
              </div>
              <BaseButton
                size="sm"
                variant="primary"
                :disabled="loading || consuming || ambiguous || availableCount <= 0"
                :aria-label="`使用重置：${credit.title}，${credit.expiry}`"
                @click="handleRequestConsume(credit.id)"
              >
                使用重置
              </BaseButton>
            </li>
          </ul>

          <BaseEmpty
            v-else
            :icon="TicketCheck"
            size="sm"
            surface="none"
            title="当前没有可用重置次数"
            description="可刷新列表，重新读取上游状态"
          />
        </div>
      </section>
    </div>

    <template v-if="showConfirm" #footer>
      <BaseButton variant="secondary" :disabled="consuming" @click="cancelConsume">
        返回
      </BaseButton>
      <BaseButton variant="primary" :loading="consuming" @click="confirmConsume">
        {{ ambiguous ? '再次确认' : '确认重置' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
