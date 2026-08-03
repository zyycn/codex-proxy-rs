<script setup lang="ts">
import { AlertTriangle, CircleCheck, Gauge, Power, Timer } from '@lucide/vue'
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import { errorReasonLabels, statusLabels, statusTones } from '../constants'

const props = withDefaults(
  defineProps<{
    status: string
    /** `status === 'error'` 时的具体原因（对应后端 `errorReason`）。 */
    errorReason?: string | null
    /** 最近一次失败的上游错误原文（对应后端 `errorMessage`），作为辅助证据展示。 */
    errorMessage?: string | null
    // 429 临时限流到期时间（对应后端 `quota.rateLimitedUntil`）；`status === 'rate_limited'` 时有值。
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

const tone = computed(() => statusTones[props.status])
const label = computed(() => statusLabels[props.status] || props.status)
/** 分类原因（受控枚举映射文案）；`status === 'error'` 时有值。 */
const reasonLabel = computed(() => {
  if (props.errorReason)
    return errorReasonLabels[props.errorReason] || props.errorReason
  return null
})
/** 上游错误原文（辅助证据）；有则显、无则隐。 */
const errorText = computed(() => props.errorMessage || null)
// 429 临时限流中且带到期时间。
const hasRateLimit = computed(() =>
  props.status === 'rate_limited' && Boolean(props.rateLimitedUntil),
)
/** 限流到期相对时间：如「剩余 12 分钟」。到点返回 null。 */
const rateLimitedRelative = computed(() => {
  if (!hasRateLimit.value)
    return null
  const until = new Date(props.rateLimitedUntil!.replace(' ', 'T')).getTime()
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
// `error` 状态始终显示弹窗（即使 reason/message 为空也展示状态卡片），
// `rate_limited` 需要带到期时间才显示；其余状态不弹窗。
const hasDetail = computed(() =>
  props.status === 'error' || hasRateLimit.value,
)
/** 弹窗明细区是否渲染（错误原因 / 限流到期 / 错误反馈至少一项）。 */
const hasDetailRows = computed(() =>
  hasRateLimit.value || Boolean(reasonLabel.value) || Boolean(errorText.value),
)

const textClass = computed(() => {
  if (tone.value === 'success') {
    return 'text-(--cp-success-text)'
  }
  if (tone.value === 'danger') {
    return 'text-(--cp-danger-text)'
  }
  if (tone.value === 'warning') {
    return 'text-(--cp-warning-text)'
  }
  if (tone.value === 'info') {
    return 'text-(--cp-info-text)'
  }
  return 'text-(--cp-text-secondary)'
})

const dotClass = computed(() => {
  if (tone.value === 'success') {
    return 'bg-(--cp-success)'
  }
  if (tone.value === 'danger') {
    return 'bg-(--cp-danger)'
  }
  if (tone.value === 'warning') {
    return 'bg-(--cp-warning)'
  }
  if (tone.value === 'info') {
    return 'bg-(--cp-info)'
  }
  return 'bg-(--cp-text-muted)'
})

/** 状态卡片左侧图标块：状态色 soft-bg + 语义图标。 */
const cardIcon = computed(() => {
  switch (props.status) {
    case 'error': return AlertTriangle
    case 'rate_limited': return Timer
    case 'quota_exhausted': return Gauge
    case 'disabled': return Power
    default: return CircleCheck
  }
})
const iconBlockClass = computed(() => {
  if (tone.value === 'success') {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }
  if (tone.value === 'danger') {
    return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
  }
  if (tone.value === 'warning') {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }
  if (tone.value === 'info') {
    return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  }
  return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
})
</script>

<template>
  <BasePopover
    :disabled="!hasDetail"
    trigger="hover"
    placement="top-start"
    :width="240"
    panel-class="!p-3"
  >
    <template #trigger>
      <span
        v-if="variant === 'pill'"
        class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] font-heavy"
        :class="textClass"
      >
        {{ label }}
      </span>
      <span
        v-else
        class="inline-flex min-w-16 items-center gap-1.5 text-[12px] leading-none font-emphasis"
        :class="textClass"
      >
        <span class="size-1.5 rounded-full" :class="dotClass" />
        <span>{{ label }}</span>
      </span>
    </template>

    <div
      class="flex gap-3 text-[12px] leading-none"
      :class="hasDetailRows ? 'items-start' : 'items-center'"
    >
      <!-- 状态图标块：soft 状态色背景圆角块 -->
      <span
        class="inline-flex size-8 shrink-0 items-center justify-center rounded-(--cp-icon-button-radius)"
        :class="iconBlockClass"
      >
        <component :is="cardIcon" class="size-4" />
      </span>

      <div class="min-w-0 flex-1">
        <!-- 状态标题行：只显示状态名 -->
        <div class="font-heavy text-(--cp-text-primary)">
          {{ label }}
        </div>

        <div v-if="hasDetailRows" class="mt-2 border-t border-(--cp-divider-subtle) pt-2">
          <!-- 错误：具体原因独立成行 -->
          <div v-if="reasonLabel" class="leading-4 text-(--cp-text-secondary)">
            {{ reasonLabel }}
          </div>
          <!-- 限流中：显示相对到期时间 -->
          <div v-if="hasRateLimit" class="flex items-baseline gap-2">
            <span class="w-20 shrink-0 font-semibold text-(--cp-text-tertiary)">限流到期</span>
            <span class="font-medium text-(--cp-text-primary)">
              {{ rateLimitedRelative ?? props.rateLimitedUntil }}
            </span>
          </div>
          <!-- 错误：显示上游反馈原文 -->
          <div v-if="errorText" class="mt-1 flex items-start gap-2">
            <span class="w-14 shrink-0 pt-px font-semibold leading-3.5 text-(--cp-text-tertiary)">上游反馈</span>
            <span class="min-w-0 flex-1 wrap-break-word leading-3.5 text-(--cp-text-secondary)">
              {{ errorText }}
            </span>
          </div>
        </div>
      </div>
    </div>
  </BasePopover>
</template>
