<script setup lang="ts">
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import type { SemanticTone, ServiceStatusItem } from '../types'

defineProps<{
  items: ServiceStatusItem[]
}>()

const iconToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const valueToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-95 w-full px-7 pt-6">
    <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">指纹信息</h2>

    <div class="mt-5.75 grid gap-2">
      <BaseEmpty
        v-if="items.length === 0"
        compact
        title="暂无指纹信息"
        description="诊断接口返回后会显示客户端指纹信息。"
        class="h-72 place-content-center"
      />
      <template v-else>
        <div
          v-for="item in items"
          :key="item.label"
          class="grid h-12.5 w-full grid-cols-[24px_16px_1fr_1fr_1fr] items-center rounded-[14px] bg-(--cp-bg-subtle) px-3.5"
        >
          <span
            class="inline-flex size-6 items-center justify-center rounded-lg"
            :class="iconToneClasses[item.tone]"
          >
            <component :is="item.icon" :size="14" />
          </span>
          <template v-if="item.label === 'User Agent' || item.label === '更新时间'">
            <strong
              class="col-start-3 text-[13px] leading-[1.15] font-[650] text-(--cp-text-primary)"
              >{{ item.label }}</strong
            >
            <span
              class="col-start-4 col-span-2 text-[13px] leading-[1.15] font-mono font-semibold text-(--cp-text-secondary) truncate"
              >{{ item.value }}</span
            >
          </template>
          <template v-else>
            <strong
              class="col-start-3 text-[13px] leading-[1.15] font-[650] text-(--cp-text-primary)"
              >{{ item.label }}</strong
            >
            <span
              class="col-start-4 text-[13px] leading-[1.15] font-bold"
              :class="valueToneClasses[item.tone]"
              >{{ item.value }}</span
            >
            <span
              class="col-start-5 text-right font-mono text-xs leading-[1.15] font-semibold text-(--cp-text-secondary)"
              >{{ item.detail }}</span
            >
          </template>
        </div>
      </template>
    </div>
  </BaseCard>
</template>
