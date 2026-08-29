<script setup lang="ts">
import type { ProfileActivityLevel, ProfileActivityMode } from '../../utils/accountProfileStatistics'
import type { AccountProfileDailyUsage } from '@/api'
import { computed, shallowRef } from 'vue'

import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { buildProfileActivityGrid, profileActivityCellLabel } from '../../utils/accountProfileStatistics'

const props = defineProps<{
  dailyUsage: AccountProfileDailyUsage[] | null
}>()

const mode = shallowRef<ProfileActivityMode>('daily')
const modeOptions = [
  { label: '每日', value: 'daily' },
  { label: '每周', value: 'weekly' },
  { label: '累计', value: 'cumulative' },
]
const levelClasses: Record<ProfileActivityLevel, string> = {
  0: 'bg-cp-activity-level-0',
  1: 'bg-cp-activity-level-1',
  2: 'bg-cp-activity-level-2',
  3: 'bg-cp-activity-level-3',
  4: 'bg-cp-activity-level-4',
}
const grid = computed(() => buildProfileActivityGrid(props.dailyUsage ?? [], mode.value))
const rangeLabel = computed(() => `${grid.value.rangeStart} 至 ${grid.value.rangeEnd}`)
</script>

<template>
  <section aria-labelledby="profile-token-activity-title">
    <div class="mb-4 flex flex-wrap items-end justify-between gap-3">
      <div>
        <h3 id="profile-token-activity-title" class="m-0 text-cp-lg font-heavy text-cp-text">
          Token 活动
        </h3>
      </div>
      <BaseSegmented v-model="mode" label="Token 活动统计方式" size="sm" :options="modeOptions" />
    </div>

    <BaseEmpty
      v-if="dailyUsage === null"
      title="暂无 Token 活动"
      description="官方个人资料没有返回每日 Token 活动。"
      size="sm"
      surface="none"
    />

    <div v-else class="overflow-x-auto pb-1" role="img" :aria-label="`Token 活动热力图，${rangeLabel}`">
      <div class="min-w-190">
        <div class="mb-1.5 grid gap-1" style="grid-template-columns: repeat(52, minmax(0, 1fr))">
          <span
            v-for="week in grid.weeks"
            :key="week.key"
            class="h-4 whitespace-nowrap text-[10px] leading-4 font-semibold text-cp-text-quaternary"
          >
            {{ week.monthLabel }}
          </span>
        </div>
        <div class="grid gap-1" style="grid-template-columns: repeat(52, minmax(0, 1fr))">
          <div v-for="week in grid.weeks" :key="week.key" class="grid grid-rows-7 gap-1">
            <span
              v-for="cell in week.cells"
              :key="cell.date"
              class="aspect-square rounded-[3px] transition-colors motion-reduce:transition-none"
              :class="cell.isFuture ? 'invisible' : levelClasses[cell.level]"
              :title="profileActivityCellLabel(cell, mode)"
            />
          </div>
        </div>
      </div>
    </div>
  </section>
</template>
