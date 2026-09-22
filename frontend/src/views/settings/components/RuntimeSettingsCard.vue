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
  <BaseCard title="并发控制">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="默认账号并发上限"
        description="账号未单独设置时使用的并发上限，0 表示不限制"
      >
        <BaseInput
          v-model="maxConcurrentPerAccount"
          aria-label="默认账号并发上限"
          type="number"
          min="0"
          max="4294967295"
          step="1"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="最小请求间隔"
        description="控制同一账号两次调度之间的最小等待时间"
      >
        <BaseInput
          v-model="requestIntervalMs"
          aria-label="最小请求间隔（毫秒）"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            毫秒
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
