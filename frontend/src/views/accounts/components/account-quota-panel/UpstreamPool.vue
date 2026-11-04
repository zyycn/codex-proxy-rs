<script setup lang="ts">
import type { AccountQuotaWindow, AccountUpstream } from '@/api'
import { ZPopover, ZScrollbar } from '@codex-proxy/ui'
import { Gauge, Network, Users } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { poolNodes } from './pool'
import PoolChart from './PoolChart.vue'

const props = defineProps<{ upstream: AccountUpstream | null, windows: AccountQuotaWindow[] }>()
const { t } = useI18n({ useScope: 'global' })
const pool = computed(() => props.upstream?.pool)
const nodes = computed(() => poolNodes(props.windows, pool.value?.accountCount))
const plans = computed(() => {
  const sorted = pool.value?.plans.slice().sort((left, right) => right.count - left.count) ?? []
  return {
    visible: sorted.slice(0, 3),
    overflow: sorted.slice(3),
  }
})
</script>

<template>
  <section class="grid min-w-0 gap-1" :aria-label="t('accounts.upstream.pool')">
    <PoolChart v-if="nodes.length" :nodes="nodes" />
    <div v-if="pool || upstream?.maxConcurrency != null || upstream?.requestsPerMinute != null" class="flex min-h-5 min-w-0 flex-wrap items-end justify-between gap-x-3 gap-y-1.5">
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-z-xs leading-none tabular-nums text-z-text-secondary">
        <span v-if="pool" class="inline-flex items-center gap-1 whitespace-nowrap" :title="t('accounts.upstream.readyAccounts')">
          <Users aria-hidden="true" :stroke-width="1.75" class="size-2.75 text-z-text-tertiary" />
          <span class="sr-only">{{ t('accounts.upstream.readyAccounts') }}</span>
          <strong class="font-heavy" :class="pool.states.ready > 0 ? 'text-z-success-text' : 'text-z-text'">{{ pool.states.ready }}</strong>
          <span class="text-z-text-quaternary">/ {{ pool.accountCount }}</span>
        </span>
        <span v-if="upstream?.maxConcurrency != null" class="inline-flex items-center gap-1" :title="t('accounts.upstream.keyConcurrency')">
          <Network aria-hidden="true" :stroke-width="1.75" class="size-2.25 text-z-text-tertiary" />
          <span class="sr-only">{{ t('accounts.upstream.keyConcurrency') }}</span>{{ upstream.maxConcurrency || '∞' }}
        </span>
        <span v-if="upstream?.requestsPerMinute != null" class="inline-flex items-center gap-1" :title="t('accounts.upstream.keyRpm')">
          <Gauge aria-hidden="true" :stroke-width="1.75" class="size-2.75 text-z-text-tertiary" />
          <span class="sr-only">{{ t('accounts.upstream.keyRpm') }}</span>{{ upstream.requestsPerMinute || '∞' }}
        </span>
      </div>
      <div v-if="pool?.plans.length" class="ml-auto flex min-w-0 flex-wrap items-end justify-end gap-1.5">
        <AccountPlanBadge
          v-for="plan in plans.visible" :key="plan.name"
          :plan-type="plan.name" :plan-type-display="plan.name" :count="plan.count"
          size="xs" class="max-w-24"
        />
        <ZPopover v-if="plans.overflow.length" class="shrink-0" trigger="hover-click" placement="top-end" :offset="12" :hover-delay="240">
          <template #reference="{ open, id }">
            <button
              type="button"
              class="inline-flex h-4.5 min-w-6 cursor-pointer items-center justify-center rounded-full border-0 bg-z-fill-quaternary px-1.5 text-[10px] font-emphasis tabular-nums text-z-text-secondary outline-none transition-colors hover:bg-z-fill-tertiary hover:text-z-text focus-visible:ring-2 focus-visible:ring-z-control-outline"
              :aria-label="t('accounts.upstream.morePlans', { count: plans.overflow.length })"
              aria-haspopup="dialog" :aria-expanded="open" :aria-controls="open ? id : undefined"
            >
              +{{ plans.overflow.length }}
            </button>
          </template>
          <section class="w-max max-w-[min(20rem,calc(100vw-2rem))] overflow-hidden rounded-z-lg" role="dialog" :aria-label="t('accounts.upstream.otherPlans')">
            <ZScrollbar max-height="min(14rem, calc(100dvh - 4rem))">
              <ul class="m-0 flex list-none flex-wrap items-center gap-2 p-3">
                <li v-for="plan in plans.overflow" :key="plan.name" class="flex min-w-0">
                  <AccountPlanBadge :plan-type="plan.name" :plan-type-display="plan.name" :count="plan.count" size="xs" />
                </li>
              </ul>
            </ZScrollbar>
          </section>
        </ZPopover>
      </div>
    </div>
  </section>
</template>
