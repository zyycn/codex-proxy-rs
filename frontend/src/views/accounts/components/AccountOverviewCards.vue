<script setup lang="ts">
import { computed } from 'vue'
import { AlertTriangle, Gauge, ShieldCheck, Users } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'

const props = defineProps<{
  summary: any
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
    value: formatCount(props.summary.active),
    caption: '可参与调度',
    tone: 'success',
    icon: ShieldCheck,
  },
  {
    label: '配额耗尽',
    value: formatCount(props.summary.quotaExhausted),
    caption: '等待配额恢复',
    tone: 'warning',
    icon: Gauge,
  },
  {
    label: '需处理',
    value: formatCount(props.summary.attention),
    caption: '过期 / 禁用 / 封禁',
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
      :padded="false"
      class="h-24"
    >
      <div class="flex h-full items-stretch justify-between gap-3 px-5 py-3">
        <div class="flex min-w-0 flex-col justify-between">
          <p class="m-0 text-[12px] leading-none font-[760] text-(--cp-text-secondary)">
            {{ item.label }}
          </p>
          <strong
            class="block font-mono text-[26px] leading-none font-[820] text-(--cp-text-primary)"
          >
            {{ item.value }}
          </strong>
          <p class="m-0 truncate text-[12px] leading-none font-[650] text-(--cp-text-muted)">
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
