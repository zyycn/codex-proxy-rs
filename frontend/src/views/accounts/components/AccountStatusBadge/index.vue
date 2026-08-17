<script setup lang="ts">
import type { AccountErrorReason, AccountStatus } from '@/api'
import { computed } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useUiClock } from '@/composables/useUiClock'
import { resolveAccountStatusPresentation } from './presenter'

const props = withDefaults(
  defineProps<{
    status: AccountStatus
    errorReason?: AccountErrorReason | null
    errorMessage?: string | null
    rateLimitedUntil?: string | null
    nextRefreshAt?: string | null
    variant?: 'inline' | 'pill'
  }>(),
  {
    errorReason: null,
    errorMessage: null,
    rateLimitedUntil: null,
    nextRefreshAt: null,
    variant: 'inline',
  },
)

const now = useUiClock()
const presentation = computed(() => resolveAccountStatusPresentation({
  status: props.status,
  errorReason: props.errorReason,
  errorMessage: props.errorMessage,
  rateLimitedUntil: props.rateLimitedUntil,
  nextRefreshAt: props.nextRefreshAt,
  now: now.value.getTime(),
}))
</script>

<template>
  <BasePopover
    :disabled="!presentation.hasDetail"
    trigger="hover-click"
    placement="top-start"
  >
    <template #trigger="{ open }">
      <component
        :is="presentation.hasDetail ? 'button' : 'span'"
        :type="presentation.hasDetail ? 'button' : undefined"
        :aria-label="presentation.hasDetail ? presentation.triggerLabel : undefined"
        :aria-expanded="presentation.hasDetail ? open : undefined"
        :aria-haspopup="presentation.hasDetail ? 'dialog' : undefined"
        class="group inline-flex min-w-0 cursor-default border-0 bg-transparent p-0 text-left outline-none"
      >
        <span
          v-if="variant === 'pill'"
          class="inline-flex h-7 shrink-0 items-center rounded-full px-2.5 text-[12px] leading-none font-heavy transition-colors duration-150 motion-reduce:transition-none"
          :class="presentation.statusStyle.text"
        >
          {{ presentation.label }}
        </span>
        <span
          v-else
          class="inline-flex min-w-16 items-center gap-1.5 rounded-md px-1.5 py-1 text-[12px] leading-none font-emphasis transition-colors duration-150 motion-reduce:transition-none"
          :class="presentation.statusStyle.text"
        >
          <span aria-hidden="true" class="size-1.5 rounded-full" :class="presentation.statusStyle.dot" />
          <span>{{ presentation.label }}</span>
        </span>
      </component>
    </template>

    <section class="w-88 overflow-hidden rounded-cp-overlay">
      <header class="flex items-start gap-3 bg-cp-subtle px-4 py-3">
        <span
          aria-hidden="true"
          class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp-control"
          :class="presentation.statusStyle.icon"
        >
          <component :is="presentation.icon" class="size-4.5" />
        </span>

        <div class="min-w-0 flex-1">
          <p class="m-0 text-[11px] leading-none font-heavy tracking-[0.08em] text-cp-tertiary">
            账号状态
          </p>
          <h3 class="mt-1.5 mb-0 text-[14px] leading-5 font-heavy text-cp-primary text-balance">
            {{ presentation.title }}
          </h3>
        </div>

        <span
          class="inline-flex h-5 shrink-0 items-center rounded-full px-2 text-[10px] leading-none font-heavy"
          :class="presentation.statusStyle.badge"
        >
          {{ presentation.label }}
        </span>
      </header>

      <div class="grid gap-3 px-4 py-3 text-[12px] leading-5">
        <p class="m-0 text-pretty font-emphasis text-cp-secondary">
          {{ presentation.description }}
        </p>

        <div
          v-if="presentation.rateLimitRecovery"
          class="flex items-center justify-between gap-3 rounded-cp-control bg-cp-subtle px-3 py-2"
        >
          <span class="font-heavy text-cp-tertiary">预计恢复</span>
          <span class="font-mono font-emphasis tabular-nums text-cp-primary">
            {{ presentation.rateLimitRecovery }}
          </span>
        </div>

        <div
          v-if="presentation.nextRefreshDisplay"
          class="flex items-center justify-between gap-3 rounded-cp-control bg-cp-subtle px-3 py-2"
        >
          <span class="font-heavy text-cp-tertiary">下次尝试</span>
          <span class="font-mono font-emphasis tabular-nums text-cp-primary">
            {{ presentation.nextRefreshDisplay }}
          </span>
        </div>

        <div class="rounded-cp-control bg-cp-subtle px-3 py-2.5">
          <p class="m-0 text-[11px] leading-none font-heavy text-cp-tertiary">
            建议操作
          </p>
          <p class="mt-1.5 mb-0 text-pretty font-emphasis text-cp-primary">
            {{ presentation.recoveryHint }}
          </p>
        </div>

        <div
          v-if="presentation.errorText"
          class="overflow-hidden rounded-cp-control bg-cp-muted"
        >
          <div class="flex items-center justify-between gap-3 px-3 pt-2.5 pb-1.5">
            <span class="text-[11px] leading-none font-heavy text-cp-tertiary">上游原始反馈</span>
            <span class="shrink-0 text-[10px] leading-none font-emphasis text-cp-muted-text">仅供排查</span>
          </div>
          <BaseScrollbar class="bg-cp-subtle" max-height="124px">
            <div class="px-3 py-2">
              <pre class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[11px] leading-[1.55] font-emphasis text-cp-secondary">{{ presentation.errorText }}</pre>
            </div>
          </BaseScrollbar>
        </div>
      </div>
    </section>
  </BasePopover>
</template>
