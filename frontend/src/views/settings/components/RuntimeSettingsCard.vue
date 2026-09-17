<script setup lang="ts">
import { Gauge, Timer } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const requestIntervalMs = defineModel<string>('requestIntervalMs', { required: true })
</script>

<template>
  <BaseCard title="并发与请求间隔">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="单账号默认最大并发"
        description="账号未单独设置时使用的并发上限"
      >
        <BaseInput
          v-model="maxConcurrentPerAccount"
          aria-label="单账号默认最大并发"
          type="number"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="请求间隔"
        description="控制同一账号两次调度之间的最小等待时间"
      >
        <BaseInput
          v-model="requestIntervalMs"
          aria-label="请求间隔 ms"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            ms
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
