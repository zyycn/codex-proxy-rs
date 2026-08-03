<script setup lang="ts">
import type { getAccounts } from '@/api'
import { AlertTriangle, Gauge, ShieldCheck, Users } from '@lucide/vue'

import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'

const props = defineProps<{
  summary: Awaited<ReturnType<typeof getAccounts>>['summary']
}>()

const overviewItems = computed(() => [
  {
    label: '总账号',
    value: formatCount(props.summary.total),
    caption: '账号池规模',
    tone: 'info',
    icon: Users,
  },
  {
    label: '正常账号',
    value: formatCount(props.summary.normal),
    caption: '可参与调度',
    tone: 'success',
    icon: ShieldCheck,
  },
  {
    label: '额度受限',
    value: formatCount((props.summary.quotaExhausted ?? 0) + (props.summary.rateLimited ?? 0)),
    caption: '等待额度恢复',
    tone: 'warning',
    icon: Gauge,
  },
  {
    label: '待处理',
    value: formatCount((props.summary.disabled ?? 0) + (props.summary.error ?? 0)),
    caption: '已停用 / 错误',
    tone: 'danger',
    icon: AlertTriangle,
  },
])

function formatCount(value: number) {
  return value.toLocaleString('zh-CN')
}

function overviewIconClass(tone: string) {
  if (tone === 'success') {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }
  if (tone === 'warning') {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }
  if (tone === 'danger') {
    return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
  }
  return 'bg-(--cp-info-bg) text-(--cp-info-text)'
}
</script>

<template>
  <div class="mt-5 grid shrink-0 grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
    <BaseCard
      v-for="item in overviewItems"
      :key="item.label"
      as="article"
      padding="compact"
    >
      <div class="flex items-stretch justify-between gap-3">
        <div class="flex min-w-0 flex-col">
          <p class="m-0 text-[12px] leading-none font-heavy text-(--cp-text-secondary)">
            {{ item.label }}
          </p>
          <strong
            class="my-2 block font-mono text-[26px] leading-none font-extrabold text-(--cp-text-primary)"
          >
            {{ item.value }}
          </strong>
          <p class="m-0 truncate text-[12px] leading-none font-emphasis text-(--cp-text-muted)">
            {{ item.caption }}
          </p>
        </div>
        <BaseMotionIcon
          aria-hidden="true"
          class="inline-flex size-9 shrink-0 items-center justify-center self-start rounded-lg"
          :class="overviewIconClass(item.tone)"
        >
          <component :is="item.icon" class="size-4.5" />
        </BaseMotionIcon>
      </div>
    </BaseCard>
  </div>
</template>
