<script setup lang="ts">
import { Gauge, Zap } from '@lucide/vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import { keyUsageTokenMetrics } from '../utils/metrics'

const metrics = [
  ...keyUsageTokenMetrics,
  { label: '缓存命中率', icon: Gauge, tone: 'text-cp-success-text' },
]
</script>

<template>
  <span role="status" class="sr-only">正在加载用量</span>
  <section aria-label="用量汇总加载中" aria-busy="true" class="grid min-w-0 gap-4 xl:grid-cols-[minmax(340px,0.95fr)_minmax(0,3fr)]">
    <BaseCard padding="compact" class="flex min-h-28 min-w-0 flex-col justify-between gap-4" aria-hidden="true">
      <div class="flex min-w-0 flex-1 items-center gap-3">
        <span class="inline-flex size-8.5 shrink-0 items-center justify-center rounded-cp-lg bg-cp-info-container text-cp-info-on-container"><Zap :size="18" /></span>
        <BaseSkeleton class="h-8 w-44 max-w-full" />
        <BaseSkeleton shape="text" class="w-12 shrink-0" />
      </div>
      <div class="flex min-h-5 items-center justify-between gap-6">
        <BaseSkeleton shape="text" class="w-20" />
        <BaseSkeleton shape="text" class="w-30" />
      </div>
    </BaseCard>
    <BaseCard padding="compact" class="flex min-h-28 min-w-0 items-center" aria-hidden="true">
      <div class="grid w-full min-w-0 grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3 2xl:grid-cols-6">
        <div v-for="item in metrics" :key="item.label" class="min-w-0">
          <div class="flex min-w-0 items-center gap-2 text-cp-lg leading-[1.15] font-emphasis text-cp-text-secondary">
            <component :is="item.icon" class="size-4.5 shrink-0" :class="item.tone" />{{ item.label }}
          </div>
          <BaseSkeleton class="mt-5 h-6 w-20 max-w-full" />
        </div>
      </div>
    </BaseCard>
  </section>

  <div class="grid min-w-0 gap-5 xl:grid-cols-[minmax(0,1.4fr)_minmax(400px,1fr)]">
    <BaseCard title="使用趋势" description="用量随时间的变化" aria-busy="true">
      <div class="flex h-71.25 flex-col" aria-hidden="true">
        <div class="flex h-12 shrink-0 flex-wrap content-start justify-end gap-3">
          <BaseSkeleton v-for="item in 6" :key="item" shape="text" class="h-2 w-10" />
        </div>
        <div class="flex min-h-0 flex-1 flex-col justify-between">
          <div v-for="line in 5" :key="line" class="flex items-center gap-3">
            <BaseSkeleton shape="text" class="h-2 w-6 shrink-0" />
            <span class="h-px flex-1 bg-cp-split opacity-50" />
          </div>
        </div>
        <div class="mt-3 flex justify-between gap-3 pl-9">
          <BaseSkeleton v-for="tick in 6" :key="tick" shape="text" class="h-2 w-8" />
        </div>
      </div>
    </BaseCard>
    <BaseCard title="额度概览" aria-busy="true">
      <div class="flex flex-1 flex-col justify-between gap-6" aria-hidden="true">
        <div class="grid flex-1 gap-6 sm:grid-cols-2">
          <div v-for="label in ['今日额度', '七日额度']" :key="label" class="flex min-w-0 flex-col justify-between gap-4">
            <div>
              <div class="text-cp-sm text-cp-text-secondary">
                {{ label }}
              </div>
              <BaseSkeleton class="mt-3 h-8 w-32 max-w-full" />
            </div>
            <div>
              <BaseSkeleton class="h-5 w-full" />
              <div class="mt-2 flex justify-between gap-2">
                <BaseSkeleton shape="text" class="h-2.5 w-16" />
                <BaseSkeleton shape="text" class="h-2.5 w-8" />
              </div>
            </div>
            <div class="grid gap-2">
              <BaseSkeleton shape="text" class="h-2.5 w-16" />
              <BaseSkeleton shape="text" class="h-2.5 w-36 max-w-full" />
            </div>
          </div>
        </div>
        <div class="grid grid-cols-2 gap-3">
          <BaseSkeleton v-for="limit in 2" :key="limit" class="h-10" />
        </div>
      </div>
    </BaseCard>
  </div>

  <BaseCard title="请求健康时间线" description="有效请求可用性" aria-busy="true">
    <template #actions>
      <BaseSkeleton shape="text" class="h-3.5 w-56 max-w-full" aria-hidden="true" />
    </template>
    <div class="grid grid-cols-48 items-center gap-x-0.5 gap-y-1 motion-safe:animate-cp-skeleton motion-reduce:animate-none sm:grid-cols-96" aria-hidden="true">
      <span v-for="point in 96" :key="point" class="flex h-5 items-center"><span class="h-3.5 w-full rounded-xs bg-cp-fill-secondary" /></span>
    </div>
  </BaseCard>
</template>
