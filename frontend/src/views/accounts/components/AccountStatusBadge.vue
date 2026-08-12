<script setup lang="ts">
import type { AccountErrorReason, AccountStatus } from '@/api'
import { AlertTriangle, CircleCheck, Gauge, Power, Timer } from '@lucide/vue'
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { errorReasonLabels, statusLabels, statusTones } from '../constants'

const props = withDefaults(
  defineProps<{
    status: AccountStatus
    errorReason?: AccountErrorReason | null
    errorMessage?: string | null
    rateLimitedUntil?: string | null
    variant?: 'inline' | 'pill'
  }>(),
  {
    errorReason: null,
    errorMessage: null,
    rateLimitedUntil: null,
    variant: 'inline',
  },
)

const statusStyles = {
  success: {
    text: 'text-(--cp-success-text)',
    dot: 'bg-(--cp-success)',
    badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
    icon: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  },
  danger: {
    text: 'text-(--cp-danger-text)',
    dot: 'bg-(--cp-danger)',
    badge: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
    icon: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
  },
  warning: {
    text: 'text-(--cp-warning-text)',
    dot: 'bg-(--cp-warning)',
    badge: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
    icon: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  },
  info: {
    text: 'text-(--cp-info-text)',
    dot: 'bg-(--cp-info)',
    badge: 'bg-(--cp-info-bg) text-(--cp-info-text)',
    icon: 'bg-(--cp-info-bg) text-(--cp-info-text)',
  },
  normal: {
    text: 'text-(--cp-text-secondary)',
    dot: 'bg-(--cp-text-muted)',
    badge: 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)',
    icon: 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)',
  },
} as const

const tone = computed(() => statusTones[props.status])
const statusStyle = computed(() => statusStyles[tone.value])
const label = computed(() => statusLabels[props.status])
const reasonLabel = computed(() =>
  props.errorReason
    ? errorReasonLabels[props.errorReason]
    : null,
)
const errorText = computed(() => props.errorMessage || null)
const detailTitle = computed(() => reasonLabel.value || label.value)
const isRateLimited = computed(() =>
  props.status === 'rate_limited' && Boolean(props.rateLimitedUntil),
)
const rateLimitedRelative = computed(() => {
  const rateLimitedUntil = props.rateLimitedUntil
  if (!rateLimitedUntil)
    return null

  const until = new Date(rateLimitedUntil.replace(' ', 'T')).getTime()
  const diffMinutes = Math.round((until - Date.now()) / 60_000)
  if (Number.isNaN(diffMinutes) || diffMinutes < 1)
    return null
  if (diffMinutes < 60)
    return `剩余 ${diffMinutes} 分钟`
  const hours = Math.floor(diffMinutes / 60)
  const mins = diffMinutes % 60
  if (mins === 0)
    return `剩余 ${hours} 小时`
  return `剩余 ${hours} 小时 ${mins} 分`
})
const hasDetail = computed(() =>
  props.status === 'error' || isRateLimited.value,
)

const detailDescription = computed(() => {
  if (props.status === 'rate_limited')
    return '上游暂时限制了请求频率，系统正在冷却该账号。'
  if (props.status === 'error')
    return '该账号暂不参与调度，处理凭据后可重新测试连接。'
  return '该账号当前状态需要关注。'
})

const recoveryHint = computed(() => {
  if (props.status === 'rate_limited')
    return '冷却结束后，系统会自动恢复调度。'

  switch (props.errorReason) {
    case 'account_unverified':
    case 'access_token_expired':
    case 'credential_expired':
      return '重新授权后，账号会重新参与调度。'
    case 'credential_invalid':
      return '请更新或重新导入凭据，再次测试连接。'
    case 'account_banned':
      return '请确认上游账号状态，解除限制后再启用。'
  }
  return '重新测试连接以获取最新状态。'
})

const triggerLabel = computed(() =>
  `${detailTitle.value}。${detailDescription.value} 点击或聚焦查看详情。`,
)

const cardIcon = computed(() => {
  switch (props.status) {
    case 'error': return AlertTriangle
    case 'rate_limited': return Timer
    case 'quota_exhausted': return Gauge
    case 'disabled': return Power
    default: return CircleCheck
  }
})
</script>

