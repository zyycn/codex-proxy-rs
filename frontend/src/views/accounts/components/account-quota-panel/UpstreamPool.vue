<script setup lang="ts">
import type { AccountQuotaWindow, AccountUpstream } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { poolNodes } from './pool'
import PoolChart from './PoolChart.vue'

const props = defineProps<{ pool: NonNullable<AccountUpstream['pool']>, windows: AccountQuotaWindow[] }>()
const { t } = useI18n({ useScope: 'global' })
const nodes = computed(() => poolNodes(props.windows, props.pool.accountCount))
</script>

<template>
  <section class="grid min-w-0 gap-1" :aria-label="t('accounts.upstream.pool')">
    <PoolChart v-if="nodes.length" :nodes="nodes" />
    <div class="flex min-h-5 min-w-0 items-center justify-between gap-x-3">
      <div class="flex shrink-0 items-baseline gap-1 whitespace-nowrap" :aria-label="`${pool.states.ready} / ${pool.accountCount} ${t('accounts.upstream.ready')}`">
        <strong class="font-mono text-cp-xs leading-[normal] font-heavy tabular-nums" :class="pool.states.ready > 0 ? 'text-cp-success-text' : 'text-cp-text'">{{ pool.states.ready }}</strong>
        <span class="font-mono text-cp-xs leading-[normal] tabular-nums text-cp-text-quaternary">/ {{ pool.accountCount }}</span>
      </div>
      <div v-if="pool.plans.length" class="flex min-w-0 flex-wrap items-center justify-end gap-x-3 gap-y-1.5">
        <span v-for="plan in pool.plans" :key="plan.name" class="inline-flex min-w-0 items-center gap-1.5">
          <AccountPlanBadge :plan-type="plan.name" :plan-type-display="plan.name" size="xs" />
          <span class="font-mono text-cp-xs leading-[normal] font-emphasis tabular-nums text-cp-text-secondary">{{ plan.count }}</span>
        </span>
      </div>
    </div>
  </section>
</template>
