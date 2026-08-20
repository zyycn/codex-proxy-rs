<script setup lang="ts">
import type { Account } from '@/api'
import { AlertTriangle, RefreshCw, RotateCcw, TicketCheck } from '@lucide/vue'
import dayjs from 'dayjs'
import { computed, shallowRef, watch } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { useAccountResetCredits } from '../composables/useAccountResetCredits'

const props = defineProps<{
  accountId: string
}>()

const emit = defineEmits<{
  accountUpdated: [account: Account]
}>()

const {
  availableCredits,
  availableCount,
  hasSnapshot,
  loading,
  consuming,
  loadError,
  ambiguous,
  showConfirm,
  loadCredits,
  requestConsume,
  cancelConsume,
  confirmConsume,
} = useAccountResetCredits({
  accountId: () => props.accountId,
  onAccountUpdated: account => emit('accountUpdated', account),
})

const panelOpen = shallowRef(false)
const confirmTitle = computed(() => ambiguous.value ? '确认上次重置结果？' : '立即重置额度？')
const confirmDescription = computed(() => ambiguous.value
  ? '上次请求的结果不确定。本次会复用同一个请求标识，只用于确认或完成原操作。'
  : '这会消费一张上游主动重置卡。操作完成后将立即重新读取额度窗口。')
const modalTitle = computed(() => showConfirm.value ? confirmTitle.value : '主动重置额度')
const modalDescription = computed(() => showConfirm.value
  ? confirmDescription.value
  : '查看可用重置卡，并按需提前开启新的 Codex 额度窗口。')
const triggerLabel = computed(() => {
  if (ambiguous.value)
    return '查看主动重置卡，有一项操作待确认'
  if (loadError.value) {
    return hasSnapshot.value
      ? `查看主动重置卡，查询失败，最近查询 ${availableCount.value} 张可用`
      : '查看主动重置卡，查询失败'
  }
  return hasSnapshot.value
    ? `查看主动重置卡，最近查询 ${availableCount.value} 张可用`
    : '查看主动重置卡'
})
const showTriggerCount = computed(() => hasSnapshot.value && availableCount.value > 0)
const confirmCreditTitle = computed(() =>
  availableCredits.value[0]?.title || 'Codex 主动重置卡',
)

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
  return expiry.isValid() ? `${expiry.format('YYYY-MM-DD HH:mm')} 到期` : '到期时间未知'
}
</script>