<template>
  <BasePopover
    :disabled="!hasDetail"
    trigger="hover-click"
    placement="top-start"
    width="352px"
    panel-class="!p-0 text-(--cp-text-primary)"
  >
    <template #trigger="{ open }">
      <component
        :is="hasDetail ? 'button' : 'span'"
        :type="hasDetail ? 'button' : undefined"
        :aria-label="hasDetail ? triggerLabel : undefined"
        :aria-expanded="hasDetail ? open : undefined"
        :aria-haspopup="hasDetail ? 'dialog' : undefined"
        class="group inline-flex min-w-0 cursor-default border-0 bg-transparent p-0 text-left outline-none"
      >
        <span
          v-if="variant === 'pill'"
          class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] leading-none font-heavy transition-colors duration-150 motion-reduce:transition-none"
          :class="statusStyle.text"
        >
          {{ label }}
        </span>
        <span
          v-else
          class="inline-flex min-w-16 items-center gap-1.5 rounded-md px-1.5 py-1 text-[12px] leading-none font-emphasis transition-colors duration-150 motion-reduce:transition-none"
          :class="statusStyle.text"
        >
          <span aria-hidden="true" class="size-1.5 rounded-full" :class="statusStyle.dot" />
          <span>{{ label }}</span>
        </span>
      </component>
    </template>

    <section class="overflow-hidden rounded-(--cp-popover-radius)">
      <header class="flex items-start gap-3 bg-(--cp-bg-subtle) px-4 py-3">
        <span
          aria-hidden="true"
          class="inline-flex size-9 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius)"
          :class="statusStyle.icon"
        >
          <component :is="cardIcon" class="size-4.5" />
        </span>

        <div class="min-w-0 flex-1">
          <p class="m-0 text-[11px] leading-none font-heavy tracking-[0.08em] text-(--cp-text-tertiary)">
            账号状态
          </p>
          <h3 class="mt-1.5 mb-0 text-[14px] leading-5 font-heavy text-(--cp-text-primary) text-balance">
            {{ detailTitle }}
          </h3>
        </div>

        <span
          class="inline-flex h-5 shrink-0 items-center rounded-full px-2 text-[10px] leading-none font-heavy"
          :class="statusStyle.badge"
        >
          {{ label }}
        </span>
      </header>

      <div class="grid gap-3 px-4 py-3 text-[12px] leading-5">
        <p class="m-0 text-pretty font-emphasis text-(--cp-text-secondary)">
          {{ detailDescription }}
        </p>

        <div
          v-if="isRateLimited"
          class="flex items-center justify-between gap-3 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2"
        >
          <span class="font-heavy text-(--cp-text-tertiary)">预计恢复</span>
          <span class="font-mono font-emphasis tabular-nums text-(--cp-text-primary)">
            {{ rateLimitedRelative ?? props.rateLimitedUntil }}
          </span>
        </div>

        <div class="rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 py-2.5">
          <p class="m-0 text-[11px] leading-none font-heavy text-(--cp-text-tertiary)">
            建议操作
          </p>
          <p class="mt-1.5 mb-0 text-pretty font-emphasis text-(--cp-text-primary)">
            {{ recoveryHint }}
          </p>
        </div>

        <div
          v-if="errorText"
          class="overflow-hidden rounded-(--cp-input-radius-base) bg-(--cp-bg-muted)"
        >
          <div class="flex items-center justify-between gap-3 px-3 pt-2.5 pb-1.5">
            <span class="text-[11px] leading-none font-heavy text-(--cp-text-tertiary)">上游原始反馈</span>
            <span class="shrink-0 text-[10px] leading-none font-emphasis text-(--cp-text-muted)">仅供排查</span>
          </div>
          <BaseScrollbar class="bg-(--cp-bg-subtle)" max-height="124px" view-class="px-3 py-2 pr-2">
            <pre class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[11px] leading-[1.55] font-emphasis text-(--cp-text-secondary)">{{ errorText }}</pre>
          </BaseScrollbar>
        </div>
      </div>
    </section>
  </BasePopover>
</template>
