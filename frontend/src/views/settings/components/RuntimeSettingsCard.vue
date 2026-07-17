<script setup lang="ts">
import { Gauge, Timer, Zap } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const refreshMarginSeconds = defineModel<string>('refreshMarginSeconds', { required: true })
const refreshConcurrency = defineModel<string>('refreshConcurrency', { required: true })
const requestIntervalMs = defineModel<string>('requestIntervalMs', { required: true })
</script>

<template>
  <BaseCard
    :padded="false"
    title="运行参数"
    description="请求节奏、账号并发和 Token 刷新"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <BaseForm :columns="2" class="max-w-6xl">
      <BaseFormItem
        label="单账号最大并发"
        description="限制单个账号同一时间可承载的请求数"
      >
        <BaseInput
          v-model="maxConcurrentPerAccount"
          aria-label="单账号最大并发"
          type="number"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="提前刷新秒数"
        description="Token 过期前多少秒触发刷新"
      >
        <BaseInput
          v-model="refreshMarginSeconds"
          aria-label="提前刷新秒数"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="刷新并发数"
        description="同时刷新 Token 的最大请求数，减小可避免限流"
      >
        <BaseInput
          v-model="refreshConcurrency"
          aria-label="刷新并发数"
          type="number"
        >
          <template #prefix>
            <Zap class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="请求间隔 ms"
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
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