<template>
  <button
    type="button"
    class="inline-flex shrink-0 touch-manipulation items-center justify-center rounded-cp-control border-0 bg-transparent text-cp-secondary outline-none transition-[background-color,color,opacity,transform] duration-150 hover:bg-cp-subtle hover:text-cp-primary active:bg-cp-muted focus-visible:ring-2 focus-visible:ring-cp-accent-border focus-visible:ring-offset-2 focus-visible:ring-offset-cp-surface motion-safe:active:scale-[0.96] motion-reduce:transition-none"
    :class="showTriggerCount ? 'h-cp-control-sm gap-1 px-2' : 'size-cp-control-sm'"
    :aria-label="triggerLabel"
    :aria-pressed="panelOpen || undefined"
    :title="triggerLabel"
    @click="panelOpen = true"
  >
    <TicketCheck class="size-4 shrink-0" aria-hidden="true" />
    <span
      v-if="showTriggerCount"
      class="translate-y-px font-mono text-[10px] leading-none font-heavy tabular-nums"
      aria-hidden="true"
    >
      x{{ availableCount }}
    </span>
  </button>

  <BaseModal
    v-model="panelOpen"
    :title="modalTitle"
    :description="modalDescription"
    :tone="showConfirm ? 'warning' : 'neutral'"
    size="sm"
    :dismissible="!consuming"
  >
    <div v-if="showConfirm" class="grid gap-3">
      <section class="rounded-cp-control bg-cp-subtle px-4 py-3.5">
        <p class="m-0 text-[11px] font-heavy text-cp-muted-text">
          本次使用
        </p>
        <p class="mt-1.5 mb-0 text-[14px] leading-snug font-heavy text-cp-primary">
          {{ confirmCreditTitle }}
        </p>
        <p class="mt-2 mb-0 text-[12px] leading-[1.55] font-emphasis text-cp-secondary">
          只消费上游重置卡；完成后系统会回读官方额度，不会直接改写本地重置时间。
        </p>
      </section>
    </div>

    <div v-else class="grid gap-4">
      <section
        v-if="ambiguous"
        class="flex items-start gap-3 rounded-cp-control bg-cp-warning-bg px-4 py-3.5"
        role="status"
      >
        <AlertTriangle class="mt-0.5 size-4 shrink-0 text-cp-warning-text" />
        <div class="min-w-0">
          <p class="m-0 text-[12px] font-heavy text-cp-warning-text">
            上次操作结果待确认
          </p>
          <p class="mt-1 mb-0 text-[11px] leading-normal font-emphasis text-cp-secondary">
            再次确认会复用原请求标识，不会创建新的消费操作。
          </p>
        </div>
      </section>

      <section class="flex items-center gap-3 rounded-cp-control bg-cp-subtle px-4 py-3.5">
        <span
          class="inline-grid size-9 shrink-0 place-items-center rounded-cp-control bg-cp-muted text-cp-info-text"
          aria-hidden="true"
        >
          <TicketCheck class="size-4" />
        </span>
        <div class="min-w-0">
          <p class="m-0 text-[12px] font-heavy text-cp-primary">
            可用重置卡
          </p>
          <p class="mt-1 mb-0 text-[11px] leading-none font-emphasis text-cp-secondary">
            每次操作消费一张
          </p>
        </div>
        <strong class="ml-auto font-mono text-[22px] leading-none font-extrabold text-cp-primary">
          {{ availableCount }}
          <span class="ml-0.5 text-[11px] font-heavy text-cp-muted-text">张</span>
        </strong>
      </section>

      <section class="grid gap-2.5">
        <div class="flex min-h-8 items-center justify-between gap-3">
          <h3 class="m-0 text-[12px] font-heavy text-cp-muted-text">
            卡片明细
          </h3>
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

        <p
          v-if="loadError"
          class="m-0 rounded-cp-control bg-cp-danger-bg px-4 py-3 text-[11px] leading-normal font-emphasis text-cp-danger-text"
          role="status"
        >
          {{ loadError }}，请刷新重试。
        </p>

        <div v-else-if="availableCredits.length" class="grid gap-2">
          <article
            v-for="credit in availableCredits"
            :key="credit.id"
            class="flex min-w-0 items-center gap-3 rounded-cp-control bg-cp-subtle px-4 py-3"
          >
            <span class="size-1.5 shrink-0 rounded-full bg-cp-info" aria-hidden="true" />
            <div class="min-w-0">
              <p class="m-0 truncate text-[12px] font-heavy text-cp-primary">
                {{ credit.title || 'Codex 主动重置卡' }}
              </p>
              <p class="mt-1 mb-0 truncate font-mono text-[10px] font-emphasis text-cp-muted-text">
                {{ expiryLabel(credit.expiresAt) }}
              </p>
            </div>
          </article>
        </div>

        <div v-else class="overflow-hidden rounded-cp-control bg-cp-subtle">
          <BaseEmpty
            :icon="TicketCheck"
            size="sm"
            surface="none"
            title="当前没有可用重置卡"
            description="可刷新列表，重新读取上游状态"
          />
        </div>
      </section>
    </div>

    <template #footer>
      <template v-if="showConfirm">
        <BaseButton variant="ghost" :disabled="consuming" @click="cancelConsume">
          返回
        </BaseButton>
        <BaseButton variant="primary" :loading="consuming" @click="confirmConsume">
          {{ ambiguous ? '确认原操作' : '消费并重置' }}
        </BaseButton>
      </template>
      <template v-else>
        <BaseButton variant="ghost" :disabled="consuming" @click="panelOpen = false">
          关闭
        </BaseButton>
        <BaseButton
          variant="primary"
          :disabled="loading || consuming || (!ambiguous && availableCount <= 0)"
          @click="requestConsume"
        >
          <template #icon>
            <RotateCcw class="size-3.5" />
          </template>
          {{ ambiguous ? '确认原操作' : '立即重置' }}
        </BaseButton>
      </template>
    </template>
  </BaseModal>
</template>
